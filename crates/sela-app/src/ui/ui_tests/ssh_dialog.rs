//! Headless UI tests for the SSH tunnel block (M3-1 / SSH-* acceptance IDs).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    div, px, size, App, AppContext, Context, Entity, SharedString, TestAppContext, Window,
};
use sela_core::{Engine, SshAuthMethod};
use sela_ssh::SshPaths;
use tempfile::tempdir;

use crate::ui::form::{ssh::TestResult, ConnectionDraft, ConnectionForm, FormEvent};

struct DialogHost {
    form: Entity<ConnectionForm>,
    events: Arc<Mutex<Vec<HostEvent>>>,
    _subscription: gpui_kit::Subscription,
}

#[derive(Debug, Clone)]
enum HostEvent {
    Submitted {
        ssh_enabled: bool,
        ssh_host: String,
        ssh_auth: SshAuthMethod,
        ssh_key: String,
        has_ssh_password: bool,
        has_ssh_passphrase: bool,
    },
    SavedOnly {
        #[allow(dead_code)]
        ssh_enabled: bool,
    },
    Test {
        #[allow(dead_code)]
        ssh_enabled: bool,
    },
    Cancelled,
}

impl DialogHost {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events: Arc<Mutex<Vec<HostEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let form = cx.new(|cx| ConnectionForm::new(None, String::new(), None, false, window, cx));
        let events_for_sub = events.clone();
        let subscription = cx.subscribe_in(&form, window, {
            let events = events_for_sub;
            move |_this, _, event, window, cx| match event {
                FormEvent::Submit(draft) => {
                    push_submit(&events, draft);
                    window.close_dialog(cx);
                }
                FormEvent::SaveOnly(draft) => {
                    events.lock().expect("events").push(HostEvent::SavedOnly {
                        ssh_enabled: draft.connection.ssh.enabled,
                    });
                    window.close_dialog(cx);
                }
                FormEvent::Test(draft) => {
                    events.lock().expect("events").push(HostEvent::Test {
                        ssh_enabled: draft.connection.ssh.enabled,
                    });
                }
                FormEvent::Cancel => {
                    events.lock().expect("events").push(HostEvent::Cancelled);
                    window.close_dialog(cx);
                }
            }
        });
        Self {
            form,
            events,
            _subscription: subscription,
        }
    }

    fn show_dialog(&self, window: &mut Window, cx: &mut App) {
        let title: SharedString = "New connection".into();
        let form_for_dialog = self.form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(560.))
                .on_ok(|_, _, _| false)
                .child(form_for_dialog.clone())
        });
    }

    fn events(&self) -> Vec<HostEvent> {
        self.events.lock().expect("events").clone()
    }
}

fn push_submit(events: &Arc<Mutex<Vec<HostEvent>>>, draft: &ConnectionDraft) {
    // Never log secrets — only presence flags.
    events.lock().expect("events").push(HostEvent::Submitted {
        ssh_enabled: draft.connection.ssh.enabled,
        ssh_host: draft.connection.ssh.host.clone(),
        ssh_auth: draft.connection.ssh.auth,
        ssh_key: draft.connection.ssh.private_key_path.clone(),
        has_ssh_password: !draft.ssh_password.is_empty(),
        has_ssh_passphrase: !draft.ssh_passphrase.is_empty(),
    });
}

impl Render for DialogHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .id("ssh-dialog-host")
            .child("Sela SSH UI test host")
    }
}

async fn open_host(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<DialogHost>) {
    let (handle, host) =
        super::harness::open_window(cx, size(px(900.), px(1000.)), |window, cx| {
            cx.new(|cx| DialogHost::new(window, cx))
        });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            host.show_dialog(window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(2), |window, _| {
        window.try_find("dialog").is_some()
    })
    .await;
    (handle, host)
}

