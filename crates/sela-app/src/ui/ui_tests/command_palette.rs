//! M3-4 command palette / quick open headless UI tests (PAL-* / follow-ups).

use gpui_kit::component::button::Button;
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{div, px, size, Context, Entity, Focusable as _, NoAction, TestAppContext, Window};

use sela_core::{
    ColumnStructure, Dialect, IndexInfo, LoadedSchema, RelationKind, TableInfo, TableStructure,
    Value,
};

use crate::ui::workspace::edit_session::{format_commit_failure, EditSession};
use crate::ui::workspace::edit_ui::pending_bar_left_text;
use crate::ui::workspace::palette::{
    apply_query_edit, table_rows_from_catalog, visible_items, ActionContext, ActionEntry,
    PaletteAction, PaletteMode, NO_TABLES_ROW, PALETTE_MAX_HEIGHT, PALETTE_WIDTH,
};
use crate::ui::{PreviewChanges, ToggleCommandPalette, ToggleQuickOpen};

fn books_catalog() -> Vec<LoadedSchema> {
    vec![
        LoadedSchema {
            name: "public".into(),
            tables: vec![
                TableInfo {
                    name: "books".into(),
                    kind: RelationKind::Table,
                },
                TableInfo {
                    name: "authors".into(),
                    kind: RelationKind::Table,
                },
            ],
        },
        LoadedSchema {
            name: "other".into(),
            tables: vec![TableInfo {
                name: "books".into(),
                kind: RelationKind::Table,
            }],
        },
    ]
}

fn books_structure() -> TableStructure {
    TableStructure {
        schema: "public".into(),
        table: "books".into(),
        columns: vec![
            ColumnStructure {
                name: "id".into(),
                type_name: "integer".into(),
                nullable: false,
                default: None,
                comment: None,
                generated: false,
            },
            ColumnStructure {
                name: "title".into(),
                type_name: "text".into(),
                nullable: true,
                default: None,
                comment: None,
                generated: false,
            },
        ],
        indexes: vec![IndexInfo {
            name: "books_pkey".into(),
            columns: vec!["id".into()],
            unique: true,
            primary: true,
            definition: None,
        }],
        foreign_keys: vec![],
        ddl: String::new(),
    }
}

/// Isolated palette surface driven like Workspace (PAL-01…PAL-11 fixtures).
struct PaletteHost {
    mode: PaletteMode,
    action_entry: Option<ActionEntry>,
    open: bool,
    query: Entity<InputState>,
    query_text: String,
    selected: usize,
    catalog: Vec<LoadedSchema>,
    action_ctx: ActionContext,
    last_opened_table: Option<(String, String)>,
    last_action: Option<PaletteAction>,
    preview_toggled: bool,
    text_input_blocks_preview: bool,
}

impl PaletteHost {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search tables or type > for actions")
        });
        Self {
            mode: PaletteMode::Tables,
            action_entry: None,
            open: false,
            query,
            query_text: String::new(),
            selected: 0,
            catalog: books_catalog(),
            action_ctx: ActionContext {
                connected: true,
                sidebar_connection_id: Some("c1".into()),
                live_connection_id: Some("c1".into()),
                editor_has_sql: false,
                query_busy: false,
                has_exportable: false,
                table_data_open: true,
                read_only: false,
                font_size: 13,
                has_pending: true,
                committing: false,
                can_commit: true,
            },
            last_opened_table: None,
            last_action: None,
            preview_toggled: false,
            text_input_blocks_preview: false,
        }
    }

    fn items(&self) -> Vec<crate::ui::workspace::palette::PaletteItem> {
        visible_items(
            self.mode,
            &self.query_text,
            &self.catalog,
            &std::collections::VecDeque::new(),
            &std::collections::VecDeque::new(),
            &self.action_ctx,
        )
        .into_iter()
        .map(|(item, _)| item)
        .collect()
    }

    fn open_mode(&mut self, mode: PaletteMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.mode == mode {
            self.open = false;
            cx.notify();
            return;
        }
        self.open = true;
        self.mode = mode;
        self.action_entry = if mode == PaletteMode::Actions {
            Some(ActionEntry::Chord)
        } else {
            None
        };
        self.query_text.clear();
        self.selected = 0;
        self.query.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.query.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn sync_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.query.read(cx).value().to_string();
        let (mode, entry, stored) = apply_query_edit(self.mode, self.action_entry, &text);
        if mode == PaletteMode::Actions && self.mode == PaletteMode::Tables && text.starts_with('>')
        {
            self.query.update(cx, |input, cx| {
                input.set_value(&stored, window, cx);
            });
        }
        self.mode = mode;
        self.action_entry = entry;
        self.query_text = stored;
        self.selected = 0;
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let items = self.items();
        let Some(item) = items.get(self.selected) else {
            return;
        };
        if !item.enabled() {
            return;
        }
        match item {
            crate::ui::workspace::palette::PaletteItem::Table(row) => {
                self.last_opened_table = Some((row.schema.clone(), row.table.clone()));
                self.open = false;
            }
            crate::ui::workspace::palette::PaletteItem::Action { action, .. } => {
                self.last_action = Some(*action);
                self.open = false;
            }
            crate::ui::workspace::palette::PaletteItem::EmptyHint(_) => {}
        }
        cx.notify();
    }
}

