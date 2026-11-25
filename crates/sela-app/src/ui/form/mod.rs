//! Create and edit a saved connection. The password never enters the JSON file.

pub(crate) mod ssh;

use std::fmt;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputContentType, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, Context, Entity, EventEmitter, IntoElement, MouseButton, Render, Subscription, Window,
};

use sela_core::{
    parse_connection_url, safe_mode_driven_by_color, ConnectionColor, Engine, SafeMode,
    SavedConnection, SshAuthMethod, SshSettings, TlsMode, TlsSettings,
};
use sela_ssh::SshPaths;

use super::theme::SelaPalette;
use ssh::TestResult;

/// Inputs for [`ConnectionForm::with_secrets`] (keeps the constructor arity low).
pub struct FormSecrets {
    pub existing: Option<SavedConnection>,
    pub password: String,
    pub ssh_password: String,
    pub ssh_passphrase: String,
    pub warning: Option<String>,
    pub keychain_unavailable: bool,
}

#[derive(Clone)]
pub struct ConnectionDraft {
    pub connection: SavedConnection,
    pub password: String,
    /// SSH bastion password (keychain `{id}:ssh-password`). Never logged.
    pub ssh_password: String,
    /// Private-key passphrase (keychain `{id}:ssh-passphrase`). Never logged.
    pub ssh_passphrase: String,
}

