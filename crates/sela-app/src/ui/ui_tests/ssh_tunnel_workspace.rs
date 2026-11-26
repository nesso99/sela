//! SSH-11 / SSH-16 headless Workspace tests (M3-1).
//!
//! Uses [`Workspace::new_for_tests`] with temp config paths and an in-memory
//! secret store. Never logs URLs or passwords.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{point, px, size, App, ElementId, Entity, ScrollDelta, TestAppContext, Window};
use sela_core::SshAuthMethod;
use sela_drivers::create_sample_database;
use sela_ssh::SshPaths;
use tempfile::TempDir;

use crate::ui::form::ConnectionForm;
use crate::ui::workspace::Workspace;

struct WorkspaceFixture {
    _dir: TempDir,
    handle: gpui_kit::AnyWindowHandle,
    workspace: Entity<Workspace>,
}

fn seed_sample_db(path: &std::path::Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("seed runtime")
            .block_on(create_sample_database(&path))
    })
    .join()
    .expect("seed thread")
    .expect("sample sqlite");
}

async fn open_connected_workspace(cx: &mut TestAppContext) -> WorkspaceFixture {
    cx.executor().allow_parking();
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("sample.sqlite");
    seed_sample_db(&db_path);
    let dir_path = dir.path().to_path_buf();
    let (handle, workspace) =
        super::harness::open_window(cx, size(px(1180.), px(760.)), |window, cx| {
            cx.new(|cx| Workspace::new_for_tests(&dir_path, window, cx))
        });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let id = workspace.update(cx, |ws, cx| {
            ws.test_seed_sqlite_connection("Sample", &db_path, cx)
        });
        workspace.update(cx, |ws, cx| ws.test_connect(id, window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(8), |_, cx| {
        workspace.read(cx).test_is_connected()
    })
    .await;
    WorkspaceFixture {
        _dir: dir,
        handle,
        workspace,
    }
}

fn ssh_env_present() -> bool {
    std::env::var("SELA_SSH_HOST")
        .ok()
        .filter(|h| !h.is_empty())
        .is_some()
}

fn ssh_required() -> bool {
    matches!(
        std::env::var("SELA_SSH_REQUIRED").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn require_ssh_var(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("SELA_SSH_HOST is set but {name} is missing (fail-not-skip)"))
}

/// Scroll Cancel into view inside the dialog (footer sits below the fold).
fn ensure_dialog_control_visible(window: &mut Window, id: &str, cx: &mut App) {
    let element_id: ElementId = id.to_string().into();
    for _ in 0..24 {
        window.render_frame(cx);
        if window
            .within("dialog")
            .try_find(element_id.clone())
            .is_some_and(|snap| snap.visible())
        {
            return;
        }
        window.within("dialog").scroll(
            "surface",
            ScrollDelta::Pixels(point(px(0.), px(-160.))),
            cx,
        );
    }
    panic!("ElementId {id:?} never became visible inside dialog after scrolling");
}

/// SSH-11: tunnel drop → banner, status, session gone; tabs + pending edits stay.
#[gpui_kit::test]
async fn ssh11_tunnel_drop_banner_status_keeps_tabs_and_edits(cx: &mut TestAppContext) {
    let fix = open_connected_workspace(cx).await;
    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        fix.workspace.update(cx, |ws, cx| {
            ws.test_new_query_tab(window, cx);
            ws.test_inject_pending_edit(cx);
            assert!(ws.test_pending_dirty(), "seed pending edit");
            assert!(ws.test_tab_count() >= 1);
            ws.test_mark_session_via_ssh();
            ws.test_simulate_ssh_tunnel_drop(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.update(|cx| {
        let ws = fix.workspace.read(cx);
        assert_eq!(
            ws.test_view_error().as_deref(),
            Some("SSH tunnel closed. The connection was dropped."),
            "SSH-11 banner"
        );
        assert_eq!(
            ws.test_view_status().as_deref(),
            Some("SSH tunnel closed."),
            "SSH-11 status"
        );
        assert!(
            !ws.test_is_connected(),
            "SSH-11: DB session must be dropped"
        );
        assert!(ws.test_tab_count() >= 1, "SSH-11: open tabs must remain");
        assert!(ws.test_pending_dirty(), "SSH-11: pending edits must remain");
    });
}

/// SSH-16: Cancel / cancel_test ignores a late Test result.
#[gpui_kit::test]
async fn ssh16_cancel_test_ignores_late_result(cx: &mut TestAppContext) {
    let fix = open_connected_workspace(cx).await;
    let active_before = cx.update(|cx| fix.workspace.read(cx).test_active_id().map(str::to_string));
    let connections_path = cx.update(|cx| fix.workspace.read(cx).test_connections_path());
    let json_before = std::fs::read_to_string(&connections_path).unwrap_or_default();

    // Keep the form Entity so we can assert after Cancel clears workspace.connection_form.
    let form_holder: std::sync::Arc<std::sync::Mutex<Option<Entity<ConnectionForm>>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        fix.workspace
            .update(cx, |ws, cx| ws.test_open_form(None, window, cx));
        window.render_frame(cx);
        let form = fix
            .workspace
            .read(cx)
            .test_connection_form()
            .expect("form open");
        *form_holder.lock().expect("holder") = Some(form.clone());

        // Temp known_hosts tree for the unreachable Test (never real ~/.ssh).
        let home = tempfile::tempdir().expect("ssh home");
        let known = home.path().join(".ssh").join("known_hosts");
        std::fs::create_dir_all(known.parent().unwrap()).ok();
        std::fs::write(&known, "").ok();
        let paths = SshPaths::from_home(home.path())
            .with_known_hosts(known)
            .without_config();
        // Keep the directory alive for the in-flight Test.
        std::mem::forget(home);

        form.update(cx, |form, cx| {
            form.test_seed_minimal_postgres(window, cx);
            // Unreachable bastion: Test runs until SSH connect times out.
            form.test_set_ssh_enabled(true, cx);
            form.test_set_ssh_auth(SshAuthMethod::Password, cx);
            form.test_set_field("ssh_host", "127.0.0.1", window, cx);
            form.test_set_field("ssh_port", "1", window, cx);
            form.test_set_field("ssh_user", "nobody", window, cx);
            form.test_set_field("ssh_password", "x", window, cx);
            form.test_set_ssh_paths(paths);
            form.test_request_test(cx);
            assert!(form.is_testing(), "Test should be in flight");
        });
        window.render_frame(cx);

        // Direct enabled-state check: gpui-component Button still fires on_click
        // when disabled, so click alone cannot prove Cancel stays interactive.
        ensure_dialog_control_visible(window, "cancel", cx);
        assert_ne!(
            window.within("dialog").find("cancel").disabled(),
            Some(true),
            "SSH-16: Cancel must stay enabled while Testing"
        );
        // Save / Test must disable while Testing (contrast control).
        assert_eq!(
            window.within("dialog").find("test-connection").disabled(),
            Some(true),
            "Test button disables while Testing"
        );

        // Production Cancel: cancel_test + FormEvent::Cancel (closes dialog).
        super::harness::click_in_dialog(window, "cancel", cx);
        window.render_frame(cx);
    })
    .unwrap();

    // Port 1 refuses quickly on the real tokio runtime; poll while GPUI
    // parks so a late Failure would apply if the generation gate were broken.
    let form = form_holder
        .lock()
        .expect("holder")
        .clone()
        .expect("form entity");
    for _ in 0..80 {
        cx.run_until_parked();
        let applied = cx.update(|cx| form.read(cx).test_result_line().is_some());
        assert!(
            !applied,
            "SSH-16: late Test result must not apply after cancel"
        );
        cx.executor().timer(Duration::from_millis(50)).await;
    }

    cx.update(|cx| {
        form.read_with(cx, |form, _| {
            assert!(
                form.test_result_line().is_none(),
                "SSH-16: Test line must stay Idle after cancel"
            );
        });
        let ws = fix.workspace.read(cx);
        assert_eq!(
            ws.test_active_id().map(str::to_string),
            active_before,
            "SSH-16: Test must not replace the workspace session"
        );
        assert!(ws.test_is_connected());
        assert!(
            !ws.test_connection_dialog_open(),
            "Cancel must close the dialog"
        );
    });

    let json_after = std::fs::read_to_string(&connections_path).unwrap_or_default();
    assert_eq!(
        json_before, json_after,
        "SSH-16: Test must not write connections.json"
    );
}

/// SSH-16: successful Test through the bastion does not persist or replace session.
///
/// Requires bastion env (`SELA_SSH_HOST`, …). When `SELA_SSH_REQUIRED=1` (CI
/// `ssh-integration`), missing env is a hard failure instead of a skip.
#[gpui_kit::test]
async fn ssh16_successful_test_does_not_persist_or_replace_session(cx: &mut TestAppContext) {
    if !ssh_env_present() {
        if ssh_required() {
            panic!(
                "SELA_SSH_REQUIRED=1 but SELA_SSH_HOST is unset; \
                 bastion env must be exported before running ssh_tunnel_workspace"
            );
        }
        eprintln!("skip SSH-16 success: set SELA_SSH_HOST (and related) to run");
        return;
    }
    let host = require_ssh_var("SELA_SSH_HOST");
    let port = require_ssh_var("SELA_SSH_PORT");
    let user = require_ssh_var("SELA_SSH_USER");
    let password = require_ssh_var("SELA_SSH_PASSWORD");
    let known_hosts = PathBuf::from(require_ssh_var("SELA_SSH_KNOWN_HOSTS"));
    let db_host = require_ssh_var("SELA_SSH_DB_HOST");
    let db_port = std::env::var("SELA_SSH_DB_PORT").unwrap_or_else(|_| "5432".into());
    let home = known_hosts
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("known_hosts under home/.ssh");

    let fix = open_connected_workspace(cx).await;
    let active_before = cx.update(|cx| fix.workspace.read(cx).test_active_id().map(str::to_string));
    let connections_path = cx.update(|cx| fix.workspace.read(cx).test_connections_path());
    let json_before = std::fs::read_to_string(&connections_path).unwrap_or_default();
    // Seed a sentinel and snapshot the full secret store (exact equality after Test).
    let sentinel_account = "ssh16-sentinel";
    let secrets_before = cx.update(|cx| {
        fix.workspace
            .read(cx)
            .test_set_secret(sentinel_account, "sentinel-value");
        let snap = fix.workspace.read(cx).test_secret_snapshot();
        assert!(
            snap.get(sentinel_account).map(String::as_str) == Some("sentinel-value"),
            "sentinel must be present before Test"
        );
        snap
    });

    cx.update_window(fix.handle, |_, window, cx| {
        window.render_frame(cx);
        fix.workspace
            .update(cx, |ws, cx| ws.test_open_form(None, window, cx));
        window.render_frame(cx);
        let form = fix
            .workspace
            .read(cx)
            .test_connection_form()
            .expect("form open");
        form.update(cx, |form, cx| {
            form.test_seed_minimal_postgres(window, cx);
            form.test_set_field("host", &db_host, window, cx);
            form.test_set_field("port", &db_port, window, cx);
            form.test_set_field("username", "sela", window, cx);
            form.test_set_field("database", "sample", window, cx);
            form.test_set_field("password", "sela", window, cx);
            form.test_set_ssh_enabled(true, cx);
            form.test_set_ssh_auth(SshAuthMethod::Password, cx);
            form.test_set_field("ssh_host", &host, window, cx);
            form.test_set_field("ssh_port", &port, window, cx);
            form.test_set_field("ssh_user", &user, window, cx);
            form.test_set_field("ssh_password", &password, window, cx);
            form.test_set_ssh_paths(
                SshPaths::from_home(&home)
                    .with_known_hosts(known_hosts.clone())
                    .without_config(),
            );
            form.test_request_test(cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    cx.wait_for(fix.handle, Duration::from_secs(20), |_, cx| {
        let form = fix.workspace.read(cx).test_connection_form();
        form.is_some_and(|f| {
            f.read(cx)
                .test_result_line()
                .is_some_and(|line| line.contains("via SSH") || line.starts_with("SSH:"))
        })
    })
    .await;

    cx.update(|cx| {
        let form = fix.workspace.read(cx).test_connection_form().expect("form");
        let line = form.read(cx).test_result_line().unwrap_or_default();
        assert!(
            line.contains("via SSH"),
            "SSH-16 success line must mention via SSH, got {line:?}"
        );
        assert!(
            line.starts_with("Connected in ") && line.ends_with(" ms (via SSH)."),
            "SSH-16 success copy, got {line:?}"
        );

        let ws = fix.workspace.read(cx);
        assert_eq!(
            ws.test_active_id().map(str::to_string),
            active_before,
            "SSH-16: successful Test must not replace the workspace session"
        );
        assert!(ws.test_is_connected());
        assert_eq!(
            ws.test_secret_snapshot(),
            secrets_before,
            "SSH-16: successful Test must not add, remove, or change secret-store accounts"
        );
    });

    let json_after = std::fs::read_to_string(&connections_path).unwrap_or_default();
    assert_eq!(
        json_before, json_after,
        "SSH-16: successful Test must not write connections.json"
    );
}
