//! SSH tunnel block on the connection dialog (M3-1 / UX 10-ssh-tunnel).
//!
//! Shown for PostgreSQL and MySQL only — absent (not disabled) for SQLite.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputContentType, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, Context, Entity, IntoElement, PathPromptOptions, Render, SharedString, Window,
};

use sela_core::SshAuthMethod;
use sela_ssh::{import_host_alias, list_host_aliases, SshPaths};

use crate::ui::file_picker::{apply_file_picker_outcome, classify_paths_result};

use super::{label, ConnectionForm};

/// Result line for Test connection (SSH-16 / UX 05).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TestResult {
    #[default]
    Idle,
    Testing,
    Success {
        ms: u64,
        via_ssh: bool,
    },
    Failure(String),
}

impl TestResult {
    pub fn success_line(ms: u64, via_ssh: bool) -> String {
        if via_ssh {
            format!("Connected in {ms} ms (via SSH).")
        } else {
            format!("Connected in {ms} ms.")
        }
    }
}

/// Host-alias picker opened by **Import from ~/.ssh/config** (SSH-05).
pub(super) struct SshConfigPicker {
    form: Entity<ConnectionForm>,
    aliases: Vec<String>,
    filter: Entity<InputState>,
    error: Option<String>,
}

impl SshConfigPicker {
    pub(super) fn new(
        form: Entity<ConnectionForm>,
        aliases: Vec<String>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            form,
            aliases,
            filter: cx.new(|cx| InputState::new(window, cx).placeholder("Filter hosts")),
            error,
        }
    }
}

impl Render for SshConfigPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self
            .filter
            .read(cx)
            .value()
            .to_string()
            .to_ascii_lowercase();
        let visible: Vec<String> = self
            .aliases
            .iter()
            .filter(|a| filter.is_empty() || a.to_ascii_lowercase().contains(&filter))
            .cloned()
            .collect();
        let form = self.form.clone();
        v_flex()
            .w_full()
            .gap_2()
            .p_1()
            .child(label("Host alias", cx))
            .child(Input::new(&self.filter).id("ssh-config-filter"))
            .when_some(self.error.clone(), |el, err| {
                el.child(div().text_sm().text_color(cx.theme().danger).child(err))
            })
            .child(v_flex().w_full().gap_1().max_h(px(240.)).children(
                visible.into_iter().enumerate().map(|(idx, alias)| {
                    let form = form.clone();
                    let alias_for_click = alias.clone();
                    let id: SharedString = format!("ssh-alias-{idx}").into();
                    Button::new(id)
                        .label(alias)
                        .ghost()
                        .w_full()
                        .on_click(move |_, window, cx| {
                            let alias = alias_for_click.clone();
                            form.update(cx, |form, cx| {
                                form.apply_ssh_config_alias(&alias, window, cx);
                            });
                            window.close_dialog(cx);
                        })
                }),
            ))
            .child({
                Button::new("ssh-config-picker-cancel")
                    .label("Cancel")
                    .ghost()
                    .on_click(|_, window, cx| {
                        window.close_dialog(cx);
                    })
            })
    }
}

