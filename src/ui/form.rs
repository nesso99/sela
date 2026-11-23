//! Create and edit a saved connection. The password never enters the JSON file.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputContentType, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Entity, EventEmitter, IntoElement, Render, Window};

use crate::config::SavedConnection;
use crate::db::Engine;

#[derive(Clone)]
pub struct ConnectionDraft {
    pub connection: SavedConnection,
    pub password: String,
}

pub enum FormEvent {
    Submit(ConnectionDraft),
    Cancel,
}

pub struct ConnectionForm {
    existing_id: Option<String>,
    engine: Engine,
    ssl: bool,
    warning: Option<String>,
    error: Option<String>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    database: Entity<InputState>,
    file_path: Entity<InputState>,
    password: Entity<InputState>,
}

impl EventEmitter<FormEvent> for ConnectionForm {}

impl ConnectionForm {
    pub fn new(
        existing: Option<SavedConnection>,
        password: String,
        warning: Option<String>,
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
            ssl: seed.ssl,
            warning,
            error: None,
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
                    .placeholder("birch")
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
                InputState::new(window, cx)
                    .placeholder("Password")
                    .default_value(password)
            }),
        }
    }

    fn value(input: &Entity<InputState>, cx: &gpui_kit::App) -> String {
        input.read(cx).value().to_string()
    }

    fn set_engine(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        if self.engine == engine {
            return;
        }
        let previous = self.engine.default_port().to_string();
        let current = Self::value(&self.port, cx);
        self.engine = engine;
        if engine != Engine::Sqlite && (current.is_empty() || current == "0" || current == previous)
        {
            let next = engine.default_port().to_string();
            self.port.update(cx, |state, cx| {
                state.set_value(next, window, cx);
            });
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
        connection.ssl = self.ssl;
        connection.host = Self::value(&self.host, cx).trim().to_string();
        connection.username = Self::value(&self.username, cx).trim().to_string();
        connection.database = Self::value(&self.database, cx).trim().to_string();
        connection.file_path = Self::value(&self.file_path, cx).trim().to_string();
        let port_text = Self::value(&self.port, cx).trim().to_string();
        if self.engine == Engine::Sqlite {
            connection.port = 0;
            connection.host.clear();
            connection.username.clear();
            connection.database.clear();
            connection.ssl = false;
        } else {
            match port_text.parse::<u16>() {
                Ok(0) | Err(_) => {
                    self.error = Some("Port must be a number from 1 to 65535.".into());
                    cx.notify();
                    return;
                }
                Ok(port) => connection.port = port,
            }
        }
        if let Err(message) = connection.validate() {
            self.error = Some(message);
            cx.notify();
            return;
        }
        let password = Self::value(&self.password, cx);
        cx.emit(FormEvent::Submit(ConnectionDraft {
            connection,
            password,
        }));
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let network = self.engine != Engine::Sqlite;
        v_flex()
            .w_full()
            .gap_2()
            .p_1()
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
            .child(Input::new(&self.name))
            .when(network, |form| {
                form.child(label("Host", cx))
                    .child(Input::new(&self.host))
                    .child(label("Port", cx))
                    .child(Input::new(&self.port))
                    .child(label("User", cx))
                    .child(Input::new(&self.username))
                    .child(label("Database", cx))
                    .child(Input::new(&self.database))
                    .child(
                        Checkbox::new("ssl")
                            .checked(self.ssl)
                            .label("Require SSL")
                            .on_change({
                                let this = this.clone();
                                move |checked, _window, cx| {
                                    let checked = *checked;
                                    this.update(cx, |form, cx| {
                                        form.ssl = checked;
                                        cx.notify();
                                    });
                                }
                            }),
                    )
            })
            .when(!network, |form| {
                form.child(label("SQLite file", cx))
                    .child(Input::new(&self.file_path))
            })
            .when(network, |form| {
                form.child(label("Password", cx)).child(
                    Input::new(&self.password)
                        .content_type(InputContentType::Password)
                        .mask_toggle(),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Passwords are stored in the OS keychain, not in the connections file. An empty password is kept only for this session.",
                    ),
            )
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