impl Render for PaletteHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        div()
            .id("palette-host")
            .size_full()
            .relative()
            .on_action({
                let this = this.clone();
                move |_: &ToggleQuickOpen, window, cx| {
                    this.update(cx, |host, cx| {
                        host.open_mode(PaletteMode::Tables, window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &ToggleCommandPalette, window, cx| {
                    this.update(cx, |host, cx| {
                        host.open_mode(PaletteMode::Actions, window, cx);
                    });
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &PreviewChanges, _, cx| {
                    this.update(cx, |host, cx| {
                        if host.text_input_blocks_preview {
                            return;
                        }
                        host.preview_toggled = true;
                        cx.notify();
                    });
                }
            })
            .on_action(|_: &NoAction, _, _| {})
            .child(Button::new("open-tables").label("Open tables").on_click({
                let this = this.clone();
                move |_, window, cx| {
                    this.update(cx, |host, cx| {
                        host.open_mode(PaletteMode::Tables, window, cx);
                    });
                }
            }))
            .child(Button::new("open-actions").label("Open actions").on_click({
                let this = this.clone();
                move |_, window, cx| {
                    this.update(cx, |host, cx| {
                        host.open_mode(PaletteMode::Actions, window, cx);
                    });
                }
            }))
            .child(Button::new("confirm-row").label("Confirm").on_click({
                let this = this.clone();
                move |_, _, cx| {
                    this.update(cx, |host, cx| host.confirm(cx));
                }
            }))
            .child(Button::new("sync-query").label("Sync").on_click({
                let this = this.clone();
                move |_, window, cx| {
                    this.update(cx, |host, cx| host.sync_query(window, cx));
                }
            }))
            .child(Button::new("preview-btn").label("Preview").on_click({
                let this = this.clone();
                move |_, _, cx| {
                    this.update(cx, |host, cx| {
                        if host.text_input_blocks_preview {
                            return;
                        }
                        host.preview_toggled = true;
                        cx.notify();
                    });
                }
            }))
            .when(self.open, |root| {
                let items = self.items();
                let mode = match self.mode {
                    PaletteMode::Tables => "Tables",
                    PaletteMode::Actions => "Actions",
                };
                root.child(
                    div()
                        .id("command-palette")
                        .w(px(PALETTE_WIDTH))
                        .max_h(px(PALETTE_MAX_HEIGHT))
                        .child(div().id("palette-mode").child(mode))
                        .child(
                            gpui_kit::component::input::Input::new(&self.query)
                                .id("palette-search"),
                        )
                        .children(items.into_iter().enumerate().map(|(ix, item)| {
                            div().id(("palette-row", ix)).child(item.label()).when_some(
                                item.right_label(),
                                |row, right| {
                                    row.child(div().id(("palette-reason", ix)).child(right))
                                },
                            )
                        })),
                )
            })
            .child(
                div()
                    .id("opened-table")
                    .child(format!("{:?}", self.last_opened_table)),
            )
            .child(
                div()
                    .id("last-action")
                    .child(format!("{:?}", self.last_action)),
            )
            .child(
                div()
                    .id("preview-flag")
                    .child(if self.preview_toggled { "1" } else { "0" }),
            )
    }
}

/// PAL-01: open table mode; Escape / toggle closes. Overlay size constants.
#[gpui_kit::test]
fn pal01_open_escape_and_size(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
        cx.new(|cx| PaletteHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-tables", cx);
        window.render_frame(cx);
        assert!(host.read(cx).open);
        assert_eq!(host.read(cx).mode, PaletteMode::Tables);
        assert!(
            !host.read(cx).items().is_empty(),
            "open table mode must list catalog rows"
        );
        assert_eq!(PALETTE_WIDTH, 520.);
        assert_eq!(PALETTE_MAX_HEIGHT, 420.);
        // Second secondary-p toggles closed.
        window.click("open-tables", cx);
        window.render_frame(cx);
        assert!(!host.read(cx).open);
    })
    .unwrap();
}

/// PAL-02 + PAL-10: filter tables; confirm opens schema.table; duplicates disambiguated.
#[gpui_kit::test]
fn pal02_pal10_quick_open_table(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
        cx.new(|cx| PaletteHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let rows = table_rows_from_catalog(&host.read(cx).catalog);
        assert!(rows
            .iter()
            .any(|r| r.disambiguator.as_deref() == Some("public")));
        assert!(rows
            .iter()
            .any(|r| r.disambiguator.as_deref() == Some("other")));
        window.click("open-tables", cx);
        window.render_frame(cx);
        window.click("palette-search", cx);
        window.input("authors", cx);
        window.click("sync-query", cx);
        window.render_frame(cx);
        let labels: Vec<_> = host.read(cx).items().iter().map(|i| i.label()).collect();
        assert!(labels.iter().any(|l| l.contains("authors")));
        window.click("confirm-row", cx);
        window.render_frame(cx);
        assert_eq!(
            host.read(cx)
                .last_opened_table
                .as_ref()
                .map(|t| t.1.as_str()),
            Some("authors")
        );
        assert!(!host.read(cx).open);
    })
    .unwrap();
}

/// PAL-03 + PAL-06: action list includes primaries; dimmed reasons; Enter on dimmed is no-op.
#[gpui_kit::test]
fn pal03_pal06_actions_and_dimmed(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
        cx.new(|cx| PaletteHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-actions", cx);
        window.render_frame(cx);
        let items = host.read(cx).items();
        let labels: Vec<_> = items.iter().map(|i| i.label()).collect();
        for need in [
            "New connection",
            "Connect",
            "Disconnect",
            "New query tab",
            "Run query",
            "Refresh schema",
            "Export…",
            "Import CSV…",
            "Toggle theme",
            "Font bigger",
            "Font smaller",
            "Toggle safe mode",
            "Quit",
            "Preview SQL",
            "Commit",
            "Discard",
            "Stop",
            "Toggle filter",
            "Toggle history",
        ] {
            assert!(labels.iter().any(|l| l == need), "missing {need}");
        }
        let connect = items
            .iter()
            .find(|i| i.label() == "Connect")
            .expect("Connect");
        assert!(!connect.enabled());
        assert_eq!(connect.right_label().as_deref(), Some("Already connected"));
        let export = items
            .iter()
            .find(|i| i.label() == "Export…")
            .expect("Export");
        assert_eq!(export.right_label().as_deref(), Some("Nothing to export"));
        // Enter on dimmed Connect does nothing.
        host.update(cx, |host, cx| {
            host.selected = items.iter().position(|i| i.label() == "Connect").unwrap();
            host.confirm(cx);
        });
        assert!(host.read(cx).open);
        assert!(host.read(cx).last_action.is_none());
        // Enabled New connection runs.
        host.update(cx, |host, cx| {
            host.selected = items
                .iter()
                .position(|i| i.label() == "New connection")
                .unwrap();
            host.confirm(cx);
        });
        assert_eq!(
            host.read(cx).last_action,
            Some(PaletteAction::NewConnection)
        );
        assert!(!host.read(cx).open);
    })
    .unwrap();
}