impl fmt::Debug for ConnectionDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionDraft")
            .field("connection", &self.connection)
            .field(
                "password",
                &if self.password.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .field(
                "ssh_password",
                &if self.ssh_password.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .field(
                "ssh_passphrase",
                &if self.ssh_passphrase.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .finish()
    }
}

pub enum FormEvent {
    /// Save & connect (primary).
    Submit(Box<ConnectionDraft>),
    /// Save without dropping the live session or reconnecting.
    SaveOnly(Box<ConnectionDraft>),
    /// Test with the form values; does not write JSON or the keychain.
    Test(Box<ConnectionDraft>),
    Cancel,
}

pub struct ConnectionForm {
    existing_id: Option<String>,
    engine: Engine,
    tls_mode: TlsMode,
    color: ConnectionColor,
    /// True after the user edits Safe mode; until then color drives the default on new connections.
    safe_mode_control_touched: bool,
    record_history: bool,
    safe_mode: SafeMode,
    warning: Option<String>,
    /// True when the OS keychain probe failed; changes the password helper copy.
    keychain_unavailable: bool,
    error: Option<String>,
    import_note: Option<String>,
    import_url: Entity<InputState>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    database: Entity<InputState>,
    file_path: Entity<InputState>,
    password: Entity<InputState>,
    conn_label: Entity<InputState>,
    group: Entity<InputState>,
    ca_path: Entity<InputState>,
    client_cert_path: Entity<InputState>,
    client_key_path: Entity<InputState>,
    // SSH (Postgres / MySQL only; absent for SQLite).
    ssh_enabled: bool,
    ssh_auth: SshAuthMethod,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_password: Entity<InputState>,
    ssh_private_key_path: Entity<InputState>,
    ssh_passphrase: Entity<InputState>,
    /// Injectable paths for UI tests (never the real ~/.ssh in tests).
    ssh_paths_override: Option<SshPaths>,
    pending_private_key_path: Option<String>,
    test_result: TestResult,
    /// Generation bumped when Test starts; stale completions are ignored.
    test_generation: u64,
    _import_url_subscription: Option<Subscription>,
    /// Clears the Test line when any SSH/DB field changes (SSH-16).
    _field_change_subscriptions: Vec<Subscription>,
}

impl EventEmitter<FormEvent> for ConnectionForm {}

impl ConnectionForm {
    /// Open a new/edit form. SSH secrets default empty; prefer
    /// [`Self::with_secrets`] when loading from the keychain.
    #[cfg(test)]
    pub fn new(
        existing: Option<SavedConnection>,
        password: String,
        warning: Option<String>,
        keychain_unavailable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_secrets(
            FormSecrets {
                existing,
                password,
                ssh_password: String::new(),
                ssh_passphrase: String::new(),
                warning,
                keychain_unavailable,
            },
            window,
            cx,
        )
    }

    /// Like [`Self::new`], with SSH password / passphrase from the keychain.
    pub fn with_secrets(secrets: FormSecrets, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let FormSecrets {
            existing,
            password,
            ssh_password,
            ssh_passphrase,
            warning,
            keychain_unavailable,
        } = secrets;
        let engine = existing
            .as_ref()
            .map(|item| item.engine)
            .unwrap_or(Engine::Postgres);
        let seed = existing
            .clone()
            .unwrap_or_else(|| SavedConnection::new(engine));
        let port = if seed.port == 0 {
            String::new()
        } else {
            seed.port.to_string()
        };
        let ssh_port = if seed.ssh.port == 0 {
            "22".to_string()
        } else {
            seed.ssh.port.to_string()
        };
        Self {
            existing_id: existing.map(|item| item.id),
            engine,
            tls_mode: seed.tls.mode,
            color: seed.color,
            safe_mode_control_touched: false,
            record_history: seed.record_history,
            safe_mode: seed.safe_mode,
            warning,
            keychain_unavailable,
            error: None,
            import_note: None,
            import_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("postgres://user:pass@host:5432/db")
            }),
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Local library")
                    .default_value(seed.name)
            }),
            host: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("127.0.0.1")
                    .default_value(seed.host)
            }),
            port: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("5432")
                    .default_value(port)
            }),
            username: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("sela")
                    .default_value(seed.username)
            }),
            database: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("sample")
                    .default_value(seed.database)
            }),
            file_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("/path/to/database.sqlite")
                    .default_value(seed.file_path)
            }),
            password: cx.new(|cx| {
                // Spec: password is masked by default; mask_toggle only adds the reveal control.
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("Password")
                    .default_value(password)
            }),
            conn_label: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("prod")
                    .default_value(seed.label)
            }),
            group: cx.new(|cx| InputState::new(window, cx).default_value(seed.group)),
            ca_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("/path/to/ca.pem")
                    .default_value(seed.tls.ca_path)
            }),
            client_cert_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("/path/to/client.crt")
                    .default_value(seed.tls.client_cert_path)
            }),
            client_key_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("/path/to/client.key (PEM, unencrypted)")
                    .default_value(seed.tls.client_key_path)
            }),
            ssh_enabled: seed.ssh.enabled,
            ssh_auth: seed.ssh.auth,
            ssh_host: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("bastion.example.com")
                    .default_value(seed.ssh.host)
            }),
            ssh_port: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("22")
                    .default_value(ssh_port)
            }),
            ssh_user: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("ubuntu")
                    .default_value(seed.ssh.user)
            }),
            ssh_password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("SSH password")
                    .default_value(ssh_password)
            }),
            ssh_private_key_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("~/.ssh/id_ed25519")
                    .default_value(seed.ssh.private_key_path)
            }),
            ssh_passphrase: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("Passphrase")
                    .default_value(ssh_passphrase)
            }),
            ssh_paths_override: None,
            pending_private_key_path: None,
            test_result: TestResult::Idle,
            test_generation: 0,
            _import_url_subscription: None,
            _field_change_subscriptions: Vec::new(),
        }
    }

    fn ensure_import_url_subscription(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._import_url_subscription.is_some() {
            return;
        }
        let import_url = self.import_url.clone();
        // subscribe_in already runs inside a ConnectionForm update (`&mut Self`).
        // Calling `this.update` here re-enters the entity and panics
        // ("cannot update ConnectionForm while it is already being updated").
        let subscription =
            cx.subscribe_in(&import_url, window, move |form, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    form.import_from_url(window, cx);
                }
            });
        self._import_url_subscription = Some(subscription);
    }

    /// SSH-16: any change to SSH or DB inputs clears the Test result line.
    fn ensure_field_change_subscriptions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self._field_change_subscriptions.is_empty() {
            return;
        }
        let fields = [
            self.host.clone(),
            self.port.clone(),
            self.username.clone(),
            self.database.clone(),
            self.password.clone(),
            self.file_path.clone(),
            self.ca_path.clone(),
            self.client_cert_path.clone(),
            self.client_key_path.clone(),
            self.ssh_host.clone(),
            self.ssh_port.clone(),
            self.ssh_user.clone(),
            self.ssh_password.clone(),
            self.ssh_private_key_path.clone(),
            self.ssh_passphrase.clone(),
        ];
        for field in fields {
            let subscription = cx.subscribe_in(&field, window, move |form, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    form.clear_test_result();
                    cx.notify();
                }
            });
            self._field_change_subscriptions.push(subscription);
        }
    }

    fn value(input: &Entity<InputState>, cx: &gpui_kit::App) -> String {
        input.read(cx).value().to_string()
    }

    fn set_value(
        input: &Entity<InputState>,
        value: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| {
            state.set_value(value.into(), window, cx);
        });
    }

    fn set_engine(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        if self.engine == engine {
            return;
        }
        let previous_engine = self.engine;
        let previous = previous_engine.default_port().to_string();
        let current = Self::value(&self.port, cx);
        self.engine = engine;
        if engine != Engine::Sqlite && (current.is_empty() || current == "0" || current == previous)
        {
            let next = engine.default_port().to_string();
            self.port.update(cx, |state, cx| {
                state.set_value(next, window, cx);
            });
        }
        if engine == Engine::Sqlite {
            self.tls_mode = TlsMode::Disable;
            // SSH section is absent for SQLite — turn off so a prior toggle does not
            // linger in the draft if the user switches back incorrectly.
            self.ssh_enabled = false;
        } else if previous_engine == Engine::Sqlite {
            // SQLite forced Disable; Prefer is the default for new network connections.
            self.tls_mode = TlsMode::Prefer;
        }
        self.clear_test_result();
        cx.notify();
    }

    fn set_tls_mode(&mut self, mode: TlsMode, cx: &mut Context<Self>) {
        if self.tls_mode == mode {
            return;
        }
        self.tls_mode = mode;
        self.clear_test_result();
        cx.notify();
    }

    fn set_color(&mut self, color: ConnectionColor, cx: &mut Context<Self>) {
        if self.color == color {
            return;
        }
        self.color = color;
        // New connections only: Red → Confirm writes until the user touches Safe mode.
        if self.existing_id.is_none() {
            self.safe_mode =
                safe_mode_driven_by_color(color, self.safe_mode_control_touched, self.safe_mode);
        }
        cx.notify();
    }

    fn set_safe_mode(&mut self, mode: SafeMode, cx: &mut Context<Self>) {
        if self.safe_mode == mode {
            self.safe_mode_control_touched = true;
            return;
        }
        self.safe_mode = mode;
        self.safe_mode_control_touched = true;
        cx.notify();
    }

    fn import_from_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = Self::value(&self.import_url, cx);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        // Snapshot SSH fields so Import URL never fills or clears them (SSH-19).
        let ssh_snapshot = (
            self.ssh_enabled,
            self.ssh_auth,
            Self::value(&self.ssh_host, cx),
            Self::value(&self.ssh_port, cx),
            Self::value(&self.ssh_user, cx),
            Self::value(&self.ssh_password, cx),
            Self::value(&self.ssh_private_key_path, cx),
            Self::value(&self.ssh_passphrase, cx),
        );
        match parse_connection_url(trimmed) {
            Ok(imported) => {
                Self::set_value(&self.import_url, String::new(), window, cx);
                self.set_engine(imported.engine, window, cx);
                Self::set_value(&self.host, imported.host, window, cx);
                Self::set_value(&self.port, imported.port.to_string(), window, cx);
                Self::set_value(&self.username, imported.username, window, cx);
                Self::set_value(&self.database, imported.database, window, cx);
                Self::set_value(&self.file_path, imported.file_path, window, cx);
                if let Some(password) = imported.password {
                    Self::set_value(&self.password, password, window, cx);
                    // Keep the field masked after Import URL (reveal stays available).
                    self.password.update(cx, |state, cx| {
                        state.set_masked(true, window, cx);
                    });
                }
                if imported.engine == Engine::Sqlite {
                    self.tls_mode = TlsMode::Disable;
                } else if let Some(mode) = imported.tls_mode {
                    // Recognised sslmode only — never wipe CA / client cert paths.
                    self.tls_mode = mode;
                }
                // Restore SSH (set_engine may have cleared enabled for SQLite).
                if imported.engine != Engine::Sqlite {
                    self.ssh_enabled = ssh_snapshot.0;
                    self.ssh_auth = ssh_snapshot.1;
                    Self::set_value(&self.ssh_host, ssh_snapshot.2, window, cx);
                    Self::set_value(&self.ssh_port, ssh_snapshot.3, window, cx);
                    Self::set_value(&self.ssh_user, ssh_snapshot.4, window, cx);
                    Self::set_value(&self.ssh_password, ssh_snapshot.5, window, cx);
                    Self::set_value(&self.ssh_private_key_path, ssh_snapshot.6, window, cx);
                    Self::set_value(&self.ssh_passphrase, ssh_snapshot.7, window, cx);
                }
                let mut note =
                    "Filled from the URL. The password is only in the password field.".to_string();
                for extra in imported.notes {
                    if !extra.is_empty() {
                        note.push(' ');
                        note.push_str(&extra);
                    }
                }
                apply_import_status_lines(&mut self.import_note, &mut self.error, Ok(note));
            }
            Err(err) => {
                apply_import_status_lines(
                    &mut self.import_note,
                    &mut self.error,
                    Err(err.message().into()),
                );
            }
        }
        cx.notify();
    }

    /// Build a draft from the form. On validation failure sets `error` and returns None.
    fn build_draft(&mut self, cx: &mut Context<Self>) -> Option<ConnectionDraft> {
        let mut connection = SavedConnection::new(self.engine);
        if let Some(id) = &self.existing_id {
            connection.id = id.clone();
        }
        connection.name = Self::value(&self.name, cx).trim().to_string();
        connection.engine = self.engine;
        connection.host = Self::value(&self.host, cx).trim().to_string();
        connection.username = Self::value(&self.username, cx).trim().to_string();
        connection.database = Self::value(&self.database, cx).trim().to_string();
        connection.file_path = Self::value(&self.file_path, cx).trim().to_string();
        connection.record_history = self.record_history;
        connection.color = self.color;
        connection.label = Self::value(&self.conn_label, cx).trim().to_string();
        connection.group = Self::value(&self.group, cx).trim().to_string();
        connection.safe_mode = self.safe_mode;
        let port_text = Self::value(&self.port, cx).trim().to_string();
        if self.engine == Engine::Sqlite {
            connection.port = 0;
            connection.host.clear();
            connection.username.clear();
            connection.database.clear();
            connection.tls = TlsSettings::default();
            connection.ssh = SshSettings::default();
        } else {
            match port_text.parse::<u16>() {
                Ok(0) | Err(_) => {
                    self.error = Some("Port must be a number from 1 to 65535.".into());
                    cx.notify();
                    return None;
                }
                Ok(port) => connection.port = port,
            }
            connection.tls = TlsSettings {
                mode: self.tls_mode,
                ca_path: Self::value(&self.ca_path, cx).trim().to_string(),
                client_cert_path: Self::value(&self.client_cert_path, cx).trim().to_string(),
                client_key_path: Self::value(&self.client_key_path, cx).trim().to_string(),
            };
            let ssh_port_text = Self::value(&self.ssh_port, cx).trim().to_string();
            let ssh_port = if !self.ssh_enabled {
                22
            } else {
                match ssh_port_text.parse::<u16>() {
                    Ok(0) | Err(_) => {
                        self.error = Some("SSH port must be a number from 1 to 65535.".into());
                        cx.notify();
                        return None;
                    }
                    Ok(port) => port,
                }
            };
            connection.ssh = if self.ssh_enabled {
                SshSettings {
                    enabled: true,
                    host: Self::value(&self.ssh_host, cx).trim().to_string(),
                    port: ssh_port,
                    user: Self::value(&self.ssh_user, cx).trim().to_string(),
                    auth: self.ssh_auth,
                    private_key_path: Self::value(&self.ssh_private_key_path, cx)
                        .trim()
                        .to_string(),
                }
            } else {
                SshSettings::default()
            };
        }
        if let Err(message) = connection.validate() {
            self.error = Some(message);
            cx.notify();
            return None;
        }
        self.error = None;
        Some(ConnectionDraft {
            connection,
            password: Self::value(&self.password, cx),
            ssh_password: Self::value(&self.ssh_password, cx),
            ssh_passphrase: Self::value(&self.ssh_passphrase, cx),
        })
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.build_draft(cx) else {
            return;
        };
        cx.emit(FormEvent::Submit(Box::new(draft)));
    }

    fn save_only(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.build_draft(cx) else {
            return;
        };
        cx.emit(FormEvent::SaveOnly(Box::new(draft)));
    }

    fn request_test(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.build_draft(cx) else {
            return;
        };
        self.test_generation = self.test_generation.wrapping_add(1);
        self.test_result = TestResult::Testing;
        cx.notify();
        cx.emit(FormEvent::Test(Box::new(draft)));
    }

    /// Cancel an in-flight Test (closing the dialog).
    pub fn cancel_test(&mut self, cx: &mut Context<Self>) {
        self.test_generation = self.test_generation.wrapping_add(1);
        self.test_result = TestResult::Idle;
        cx.notify();
    }

    pub fn test_generation(&self) -> u64 {
        self.test_generation
    }

    pub fn is_testing(&self) -> bool {
        matches!(self.test_result, TestResult::Testing)
    }

    /// Injectable SSH paths for headless UI tests (None → system default).
    pub(crate) fn ssh_paths_override_for_test(&self) -> Option<SshPaths> {
        self.ssh_paths_override.clone()
    }

    /// Test helpers: read field values and mask state without logging secrets.
    #[cfg(test)]
    pub(crate) fn test_field(&self, name: &str, cx: &gpui_kit::App) -> String {
        match name {
            "import_url" => Self::value(&self.import_url, cx),
            "name" => Self::value(&self.name, cx),
            "host" => Self::value(&self.host, cx),
            "port" => Self::value(&self.port, cx),
            "username" => Self::value(&self.username, cx),
            "database" => Self::value(&self.database, cx),
            "password" => Self::value(&self.password, cx),
            "ssh_host" => Self::value(&self.ssh_host, cx),
            "ssh_port" => Self::value(&self.ssh_port, cx),
            "ssh_user" => Self::value(&self.ssh_user, cx),
            "ssh_password" => Self::value(&self.ssh_password, cx),
            "ssh_private_key_path" => Self::value(&self.ssh_private_key_path, cx),
            "ssh_passphrase" => Self::value(&self.ssh_passphrase, cx),
            other => panic!("unknown form field: {other}"),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_password_masked(&self, cx: &gpui_kit::App) -> bool {
        self.password.read(cx).presentation().is_masked()
    }

    #[cfg(test)]
    pub(crate) fn test_ssh_passphrase_masked(&self, cx: &gpui_kit::App) -> bool {
        self.ssh_passphrase.read(cx).presentation().is_masked()
    }

    #[cfg(test)]
    pub(crate) fn test_ssh_enabled(&self) -> bool {
        self.ssh_enabled
    }

    #[cfg(test)]
    pub(crate) fn test_ssh_auth(&self) -> SshAuthMethod {
        self.ssh_auth
    }

    #[cfg(test)]
    pub(crate) fn test_import_note(&self) -> Option<&str> {
        self.import_note.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn test_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn test_result_line(&self) -> Option<String> {
        match &self.test_result {
            TestResult::Idle => None,
            TestResult::Testing => Some("Testing…".into()),
            TestResult::Success { ms, via_ssh } => Some(TestResult::success_line(*ms, *via_ssh)),
            TestResult::Failure(msg) => Some(msg.clone()),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_ssh_paths(&mut self, paths: SshPaths) {
        self.ssh_paths_override = Some(paths);
    }

    #[cfg(test)]
    pub(crate) fn test_set_ssh_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.set_ssh_enabled(enabled, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_set_ssh_auth(&mut self, auth: SshAuthMethod, cx: &mut Context<Self>) {
        self.set_ssh_auth(auth, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_set_field(
        &mut self,
        name: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match name {
            "ssh_host" => &self.ssh_host,
            "ssh_port" => &self.ssh_port,
            "ssh_user" => &self.ssh_user,
            "ssh_password" => &self.ssh_password,
            "ssh_private_key_path" => &self.ssh_private_key_path,
            "ssh_passphrase" => &self.ssh_passphrase,
            "name" => &self.name,
            "host" => &self.host,
            "port" => &self.port,
            "username" => &self.username,
            "database" => &self.database,
            other => panic!("unknown form field: {other}"),
        };
        Self::set_value(input, value.to_string(), window, cx);
    }

    #[cfg(test)]
    pub(crate) fn test_build_draft(&mut self, cx: &mut Context<Self>) -> Option<ConnectionDraft> {
        self.build_draft(cx)
    }

    /// Fill the minimum valid Postgres fields so Save can succeed in UI tests.
    /// Does not touch the password and never logs values.
    #[cfg(test)]
    pub(crate) fn test_seed_minimal_postgres(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::set_value(&self.name, "ui-test", window, cx);
        Self::set_value(&self.host, "127.0.0.1", window, cx);
        Self::set_value(&self.port, "5432", window, cx);
        Self::set_value(&self.username, "sela", window, cx);
        Self::set_value(&self.database, "sample", window, cx);
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        self.ensure_import_url_subscription(window, cx);
        self.ensure_field_change_subscriptions(window, cx);
        self.flush_pending_private_key_path(window, cx);
        let network = self.engine != Engine::Sqlite;
        let testing = self.is_testing();
        let palette = SelaPalette::for_mode(cx.theme().mode);
        let selected_color_label = self.color.label();
        let test_line = match &self.test_result {
            TestResult::Idle => None,
            TestResult::Testing => Some(("muted", "Testing…".to_string())),
            TestResult::Success { ms, via_ssh } => {
                Some(("success", TestResult::success_line(*ms, *via_ssh)))
            }
            TestResult::Failure(msg) => Some(("error", msg.clone())),
        };
        v_flex()
            .w_full()
            .gap_2()
            .p_1()
            .child(label("Import URL", cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.import_url).id("import-url-input")),
                    )
                    .child({
                        let this = this.clone();
                        Button::new("import-url")
                            .label("Import")
                            .ghost()
                            .on_click(move |_, window, cx| {
                                this.update(cx, |form, cx| form.import_from_url(window, cx));
                            })
                    }),
            )
            .child(
                h_flex().gap_2().child(engine_button(
                    this.clone(),
                    "engine-postgres",
                    Engine::Postgres,
                    self.engine,
                )).child(engine_button(
                    this.clone(),
                    "engine-mysql",
                    Engine::Mysql,
                    self.engine,
                )).child(engine_button(
                    this.clone(),
                    "engine-sqlite",
                    Engine::Sqlite,
                    self.engine,
                )),
            )
            .child(label("Name", cx))
            .child(Input::new(&self.name).id("name-input"))
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(label("Color", cx))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .children(ConnectionColor::all().iter().copied().map(
                                        |color| {
                                            color_swatch(this.clone(), color, self.color, &palette, cx)
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(selected_color_label),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Red marks production. A new connection set to Red starts in Confirm writes.",
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(label("Label", cx))
                            .child(Input::new(&self.conn_label).id("label-input")),
                    ),
            )
            .child(label("Group", cx))
            .child(Input::new(&self.group).id("group-input"))
            .child(label("Safe mode", cx))
            .child(h_flex().gap_1().children(
                SafeMode::all()
                    .iter()
                    .copied()
                    .map(|mode| safe_mode_button(this.clone(), mode, self.safe_mode)),
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.safe_mode.helper()),
            )
            .when(network, |form| {
                form.child(label("Host", cx))
                    .child(Input::new(&self.host).id("host-input"))
                    .child(label("Port", cx))
                    .child(Input::new(&self.port).id("port-input"))
                    .child(label("User", cx))
                    .child(Input::new(&self.username).id("username-input"))
                    .child(label("Database", cx))
                    .child(Input::new(&self.database).id("database-input"))
                    .child(label("Password", cx))
                    .child(
                        Input::new(&self.password)
                            .id("password-input")
                            .content_type(InputContentType::Password)
                            .mask_toggle(),
                    )
            })
            .when(network, |form| {
                form.child(self.render_ssh_section(this.clone(), cx))
            })
            .when(network, |form| {
                form.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .pt_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("TLS"),
                        )
                        .child(label("SSL mode", cx))
                        .child(h_flex().gap_1().children(
                            TlsMode::all()
                                .iter()
                                .copied()
                                .map(|mode| tls_mode_button(this.clone(), mode, self.tls_mode)),
                        ))
                        .when(self.tls_mode == TlsMode::Prefer, |tls| {
                            tls.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "prefer may fall back to unencrypted; use require or verify-full for remote servers",
                                    ),
                            )
                        })
                        .child(label("CA certificate path", cx))
                        .child(Input::new(&self.ca_path))
                        .child(label("Client certificate path", cx))
                        .child(Input::new(&self.client_cert_path))
                        .child(label("Client key path", cx))
                        .child(Input::new(&self.client_key_path)),
                )
            })
            .when(!network, |form| {
                form.child(label("SQLite file", cx))
                    .child(Input::new(&self.file_path))
            })
            .child({
                let this = this.clone();
                let checked = self.record_history;
                Checkbox::new("record-history")
                    .label("Record query history")
                    .checked(checked)
                    .on_click(move |value, _, cx| {
                        this.update(cx, |form, cx| {
                            form.record_history = *value;
                            cx.notify();
                        });
                    })
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Stores SQL you run on this connection in history.sqlite on this computer.",
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.keychain_unavailable {
                        "The OS keychain is unavailable, so the password will not be saved after you quit. It is kept in memory for this session only and is never written to the connections file. Certificate paths are plain text. Client keys must be unencrypted PEM for now. TLS options apply to PostgreSQL and MySQL only. SSH passwords and key passphrases use the same keychain.".to_string()
                    } else {
                        "Passwords are stored in the OS keychain, not in the connections file. Certificate paths are plain text. Client keys must be unencrypted PEM for now. TLS options apply to PostgreSQL and MySQL only. SSH passwords and key passphrases use the same keychain.".to_string()
                    }),
            )
            .when_some(self.import_note.clone(), |form, note| {
                form.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(note),
                )
            })
            .when_some(self.warning.clone(), |form, warning| {
                form.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(warning),
                )
            })
            .when_some(self.error.clone(), |form, error| {
                form.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when_some(test_line, |form, (kind, text)| {
                let color = match kind {
                    "success" => palette.status_success,
                    "error" => palette.status_error,
                    _ => cx.theme().muted_foreground,
                };
                form.child(
                    div()
                        .id("test-result")
                        .text_sm()
                        .text_color(color)
                        .child(text),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .pt_2()
                    .child({
                        let this = this.clone();
                        Button::new("cancel")
                            .label("Cancel")
                            .ghost()
                            .disabled(testing)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |form, cx| {
                                    form.cancel_test(cx);
                                    cx.emit(FormEvent::Cancel);
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("test-connection")
                            .label(if testing { "Testing…" } else { "Test" })
                            .disabled(testing)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |form, cx| form.request_test(cx));
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("save-only")
                            .label("Save")
                            .disabled(testing)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |form, cx| form.save_only(cx));
                            })
                    })
                    .child({
                        let this = this.clone();
                        // Keep id `save-connection` so existing UI tests keep working;
                        // label matches UX 05 (Save & connect).
                        Button::new("save-connection")
                            .label("Save & connect")
                            .primary()
                            .disabled(testing)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |form, cx| form.submit(cx));
                            })
                    }),
            )
    }
}

