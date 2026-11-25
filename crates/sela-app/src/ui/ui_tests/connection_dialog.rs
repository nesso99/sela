//! Connection dialog UI tests: Import URL Enter (#96), Save/Cancel, password mask.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    div, px, size, App, AppContext, Context, Entity, SharedString, TestAppContext, Window,
};

use crate::ui::form::{ConnectionForm, FormEvent};

/// Host that opens `ConnectionForm` the same way the workspace does after #96:
/// kit dialog with `.on_ok(|_, _, _| false)` so Enter from inputs does not
/// dismiss the dialog; Save/Cancel go through FormEvent only.
struct DialogHost {
    form: Entity<ConnectionForm>,
    events: Arc<Mutex<Vec<HostEvent>>>,
    _subscription: gpui_kit::Subscription,
}

#[derive(Debug, Clone)]
enum HostEvent {
    Submitted { name: String, host: String },
    Cancelled,
}

impl DialogHost {
    /// Build the form host. Call [`Self::show_dialog`] only after
    /// `gpui_kit::open_window` returns — Root/WindowState is not ready inside
    /// the build closure.
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events: Arc<Mutex<Vec<HostEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let form = cx.new(|cx| ConnectionForm::new(None, String::new(), None, false, window, cx));
        let events_for_sub = events.clone();
        let subscription = cx.subscribe_in(&form, window, {
            let events = events_for_sub;
            move |_this, _, event, window, cx| match event {
                FormEvent::Submit(draft) => {
                    // Never log draft.password or connection URLs.
                    events.lock().expect("events").push(HostEvent::Submitted {
                        name: draft.connection.name.clone(),
                        host: draft.connection.host.clone(),
                    });
                    window.close_dialog(cx);
                }
                FormEvent::SaveOnly(draft) => {
                    events.lock().expect("events").push(HostEvent::Submitted {
                        name: draft.connection.name.clone(),
                        host: draft.connection.host.clone(),
                    });
                    window.close_dialog(cx);
                }
                FormEvent::Test(_) => {}
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
                // Match Workspace::open_connection_dialog (#96): Enter from a
                // single-line Input must not Confirm/dismiss the dialog.
                .on_ok(|_, _, _| false)
                .child(form_for_dialog.clone())
        });
    }

    fn events(&self) -> Vec<HostEvent> {
        self.events.lock().expect("events").clone()
    }
}

impl Render for DialogHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .id("dialog-host")
            .child("Sela UI test host")
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

/// #96: Enter in Import URL fills the form and keeps the dialog open.
#[gpui_kit::test]
async fn import_url_enter_fills_form_and_keeps_dialog_open(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    // Secret appears only in the password field after import — never assert it
    // via accessibility value, and never print it.
    const SAMPLE_URL: &str = "postgres://selaqa:selaqa@127.0.0.1:5432/selaqa";
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("import-url-input", cx);
        window.input(SAMPLE_URL, cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("dialog").is_some(),
            "Enter in Import URL must not close the connection dialog (#96)"
        );
        assert!(
            host.read(cx).events().is_empty(),
            "Enter must not Submit or Cancel"
        );
        let form = host.read(cx).form.clone();
        form.read_with(cx, |form, cx| {
            assert_eq!(form.test_field("host", cx), "127.0.0.1");
            assert_eq!(form.test_field("port", cx), "5432");
            assert_eq!(form.test_field("username", cx), "selaqa");
            assert_eq!(form.test_field("database", cx), "selaqa");
            assert_eq!(form.test_field("password", cx), "selaqa");
            assert!(
                form.test_password_masked(cx),
                "password must stay masked after Import URL"
            );
            let note = form.test_import_note().unwrap_or("");
            assert!(
                !note.contains("selaqa"),
                "import note must not echo the password"
            );
            assert!(form.test_field("import_url", cx).is_empty());
        });
        let password = window.within("dialog").find("password-input");
        assert_eq!(password.value(), None);
        assert_eq!(password.role(), Some(gpui_kit::Role::PasswordInput));
    })
    .unwrap();
}