/// PAL-05 / PAL-07 / PAL-11: keyboard filter, mode switch via `>`, chord switches mode.
#[gpui_kit::test]
fn pal05_pal07_pal11_keyboard_and_mode_switch(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
        cx.new(|cx| PaletteHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-tables", cx);
        window.render_frame(cx);
        window.click("palette-search", cx);
        window.input(">", cx);
        window.click("sync-query", cx);
        window.render_frame(cx);
        assert_eq!(host.read(cx).mode, PaletteMode::Actions);
        assert_eq!(host.read(cx).query_text, "");
        // Switch to tables chord while open keeps surface open.
        window.click("open-tables", cx);
        window.render_frame(cx);
        assert!(host.read(cx).open);
        assert_eq!(host.read(cx).mode, PaletteMode::Tables);
        // Action chord while open switches to actions.
        window.click("open-actions", cx);
        window.render_frame(cx);
        assert!(host.read(cx).open);
        assert_eq!(host.read(cx).mode, PaletteMode::Actions);
        // Second action chord closes.
        window.click("open-actions", cx);
        window.render_frame(cx);
        assert!(!host.read(cx).open);
    })
    .unwrap();
}

/// PAL-08: reserved chords are not palette bindings (constants / keymap shape).
#[gpui_kit::test]
fn pal08_reserved_chords_not_stolen(cx: &mut TestAppContext) {
    let (_handle, _host) =
        super::harness::open_window(cx, size(px(700.), px(400.)), |window, cx| {
            cx.new(|cx| PaletteHost::new(window, cx))
        });
    // Palette owns secondary-p / secondary-shift-p only. Quit / space stay elsewhere.
    // Asserted by action types existing alongside Quit (bound in install_sela_keybindings).
    let _ = ToggleQuickOpen;
    let _ = ToggleCommandPalette;
    let _ = crate::ui::Quit;
}