pub(super) fn label(text: &str, cx: &gpui_kit::App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
}

fn color_swatch(
    form: Entity<ConnectionForm>,
    color: ConnectionColor,
    current: ConnectionColor,
    palette: &SelaPalette,
    cx: &gpui_kit::App,
) -> impl IntoElement {
    let selected = color == current;
    let fill = palette.connection_color(color);
    let id = format!("conn-color-{}", color.as_str());
    div()
        .id(id)
        .w(px(16.))
        .h(px(16.))
        .flex_shrink_0()
        .cursor_pointer()
        .when(selected, |swatch| {
            swatch.border_2().border_color(cx.theme().ring)
        })
        .when(!selected, |swatch| {
            swatch.border_1().border_color(cx.theme().border)
        })
        .when_some(fill, |swatch, hue| swatch.bg(hue))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            form.update(cx, |form, cx| form.set_color(color, cx));
        })
}

fn engine_button(
    form: Entity<ConnectionForm>,
    id: &'static str,
    engine: Engine,
    current: Engine,
) -> impl IntoElement {
    let selected = engine == current;
    Button::new(id)
        .small()
        .label(engine.label())
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.ghost())
        .on_click(move |_, window, cx| {
            form.update(cx, |form, cx| form.set_engine(engine, window, cx));
        })
}

