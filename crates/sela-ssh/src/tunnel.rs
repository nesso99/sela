//! Local port-forward tunnel over SSH (`direct-tcpip`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::ssh_key::PublicKey;
use russh::Disconnect;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::auth::{authenticate, AuthMethod, SshAuthSecrets};
use crate::config::{resolve_endpoint, ResolvedSshTarget, SshEndpointInput};
use crate::error::SshError;
use crate::host_key::{fingerprint_sha256, learn_host_key, HostKeyCheck, HostKeyGate};
use crate::paths::SshPaths;
use sela_core::{SshAuthMethod, SshSettings};

const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

/// Parameters to open a tunnel to a database reachable from the bastion.
#[derive(Debug, Clone)]
pub struct TunnelRequest {
    pub ssh: SshSettings,
    /// Database host as seen from the bastion (often a Docker service name).
    pub remote_db_host: String,
    pub remote_db_port: u16,
    pub secrets: SshAuthSecrets,
    pub paths: SshPaths,
}

/// Result of [`open_tunnel`]: connected, or a first-seen host key to confirm.
#[derive(Debug)]
pub enum TunnelOpen {
    Ready(SshTunnel),
    /// First-seen host key. Call [`UnknownHostKey::accept_and_remember`] or
    /// [`UnknownHostKey::accept_once`] after the user confirms the fingerprint.
    UnknownHostKey(Box<UnknownHostKey>),
}

/// Pending tunnel after an unknown host key was presented (SSH-18).
pub struct UnknownHostKey {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    key: PublicKey,
    request: TunnelRequest,
    target: ResolvedSshTarget,
    auth: AuthMethod,
}

impl std::fmt::Debug for UnknownHostKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnknownHostKey")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl UnknownHostKey {
    /// Trust the key, append it to known_hosts, then open the tunnel.
    pub async fn accept_and_remember(self) -> Result<SshTunnel, SshError> {
        learn_host_key(
            &self.host,
            self.port,
            &self.key,
            &self.request.paths.known_hosts,
        )?;
        open_tunnel_resolved(self.request, self.target, self.auth, None).await
    }

    /// Accept for this connection only; do not write known_hosts.
    pub async fn accept_once(self) -> Result<SshTunnel, SshError> {
        open_tunnel_resolved(self.request, self.target, self.auth, Some(self.key)).await
    }

    /// Refuse the connection (default when the UI cancels).
    pub fn reject(self) -> SshError {
        SshError::UnknownHostKey {
            host: self.host,
            port: self.port,
            fingerprint: self.fingerprint,
        }
    }
}

/// A live local port forward. Dropping (or [`SshTunnel::close`]) stops the
/// listener and disconnects the SSH session (SSH-10).
pub struct SshTunnel {
    local_addr: SocketAddr,
    shutdown: CancellationToken,
    forward_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SshTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTunnel")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl SshTunnel {
    /// Local address the database driver must dial (`127.0.0.1:<ephemeral>`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn local_port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Stop accepting and disconnect the SSH session.
    pub async fn close(mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.forward_task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        self.shutdown.cancel();
        // The forward task owns the Handle and disconnects on cancel.
    }
}

/// Open a local port forward. May return [`TunnelOpen::UnknownHostKey`].
pub async fn open_tunnel(request: TunnelRequest) -> Result<TunnelOpen, SshError> {
    if !request.ssh.enabled {
        return Err(SshError::config(
            "SSH tunnel is not enabled on this connection",
        ));
    }
    let input = SshEndpointInput {
        host: request.ssh.host.clone(),
        port: request.ssh.port,
        user: request.ssh.user.clone(),
        identity_file: request.ssh.identity_file.clone(),
        use_ssh_config: request.ssh.use_ssh_config,
    };
    let target = resolve_endpoint(&input, &request.paths)?;
    let auth = auth_method_for(&request.ssh, &target)?;
    match try_open(request.clone(), target.clone(), auth.clone(), None).await? {
        OpenAttempt::Ready(tunnel) => Ok(TunnelOpen::Ready(tunnel)),
        OpenAttempt::Unknown {
            host,
            port,
            key,
            fingerprint,
        } => Ok(TunnelOpen::UnknownHostKey(Box::new(UnknownHostKey {
            host,
            port,
            fingerprint,
            key,
            request,
            target,
            auth,
        }))),
    }
}