/// SSH-14: Postgres shows Use SSH tunnel; fields appear when on; default auth Private key.
#[gpui_kit::test]
async fn ssh14_postgres_shows_ssh_toggle_and_fields(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.within("dialog").try_find("ssh-enabled").is_some(),
            "SSH toggle must be present for Postgres"
        );
        // Toggle off → SSH host field absent.
        assert!(window.within("dialog").try_find("ssh-host-input").is_none());
        super::harness::click_in_dialog(window, "ssh-enabled", cx);
        window.render_frame(cx);
        assert!(window.within("dialog").try_find("ssh-host-input").is_some());
        assert!(window.within("dialog").try_find("ssh-port-input").is_some());
        assert!(window.within("dialog").try_find("ssh-user-input").is_some());
        assert!(window
            .within("dialog")
            .try_find("ssh-auth-private-key")
            .is_some());
        host.read(cx).form.read_with(cx, |form, cx| {
            assert!(form.test_ssh_enabled());
            assert_eq!(form.test_ssh_auth(), SshAuthMethod::PrivateKey);
            assert_eq!(form.test_field("ssh_port", cx), "22");
            assert!(form.test_ssh_passphrase_masked(cx));
        });
    })
    .unwrap();
}

/// SSH-14: SQLite has no SSH block (not a disabled checkbox).
#[gpui_kit::test]
async fn ssh14_sqlite_hides_ssh_section(cx: &mut TestAppContext) {
    let (handle, _host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        super::harness::click_in_dialog(window, "engine-sqlite", cx);
        window.render_frame(cx);
        assert!(
            window.within("dialog").try_find("ssh-enabled").is_none(),
            "SSH section must be absent for SQLite"
        );
        assert!(window.within("dialog").try_find("ssh-host-input").is_none());
        // Back to Postgres restores the toggle.
        super::harness::click_in_dialog(window, "engine-postgres", cx);
        window.render_frame(cx);
        assert!(window.within("dialog").try_find("ssh-enabled").is_some());
    })
    .unwrap();
}

/// SSH-14: validation copy when SSH is on.
#[gpui_kit::test]
async fn ssh14_validation_messages_when_enabled(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let form = host.read(cx).form.clone();
        form.update(cx, |form, cx| {
            form.test_seed_minimal_postgres(window, cx);
            form.test_set_ssh_enabled(true, cx);
        });
        // Empty host → validation error.
        form.update(cx, |form, cx| {
            let draft = form.test_build_draft(cx);
            assert!(draft.is_none());
            assert_eq!(form.test_error(), Some("SSH host is required."));
        });
        form.update(cx, |form, cx| {
            form.test_set_field("ssh_host", "bastion", window, cx);
            form.test_set_field("ssh_user", "", window, cx);
            let draft = form.test_build_draft(cx);
            assert!(draft.is_none());
            assert_eq!(form.test_error(), Some("SSH user is required."));
        });
        form.update(cx, |form, cx| {
            form.test_set_field("ssh_user", "ubuntu", window, cx);
            form.test_set_ssh_auth(SshAuthMethod::PrivateKey, cx);
            form.test_set_field("ssh_private_key_path", "", window, cx);
            let draft = form.test_build_draft(cx);
            assert!(draft.is_none());
            assert_eq!(form.test_error(), Some("Private key path is required."));
        });
        form.update(cx, |form, cx| {
            form.test_set_field("ssh_port", "0", window, cx);
            let draft = form.test_build_draft(cx);
            assert!(draft.is_none());
            assert_eq!(
                form.test_error(),
                Some("SSH port must be a number from 1 to 65535.")
            );
        });
    })
    .unwrap();
}