fn tls_mode_button(
    form: Entity<ConnectionForm>,
    mode: TlsMode,
    current: TlsMode,
) -> impl IntoElement {
    let selected = mode == current;
    let id = match mode {
        TlsMode::Disable => "tls-disable",
        TlsMode::Prefer => "tls-prefer",
        TlsMode::Require => "tls-require",
        TlsMode::VerifyCa => "tls-verify-ca",
        TlsMode::VerifyFull => "tls-verify-full",
    };
    Button::new(id)
        .small()
        .label(mode.label())
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.ghost())
        .on_click(move |_, _, cx| {
            form.update(cx, |form, cx| form.set_tls_mode(mode, cx));
        })
}

fn safe_mode_button(
    form: Entity<ConnectionForm>,
    mode: SafeMode,
    current: SafeMode,
) -> impl IntoElement {
    let selected = mode == current;
    let id = match mode {
        SafeMode::Off => "safe-mode-off",
        SafeMode::Confirm => "safe-mode-confirm",
        SafeMode::ReadOnly => "safe-mode-read-only",
    };
    Button::new(id)
        .small()
        .label(mode.form_label())
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.ghost())
        .on_click(move |_, _, cx| {
            form.update(cx, |form, cx| form.set_safe_mode(mode, cx));
        })
}