async fn open_tunnel_resolved(
    request: TunnelRequest,
    target: ResolvedSshTarget,
    auth: AuthMethod,
    accept_once: Option<PublicKey>,
) -> Result<SshTunnel, SshError> {
    match try_open(request, target, auth, accept_once).await? {
        OpenAttempt::Ready(tunnel) => Ok(tunnel),
        OpenAttempt::Unknown {
            host,
            port,
            fingerprint,
            ..
        } => Err(SshError::UnknownHostKey {
            host,
            port,
            fingerprint,
        }),
    }
}

enum OpenAttempt {
    Ready(SshTunnel),
    Unknown {
        host: String,
        port: u16,
        key: PublicKey,
        fingerprint: String,
    },
}

fn auth_method_for(ssh: &SshSettings, target: &ResolvedSshTarget) -> Result<AuthMethod, SshError> {
    match ssh.auth {
        SshAuthMethod::Password => Ok(AuthMethod::Password),
        SshAuthMethod::PrivateKey => {
            let path = target.identity_file.clone().ok_or_else(|| {
                SshError::config(
                    "SSH identity file is required for private-key auth (set IdentityFile or the connection field)",
                )
            })?;
            Ok(AuthMethod::PrivateKey { path })
        }
        SshAuthMethod::Agent => Ok(AuthMethod::Agent),
    }
}