/// SSH-14 / SSH-13: save omits secrets from the connection; draft carries them separately.
#[gpui_kit::test]
async fn ssh14_save_round_trips_fields_without_secrets_in_json(cx: &mut TestAppContext) {
    let dir = tempdir().unwrap();
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(&key_path, "dummy-key").unwrap();

    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let form = host.read(cx).form.clone();
        form.update(cx, |form, cx| {
            form.test_seed_minimal_postgres(window, cx);
            form.test_set_ssh_enabled(true, cx);
            form.test_set_field("ssh_host", "bastion.example.com", window, cx);
            form.test_set_field("ssh_port", "22", window, cx);
            form.test_set_field("ssh_user", "ubuntu", window, cx);
            form.test_set_ssh_auth(SshAuthMethod::PrivateKey, cx);
            form.test_set_field(
                "ssh_private_key_path",
                &key_path.to_string_lossy(),
                window,
                cx,
            );
            form.test_set_field("ssh_passphrase", "secret-pp", window, cx);
        });
        super::harness::click_in_dialog(window, "save-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, _, cx| {
        let events = host.read(cx).events();
        match events.as_slice() {
            [HostEvent::Submitted {
                ssh_enabled,
                ssh_host,
                ssh_auth,
                ssh_key,
                has_ssh_password,
                has_ssh_passphrase,
            }] => {
                assert!(*ssh_enabled);
                assert_eq!(ssh_host, "bastion.example.com");
                assert_eq!(*ssh_auth, SshAuthMethod::PrivateKey);
                assert_eq!(ssh_key, &key_path.to_string_lossy());
                assert!(!*has_ssh_password);
                assert!(*has_ssh_passphrase);
            }
            other => panic!("unexpected events: {other:?}"),
        }
    })
    .unwrap();
}

/// SSH-05: Import from ~/.ssh/config fills Host/Port/User/IdentityFile.
#[gpui_kit::test]
async fn ssh05_import_ssh_config_fills_fields(cx: &mut TestAppContext) {
    let dir = tempdir().unwrap();
    let ssh_dir = dir.path().join(".ssh");
    std::fs::create_dir_all(&ssh_dir).unwrap();
    let key = ssh_dir.join("id_ed25519");
    std::fs::write(&key, "k").unwrap();
    std::fs::write(
        ssh_dir.join("config"),
        "Host mybastion\n  HostName bastion.internal\n  User deploy\n  Port 2222\n  IdentityFile ~/.ssh/id_ed25519\n",
    )
    .unwrap();
    let paths = SshPaths::from_home(dir.path());

    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let form = host.read(cx).form.clone();
        form.update(cx, |form, cx| {
            form.test_set_ssh_paths(paths);
            form.apply_ssh_config_alias("mybastion", window, cx);
        });
        form.read_with(cx, |form, cx| {
            assert!(form.test_ssh_enabled());
            assert_eq!(form.test_ssh_auth(), SshAuthMethod::PrivateKey);
            assert_eq!(form.test_field("ssh_host", cx), "bastion.internal");
            assert_eq!(form.test_field("ssh_port", cx), "2222");
            assert_eq!(form.test_field("ssh_user", cx), "deploy");
            assert!(form
                .test_field("ssh_private_key_path", cx)
                .contains("id_ed25519"));
            assert!(form.test_field("ssh_passphrase", cx).is_empty());
            assert_eq!(
                form.test_import_note(),
                Some("Filled SSH from Host mybastion.")
            );
        });
    })
    .unwrap();
}

/// SSH-19: Import URL never fills or clears SSH fields.
#[gpui_kit::test]
async fn ssh19_import_url_leaves_ssh_untouched(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let form = host.read(cx).form.clone();
        form.update(cx, |form, cx| {
            form.test_set_ssh_enabled(true, cx);
            form.test_set_field("ssh_host", "keep-bastion", window, cx);
            form.test_set_field("ssh_user", "keep-user", window, cx);
            form.test_set_field("ssh_passphrase", "keep-pp", window, cx);
        });
        window.within("dialog").click("import-url-input", cx);
        window.input("postgres://selaqa:selaqa@127.0.0.1:5432/selaqa", cx);
        window.within("dialog").click("import-url", cx);
        window.render_frame(cx);
        form.read_with(cx, |form, cx| {
            assert!(form.test_ssh_enabled());
            assert_eq!(form.test_field("ssh_host", cx), "keep-bastion");
            assert_eq!(form.test_field("ssh_user", cx), "keep-user");
            assert_eq!(form.test_field("ssh_passphrase", cx), "keep-pp");
            assert_eq!(form.test_field("host", cx), "127.0.0.1");
        });
    })
    .unwrap();
}