/// Keep import success and error lines mutually exclusive (#100).
fn apply_import_status_lines(
    import_note: &mut Option<String>,
    error: &mut Option<String>,
    outcome: Result<String, String>,
) {
    match outcome {
        Ok(note) => {
            *error = None;
            *import_note = Some(note);
        }
        Err(message) => {
            *import_note = None;
            *error = Some(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply_import_status_lines;

    #[test]
    fn import_failure_clears_previous_success_note() {
        let mut note =
            Some("Filled from the URL. The password is only in the password field.".into());
        let mut error = None;
        apply_import_status_lines(
            &mut note,
            &mut error,
            Err("Use a postgres://, mysql://, or sqlite: URL.".into()),
        );
        assert!(note.is_none());
        assert_eq!(
            error.as_deref(),
            Some("Use a postgres://, mysql://, or sqlite: URL.")
        );
    }

    #[test]
    fn import_success_clears_previous_error() {
        let mut note = None;
        let mut error = Some("Use a postgres://, mysql://, or sqlite: URL.".into());
        apply_import_status_lines(
            &mut note,
            &mut error,
            Ok("Filled from the URL. The password is only in the password field.".into()),
        );
        assert!(error.is_none());
        assert_eq!(
            note.as_deref(),
            Some("Filled from the URL. The password is only in the password field.")
        );
    }
}