async fn try_open(
    request: TunnelRequest,
    target: ResolvedSshTarget,
    auth: AuthMethod,
    accept_once: Option<PublicKey>,
) -> Result<OpenAttempt, SshError> {
    if request.remote_db_host.trim().is_empty() {
        return Err(SshError::config("remote database host is empty"));
    }
    if request.remote_db_port == 0 {
        return Err(SshError::config("remote database port is invalid"));
    }

    let gate = match accept_once {
        Some(key) => HostKeyGate::with_accept_once(key),
        None => HostKeyGate::new(),
    };
    let handler = ClientHandler {
        host: target.host.clone(),
        port: target.port,
        known_hosts: request.paths.known_hosts.clone(),
        gate: Arc::clone(&gate),
    };

    let config = Arc::new(client::Config {
        nodelay: true,
        ..Default::default()
    });

    let connect = client::connect(config, (target.host.as_str(), target.port), handler);
    let mut session = match tokio::time::timeout(SSH_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(session)) => session,
        Ok(Err(err)) => {
            return map_connect_failure(err, &gate, &target);
        }
        Err(_) => {
            return Err(SshError::connection(format!(
                "timed out after {}s connecting to the SSH bastion",
                SSH_CONNECT_TIMEOUT.as_secs()
            )));
        }
    };

    // Prefer gated host-key outcome if the handler recorded one.
    if let Some(check) = gate.take_last() {
        match check {
            HostKeyCheck::Mismatch { line } => {
                let _ = session
                    .disconnect(Disconnect::ByApplication, "", "en")
                    .await;
                return Err(SshError::HostKeyMismatch {
                    host: target.host,
                    port: target.port,
                    line,
                });
            }
            HostKeyCheck::Unknown { fingerprint, key } => {
                let _ = session
                    .disconnect(Disconnect::ByApplication, "", "en")
                    .await;
                return Ok(OpenAttempt::Unknown {
                    host: target.host,
                    port: target.port,
                    key,
                    fingerprint,
                });
            }
            HostKeyCheck::Match => {}
        }
    }

    authenticate(&mut session, &target.user, &auth, &request.secrets).await?;

    // SSH-12: bind 127.0.0.1:0 so the OS picks a free ephemeral port.
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(|err| SshError::io(format!("could not bind local SSH tunnel listener: {err}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|err| SshError::io(format!("could not read local tunnel address: {err}")))?;
    if !local_addr.ip().is_loopback() {
        return Err(SshError::io(
            "SSH tunnel listener is not loopback; refusing to expose the forward",
        ));
    }

    let shutdown = CancellationToken::new();
    let shutdown_fwd = shutdown.clone();
    let remote_host = request.remote_db_host.clone();
    let remote_port = request.remote_db_port;

    // Handle is not Clone; the forward task owns it and opens channels serially
    // via an mpsc of accepted sockets.
    let forward_task = tokio::spawn(async move {
        forward_loop(listener, session, remote_host, remote_port, shutdown_fwd).await;
    });

    Ok(OpenAttempt::Ready(SshTunnel {
        local_addr,
        shutdown,
        forward_task: Some(forward_task),
    }))
}

fn map_connect_failure(
    err: SshError,
    gate: &HostKeyGate,
    target: &ResolvedSshTarget,
) -> Result<OpenAttempt, SshError> {
    if let Some(check) = gate.take_last() {
        match check {
            HostKeyCheck::Mismatch { line } => {
                return Err(SshError::HostKeyMismatch {
                    host: target.host.clone(),
                    port: target.port,
                    line,
                });
            }
            HostKeyCheck::Unknown { fingerprint, key } => {
                return Ok(OpenAttempt::Unknown {
                    host: target.host.clone(),
                    port: target.port,
                    key,
                    fingerprint,
                });
            }
            HostKeyCheck::Match => {}
        }
    }
    // Never include credentials; russh messages are host/transport only.
    Err(SshError::connection(format!(
        "could not connect to SSH bastion: {err}"
    )))
}

async fn forward_loop(
    listener: TcpListener,
    session: Handle<ClientHandler>,
    remote_host: String,
    remote_port: u16,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((socket, origin)) = accepted else { break; };
                // Open the channel on the session (not Clone); relay in a child
                // task so the accept loop stays responsive.
                let opened = session
                    .channel_open_direct_tcpip(
                        remote_host.clone(),
                        u32::from(remote_port),
                        origin.ip().to_string(),
                        u32::from(origin.port()),
                    )
                    .await;
                match opened {
                    Ok(channel) => {
                        tokio::spawn(async move {
                            let _ = relay_channel(channel, socket).await;
                        });
                    }
                    Err(_) => {
                        // Tunnel dropped mid-flight (SSH-11); stop accepting.
                        break;
                    }
                }
            }
        }
    }
    let _ = session
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;
}

async fn relay_channel(
    channel: russh::Channel<client::Msg>,
    mut socket: TcpStream,
) -> Result<(), SshError> {
    let mut stream = channel.into_stream();
    let (mut ssh_read, mut ssh_write) = tokio::io::split(&mut stream);
    let (mut sock_read, mut sock_write) = socket.split();

    let client_to_ssh = async {
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = sock_read.read(&mut buf).await?;
            if n == 0 {
                let _ = ssh_write.shutdown().await;
                break;
            }
            ssh_write.write_all(&buf[..n]).await?;
        }
        Ok::<(), std::io::Error>(())
    };
    let ssh_to_client = async {
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = ssh_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            sock_write.write_all(&buf[..n]).await?;
        }
        Ok::<(), std::io::Error>(())
    };

    tokio::select! {
        r = client_to_ssh => { let _ = r; }
        r = ssh_to_client => { let _ = r; }
    }
    Ok(())
}

/// russh client handler that enforces known_hosts via [`HostKeyGate`].
pub(crate) struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    gate: Arc<HostKeyGate>,
}

impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // Fingerprint is computed inside decide for Unknown; never log the key.
        let _ = fingerprint_sha256(server_public_key);
        self.gate
            .decide(&self.host, self.port, server_public_key, &self.known_hosts)
    }
}

impl From<russh::Error> for SshError {
    fn from(err: russh::Error) -> Self {
        SshError::connection(format!("SSH protocol error: {err}"))
    }
}