/// SSH-20: Duplicate copies SSH host settings; secrets stay on the draft only when typed.
#[gpui_kit::test]
async fn ssh20_duplicate_copies_ssh_host_not_secrets(cx: &mut TestAppContext) {
    use sela_core::{duplicate_connection, SavedConnection, SshSettings};

    let mut source = SavedConnection::new(Engine::Postgres);
    source.name = "Prod".into();
    source.host = "db".into();
    source.port = 5432;
    source.username = "u".into();
    source.database = "d".into();
    source.ssh = SshSettings {
        enabled: true,
        host: "bastion".into(),
        port: 22,
        user: "tunnel".into(),
        auth: SshAuthMethod::Password,
        private_key_path: String::new(),
    };
    let copy = duplicate_connection(&source, ["Prod"]);
    assert_eq!(copy.ssh, source.ssh);
    assert_ne!(copy.id, source.id);
    // UI: opening a form for the copy starts with empty SSH password field.
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        // Replace the empty form with one seeded from the duplicate (no secrets).
        let form = cx.new(|cx| {
            ConnectionForm::with_secrets(
                crate::ui::form::FormSecrets {
                    existing: Some(copy.clone()),
                    password: String::new(),
                    ssh_password: String::new(),
                    ssh_passphrase: String::new(),
                    warning: None,
                    keychain_unavailable: false,
                },
                window,
                cx,
            )
        });
        host.update(cx, |host, _| {
            host.form = form;
        });
        host.read(cx).form.read_with(cx, |form, cx| {
            assert!(form.test_ssh_enabled());
            assert_eq!(form.test_field("ssh_host", cx), "bastion");
            assert_eq!(form.test_ssh_auth(), SshAuthMethod::Password);
            assert!(form.test_field("ssh_password", cx).is_empty());
        });
    })
    .unwrap();
}

/// SSH-16: editing an SSH field clears the Test result line.
#[gpui_kit::test]
async fn ssh16_editing_ssh_field_clears_test_result(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let form = host.read(cx).form.clone();
        form.update(cx, |form, cx| {
            form.set_test_result(
                TestResult::Success {
                    ms: 12,
                    via_ssh: true,
                },
                cx,
            );
            assert_eq!(
                form.test_result_line().as_deref(),
                Some("Connected in 12 ms (via SSH).")
            );
            form.test_set_ssh_enabled(true, cx);
            assert!(form.test_result_line().is_none());
        });
    })
    .unwrap();
}

/// SSH-07 / SSH-18: dialog copy helpers match the UX spec (unit via UI module).
#[gpui_kit::test]
async fn ssh07_ssh18_dialog_copy_helpers(cx: &mut TestAppContext) {
    let _ = cx; // harness available; copy is pure.
    assert_eq!(sela_ssh::host_key_changed_title(), "SSH host key changed");
    let body = sela_ssh::host_key_changed_body("bastion", "SHA256:old", "SHA256:new");
    assert!(body.contains("Expected:"));
    assert!(body.contains("Presented:"));
    assert!(body.contains("Sela will not connect."));
    assert!(!body.to_lowercase().contains("replace"));
    assert_eq!(sela_ssh::unknown_host_key_title(), "Unknown SSH host key");
    let unknown = sela_ssh::unknown_host_key_body("bastion", "SHA256:abc");
    assert!(unknown.contains("SHA256:abc"));
    assert!(unknown.contains("Accept and save this key to known_hosts?"));
}

/// SSH-08: Test failure surfaces SSH: prefix (form TestResult line).
#[gpui_kit::test]
async fn ssh08_test_failure_shows_ssh_prefix(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let form = host.read(cx).form.clone();
        form.update(cx, |form, cx| {
            form.set_test_result(
                TestResult::Failure("SSH: authentication failed.".into()),
                cx,
            );
            let line = form.test_result_line().unwrap_or_default();
            assert!(
                line.starts_with("SSH:"),
                "SSH failures must use the SSH: prefix, got {line:?}"
            );
            assert_eq!(line, "SSH: authentication failed.");
            assert!(!line.to_lowercase().contains("password:"));
        });
    })
    .unwrap();
}