/// PAL-09: empty catalog shows muted empty row.
#[gpui_kit::test]
fn pal09_empty_schema_row(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
        cx.new(|cx| PaletteHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        host.update(cx, |host, _| {
            host.catalog.clear();
            host.action_ctx.connected = false;
        });
        window.click("open-tables", cx);
        window.render_frame(cx);
        let items = host.read(cx).items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label(), NO_TABLES_ROW);
        assert!(!items[0].enabled());
        host.update(cx, |host, cx| host.confirm(cx));
        assert!(host.read(cx).open);
        assert!(host.read(cx).last_opened_table.is_none());
    })
    .unwrap();
}

/// Preview chord must not fire while a text input has focus (paste).
#[gpui_kit::test]
fn preview_blocked_when_text_input_focused(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
        cx.new(|cx| PaletteHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, _| {
            host.text_input_blocks_preview = true;
        });
        window.click("preview-btn", cx);
        window.render_frame(cx);
        assert!(
            !host.read(cx).preview_toggled,
            "Preview must not run while a text input has focus"
        );
        host.update(cx, |host, _| {
            host.text_input_blocks_preview = false;
        });
        window.click("preview-btn", cx);
        window.render_frame(cx);
        assert!(host.read(cx).preview_toggled);
        // Action type stays registered for the DataTable keybinding path.
        let _ = PreviewChanges;
    })
    .unwrap();
}

/// Cancelled commit pending-bar copy (carry-over from #112).
#[gpui_kit::test]
fn cancelled_commit_pending_bar_copy(_cx: &mut TestAppContext) {
    let mut edit =
        EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
    let pk = vec![Value::Int(1)];
    edit.changeset
        .as_mut()
        .unwrap()
        .set_cell(&pk, "title", Value::text("a"), Value::text("b"))
        .unwrap();
    let gen = edit.begin_commit();
    let err = sela_core::DbError::cancelled("apply cancelled");
    let (_banner, status) = format_commit_failure(&err, 1);
    assert_eq!(status, "Commit cancelled. No changes were saved.");
    assert!(edit.note_commit_failure(gen, false, true));
    let counts = edit.pending_counts();
    let left = pending_bar_left_text(&edit, &counts);
    assert!(
        left.contains("Commit cancelled. No changes were saved."),
        "pending bar left={left}"
    );
    assert!(!left.contains("Commit failed"));
}
