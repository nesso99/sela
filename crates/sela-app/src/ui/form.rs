//! Create and edit a saved connection. The password never enters the JSON file.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputContentType, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, Context, Entity, EventEmitter, IntoElement, MouseButton, Render, Subscription, Window,
};

use sela_core::{
    parse_connection_url, safe_mode_driven_by_color, ConnectionColor, Engine, SafeMode,
    SavedConnection, TlsMode, TlsSettings,
};

use super::theme::SelaPalette;

#[derive(Clone)]
pub struct ConnectionDraft {
    pub connection: SavedConnection,
    pub password: String,
}

pub enum FormEvent {
    Submit(Box<ConnectionDraft>),
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
    _import_url_subscription: Option<Subscription>,
}

impl EventEmitter<FormEvent> for ConnectionForm {}

impl ConnectionForm {
    pub fn new(
        existing: Option<SavedConnection>,
        password: String,
        warning: Option<String>,
        keychain_unavailable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
            _import_url_subscription: None,
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
        } else if previous_engine == Engine::Sqlite {
            // SQLite forced Disable; Prefer is the default for new network connections.
            self.tls_mode = TlsMode::Prefer;
        }
        cx.notify();
    }

    fn set_tls_mode(&mut self, mode: TlsMode, cx: &mut Context<Self>) {
        if self.tls_mode == mode {
            return;
        }
        self.tls_mode = mode;
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

    fn submit(&mut self, cx: &mut Context<Self>) {
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
        } else {
            match port_text.parse::<u16>() {
                Ok(0) | Err(_) => {
                    self.error = Some("Port must be a number from 1 to 65535.".into());
                    cx.notify();
                    return;
                }
                Ok(port) => connection.port = port,
            }
            connection.tls = TlsSettings {
                mode: self.tls_mode,
                ca_path: Self::value(&self.ca_path, cx).trim().to_string(),
                client_cert_path: Self::value(&self.client_cert_path, cx).trim().to_string(),
                client_key_path: Self::value(&self.client_key_path, cx).trim().to_string(),
            };
        }
        if let Err(message) = connection.validate() {
            self.error = Some(message);
            cx.notify();
            return;
        }
        let password = Self::value(&self.password, cx);
        cx.emit(FormEvent::Submit(Box::new(ConnectionDraft {
            connection,
            password,
        })));
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
            other => panic!("unknown form field: {other}"),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_password_masked(&self, cx: &gpui_kit::App) -> bool {
        self.password.read(cx).presentation().is_masked()
    }

    #[cfg(test)]
    pub(crate) fn test_import_note(&self) -> Option<&str> {
        self.import_note.as_deref()
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
        let network = self.engine != Engine::Sqlite;
        let palette = SelaPalette::for_mode(cx.theme().mode);
        let selected_color_label = self.color.label();
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
            .when(network, |form| {
                form.child(label("Host", cx))
                    .child(Input::new(&self.host).id("host-input"))
                    .child(label("Port", cx))
                    .child(Input::new(&self.port).id("port-input"))
                    .child(label("User", cx))
                    .child(Input::new(&self.username).id("username-input"))
                    .child(label("Database", cx))
                    .child(Input::new(&self.database).id("database-input"))
                    .child(label("SSL mode", cx))
                    .child(h_flex().gap_1().children(
                        TlsMode::all()
                            .iter()
                            .copied()
                            .map(|mode| tls_mode_button(this.clone(), mode, self.tls_mode)),
                    ))
                    .when(self.tls_mode == TlsMode::Prefer, |form| {
                        form.child(
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
                    .child(Input::new(&self.client_key_path))
            })
            .when(!network, |form| {
                form.child(label("SQLite file", cx))
                    .child(Input::new(&self.file_path))
            })
            .when(network, |form| {
                form.child(label("Password", cx)).child(
                    Input::new(&self.password)
                        .id("password-input")
                        .content_type(InputContentType::Password)
                        .mask_toggle(),
                )
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
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.keychain_unavailable {
                        "The OS keychain is unavailable, so the password will not be saved after you quit. It is kept in memory for this session only and is never written to the connections file. Certificate paths are plain text. Client keys must be unencrypted PEM for now. TLS options apply to PostgreSQL and MySQL only.".to_string()
                    } else {
                        "Passwords are stored in the OS keychain, not in the connections file. Certificate paths are plain text. Client keys must be unencrypted PEM for now. TLS options apply to PostgreSQL and MySQL only.".to_string()
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
            .child(
                h_flex().gap_2().justify_end().pt_2().child({
                    let this = this.clone();
                    Button::new("cancel")
                        .label("Cancel")
                        .ghost()
                        .on_click(move |_, _, cx| {
                            this.update(cx, |_form, cx| cx.emit(FormEvent::Cancel));
                        })
                }).child({
                    let this = this.clone();
                    Button::new("save-connection")
                        .label("Save connection")
                        .primary()
                        .on_click(move |_, _, cx| {
                            this.update(cx, |form, cx| form.submit(cx));
                        })
                }),
            )
    }
}

fn label(text: &str, cx: &gpui_kit::App) -> impl IntoElement {
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