/// Enter in Name (or Host) must not dismiss the dialog or save (#96 meaning).
#[gpui_kit::test]
async fn enter_in_name_field_does_not_close_or_save(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("name-input", cx);
        window.input("Should stay open", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("dialog").is_some(),
            "Enter in Name must not close the New connection dialog"
        );
        assert!(
            host.read(cx).events().is_empty(),
            "Enter in a field must not Submit or Cancel; nothing is saved"
        );
        let form = host.read(cx).form.clone();
        form.read_with(cx, |form, cx| {
            assert_eq!(form.test_field("name", cx), "Should stay open");
            // New Postgres drafts default host to 127.0.0.1; Enter must not
            // Submit or Import URL (no import note, password stays empty).
            assert_eq!(form.test_field("host", cx), "127.0.0.1");
            assert!(form.test_field("password", cx).is_empty());
            assert!(form.test_import_note().is_none());
        });
    })
    .unwrap();
}

#[gpui_kit::test]
async fn enter_in_host_field_does_not_close_or_save(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("host-input", cx);
        window.input("127.0.0.1", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("dialog").is_some(),
            "Enter in Host must not close the New connection dialog"
        );
        assert!(
            host.read(cx).events().is_empty(),
            "Enter in Host must not Submit or Cancel"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn import_button_fills_form_and_masks_password(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    const SAMPLE_URL: &str = "postgres://selaqa:selaqa@127.0.0.1:5432/selaqa";
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("import-url-input", cx);
        window.input(SAMPLE_URL, cx);
        window.within("dialog").click("import-url", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("dialog").is_some());
        let form = host.read(cx).form.clone();
        form.read_with(cx, |form, cx| {
            assert_eq!(form.test_field("host", cx), "127.0.0.1");
            assert_eq!(form.test_field("port", cx), "5432");
            assert_eq!(form.test_field("username", cx), "selaqa");
            assert_eq!(form.test_field("database", cx), "selaqa");
            assert_eq!(form.test_field("password", cx), "selaqa");
            assert!(form.test_password_masked(cx));
            let note = form.test_import_note().unwrap_or("");
            assert!(
                note.contains("Filled from the URL"),
                "expected muted import note"
            );
            assert!(
                !note.to_lowercase().contains("password:") && !note.contains("selaqa@"),
                "note must not contain the URL or password"
            );
        });
        assert_eq!(
            window.within("dialog").find("password-input").value(),
            None,
            "masked password must not appear in accessibility value"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn save_with_focus_in_name_submits_draft(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    // Defaults already fill host/port; type the remaining required fields and Save.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("name-input", cx);
        window.input("Local library", cx);
        window.within("dialog").click("username-input", cx);
        window.input("sela", cx);
        window.within("dialog").click("database-input", cx);
        window.input("sample", cx);
        // Focus stays in a field; Save still submits.
        window.within("dialog").click("name-input", cx);
        super::harness::click_in_dialog(window, "save-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("dialog").is_none(), "Save closes dialog");
        let events = host.read(cx).events();
        assert!(
            matches!(
                events.as_slice(),
                [HostEvent::Submitted { name, host }] if name == "Local library" && host == "127.0.0.1"
            ),
            "unexpected events: {events:?}"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn cancel_with_focus_in_field_closes_without_submit(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("name-input", cx);
        window.input("Do not save", cx);
        super::harness::click_in_dialog(window, "cancel", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("dialog").is_none());
        let events = host.read(cx).events();
        assert!(
            matches!(events.as_slice(), [HostEvent::Cancelled]),
            "unexpected events: {events:?}"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn password_field_is_masked_by_default(cx: &mut TestAppContext) {
    let (handle, host) = open_host(cx).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        super::harness::click_in_dialog(window, "password-input", cx);
        window.input("hunter2", cx);
        let password = window.within("dialog").find("password-input");
        assert_eq!(password.role(), Some(gpui_kit::Role::PasswordInput));
        assert_eq!(
            password.value(),
            None,
            "accessibility value must omit the secret"
        );
        host.read(cx).form.read_with(cx, |form, cx| {
            assert!(form.test_password_masked(cx));
            assert_eq!(form.test_field("password", cx), "hunter2");
        });
    })
    .unwrap();
}