impl ConnectionForm {
    pub(super) fn set_ssh_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.ssh_enabled == enabled {
            return;
        }
        self.ssh_enabled = enabled;
        self.clear_test_result();
        cx.notify();
    }

    pub(super) fn set_ssh_auth(&mut self, auth: SshAuthMethod, cx: &mut Context<Self>) {
        if self.ssh_auth == auth {
            return;
        }
        self.ssh_auth = auth;
        self.clear_test_result();
        cx.notify();
    }

    pub(super) fn clear_test_result(&mut self) {
        self.test_result = TestResult::Idle;
    }

    pub(crate) fn set_test_result(&mut self, result: TestResult, cx: &mut Context<Self>) {
        self.test_result = result;
        cx.notify();
    }

    pub(crate) fn ssh_paths_or_default(&self) -> Result<SshPaths, String> {
        if let Some(paths) = &self.ssh_paths_override {
            return Ok(paths.clone());
        }
        SshPaths::system_default().map_err(|err| err.to_string())
    }

    /// Open the host-alias picker (SSH-05).
    pub(super) fn open_ssh_config_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (aliases, error) = match self.ssh_paths_or_default() {
            Ok(paths) => match list_host_aliases(&paths) {
                Ok(list) => (list, None),
                Err(err) => (Vec::new(), Some(err.to_string())),
            },
            Err(err) => (Vec::new(), Some(err)),
        };
        let form = cx.entity();
        let picker = cx.new(|cx| SshConfigPicker::new(form, aliases, error, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Import from ~/.ssh/config")
                .width(px(420.))
                .on_ok(|_, _, _| false)
                .child(picker.clone())
        });
    }

    /// Fill SSH fields from a Host alias (SSH-05). Clears on-screen SSH secrets.
    pub(crate) fn apply_ssh_config_alias(
        &mut self,
        alias: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = match self.ssh_paths_or_default() {
            Ok(p) => p,
            Err(err) => {
                self.error = Some(err);
                self.import_note = None;
                cx.notify();
                return;
            }
        };
        match import_host_alias(alias, &paths) {
            Ok(fields) => {
                self.ssh_enabled = true;
                self.ssh_auth = SshAuthMethod::PrivateKey;
                Self::set_value(&self.ssh_host, fields.host, window, cx);
                Self::set_value(&self.ssh_port, fields.port.to_string(), window, cx);
                Self::set_value(&self.ssh_user, fields.user, window, cx);
                Self::set_value(
                    &self.ssh_private_key_path,
                    fields.private_key_path,
                    window,
                    cx,
                );
                // Changing host clears on-screen SSH password/passphrase.
                Self::set_value(&self.ssh_password, String::new(), window, cx);
                Self::set_value(&self.ssh_passphrase, String::new(), window, cx);
                self.ssh_password.update(cx, |state, cx| {
                    state.set_masked(true, window, cx);
                });
                self.ssh_passphrase.update(cx, |state, cx| {
                    state.set_masked(true, window, cx);
                });
                self.clear_test_result();
                self.error = None;
                self.import_note = Some(format!("Filled SSH from Host {alias}."));
            }
            Err(err) => {
                self.import_note = None;
                self.error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    pub(super) fn browse_private_key(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose private key".into()),
        });
        cx.spawn(async move |this, async_cx| {
            let outcome = classify_paths_result(receiver.await);
            this.update(async_cx, |form, cx| {
                form.error = apply_file_picker_outcome(outcome, |path| {
                    form.pending_private_key_path = Some(path.to_string_lossy().to_string());
                    form.clear_test_result();
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Apply a Browse result that arrived without a Window in the async task.
    pub(super) fn flush_pending_private_key_path(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.pending_private_key_path.take() {
            Self::set_value(&self.ssh_private_key_path, path, window, cx);
            self.clear_test_result();
        }
    }

    pub(super) fn render_ssh_section(
        &mut self,
        this: Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let enabled = self.ssh_enabled;
        let auth = self.ssh_auth;
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
                    .child("SSH"),
            )
            .child({
                let this = this.clone();
                Checkbox::new("ssh-enabled")
                    .label("Use SSH tunnel")
                    .checked(enabled)
                    .on_click(move |value, _, cx| {
                        this.update(cx, |form, cx| form.set_ssh_enabled(*value, cx));
                    })
            })
            .when(enabled, |section| {
                section
                    .child(label("SSH host", cx))
                    .child(Input::new(&self.ssh_host).id("ssh-host-input"))
                    .child(label("SSH port", cx))
                    .child(Input::new(&self.ssh_port).id("ssh-port-input"))
                    .child(label("SSH user", cx))
                    .child(Input::new(&self.ssh_user).id("ssh-user-input"))
                    .child(label("Auth", cx))
                    .child(
                        h_flex().gap_1().children(
                            [
                                SshAuthMethod::Password,
                                SshAuthMethod::PrivateKey,
                                SshAuthMethod::Agent,
                            ]
                            .into_iter()
                            .map(|method| ssh_auth_button(this.clone(), method, auth)),
                        ),
                    )
                    .when(auth == SshAuthMethod::Password, |section| {
                        section.child(label("SSH password", cx)).child(
                            Input::new(&self.ssh_password)
                                .id("ssh-password-input")
                                .content_type(InputContentType::Password)
                                .mask_toggle(),
                        )
                    })
                    .when(auth == SshAuthMethod::PrivateKey, |section| {
                        section
                            .child(label("Private key", cx))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div().flex_1().child(
                                            Input::new(&self.ssh_private_key_path)
                                                .id("ssh-key-path-input"),
                                        ),
                                    )
                                    .child({
                                        let this = this.clone();
                                        Button::new("ssh-browse-key")
                                            .label("Browse")
                                            .ghost()
                                            .on_click(move |_, window, cx| {
                                                this.update(cx, |form, cx| {
                                                    form.browse_private_key(window, cx);
                                                });
                                            })
                                    }),
                            )
                            .child(label("Passphrase", cx))
                            .child(
                                Input::new(&self.ssh_passphrase)
                                    .id("ssh-passphrase-input")
                                    .content_type(InputContentType::Password)
                                    .mask_toggle(),
                            )
                    })
                    .child({
                        let this = this.clone();
                        Button::new("ssh-import-config")
                            .label("Import from ~/.ssh/config")
                            .ghost()
                            .on_click(move |_, window, cx| {
                                this.update(cx, |form, cx| {
                                    form.open_ssh_config_picker(window, cx);
                                });
                            })
                    })
            })
    }
}

fn ssh_auth_button(
    form: Entity<ConnectionForm>,
    method: SshAuthMethod,
    current: SshAuthMethod,
) -> impl IntoElement {
    let selected = method == current;
    let (id, label) = match method {
        SshAuthMethod::Password => ("ssh-auth-password", "Password"),
        SshAuthMethod::PrivateKey => ("ssh-auth-private-key", "Private key"),
        SshAuthMethod::Agent => ("ssh-auth-agent", "SSH agent"),
    };
    Button::new(id)
        .small()
        .label(label)
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.ghost())
        .on_click(move |_, _, cx| {
            form.update(cx, |form, cx| form.set_ssh_auth(method, cx));
        })
}

#[cfg(test)]
mod tests {
    use super::TestResult;

    #[test]
    fn test_success_line_marks_ssh() {
        assert_eq!(
            TestResult::success_line(12, true),
            "Connected in 12 ms (via SSH)."
        );
        assert_eq!(TestResult::success_line(12, false), "Connected in 12 ms.");
    }
}
