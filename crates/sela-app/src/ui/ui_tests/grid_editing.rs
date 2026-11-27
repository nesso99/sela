//! Grid cell-editor UI tests: Space, empty accept, Duplicate + Copy as INSERT.

use std::collections::BTreeMap;
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    div, px, size, AppContext, ClipboardItem, Context, Entity, Focusable, TestAppContext, Window,
};

use sela_core::{
    format_selection, ColumnMeta, ColumnStructure, CopyFormat, Dialect, IndexInfo, QueryPage,
    RelationKind, RowChange, TableStructure, Value,
};

use crate::ui::grid::{GridDelegate, OverlayPaintRow};
use crate::ui::workspace::edit_session::{EditRowId, EditSession, RowMarker};
use crate::ui::workspace::tab_view::CellInspect;
use crate::ui::{AcceptCellEditor, CopyAsInsert, DuplicateEditRow, InspectCell};

fn books_structure() -> TableStructure {
    TableStructure {
        schema: "public".into(),
        table: "books".into(),
        columns: vec![
            ColumnStructure {
                name: "id".into(),
                type_name: "integer".into(),
                nullable: false,
                default: Some("nextval('books_id_seq')".into()),
                comment: None,
                generated: false,
            },
            ColumnStructure {
                name: "note".into(),
                type_name: "text".into(),
                nullable: true,
                default: None,
                comment: None,
                generated: false,
            },
            ColumnStructure {
                name: "pages".into(),
                type_name: "integer".into(),
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
            partial: false,
            expression: false,
            definition: None,
        }],
        foreign_keys: vec![],
        ddl: String::new(),
    }
}

fn books_page() -> QueryPage {
    QueryPage {
        columns: vec![
            ColumnMeta {
                name: "id".into(),
                type_name: "INTEGER".into(),
            },
            ColumnMeta {
                name: "note".into(),
                type_name: "TEXT".into(),
            },
            ColumnMeta {
                name: "pages".into(),
                type_name: "INTEGER".into(),
            },
        ],
        rows: vec![vec![Value::Int(1), Value::Null, Value::Int(310)]],
        duration: Duration::from_millis(0),
    }
}

/// Focused harness: DataTable + in-cell editor with production key contexts.
struct GridEditHost {
    grid: Entity<TableState<GridDelegate>>,
    cell_editor: Entity<InputState>,
    edit: EditSession,
    cell_inspect: Option<CellInspect>,
    last_insert_sql: Option<String>,
}

impl GridEditHost {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let edit =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let grid = cx.new(|cx| {
            let mut state = TableState::new(GridDelegate::empty(), window, cx)
                .sortable(false)
                .col_movable(false)
                .cell_selectable(true)
                .row_selectable(true)
                .col_selectable(true)
                .row_header(true);
            state.delegate_mut().show_page(books_page());
            state
        });
        let cell_editor = cx.new(|cx| InputState::new(window, cx).placeholder("NULL"));
        Self {
            grid,
            cell_editor,
            edit,
            cell_inspect: None,
            last_insert_sql: None,
        }
    }

    fn overlay_paint_rows(&self) -> Vec<OverlayPaintRow> {
        self.edit
            .overlay
            .iter()
            .map(|row| {
                let mut raw = BTreeMap::new();
                if let Some(cs) = &self.edit.changeset {
                    for change in cs.changes() {
                        if let RowChange::Insert { id, values: vals } = change {
                            if *id == row.insert_id {
                                for (k, v) in vals {
                                    raw.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
                OverlayPaintRow {
                    insert_id: row.insert_id,
                    marker: if row.duplicated {
                        RowMarker::Duplicated
                    } else {
                        RowMarker::Inserted
                    },
                    raw,
                    after_server_row: row.after_source_pk.as_ref().map(|_| 0usize),
                    required_unset: vec!["id".into()],
                    generated: Vec::new(),
                }
            })
            .collect()
    }

    fn sync_paint(&mut self, cx: &mut Context<Self>) {
        let editor_open = self.edit.editor.clone();
        let overlays = self.overlay_paint_rows();
        let editor_input = self.cell_editor.clone();
        self.grid.update(cx, |state, cx| {
            let paint = &mut state.delegate_mut().edit_paint;
            paint.overlays = overlays;
            paint.editing = editor_open.as_ref().map(|ed| (ed.row.clone(), ed.col_ix));
            paint.editor_input = if editor_open.is_some() {
                Some(editor_input.clone())
            } else {
                None
            };
            paint.editor_error = editor_open.and_then(|ed| ed.validation_error);
            state.refresh(cx);
        });
    }

    fn begin_overlay_edit(
        &mut self,
        insert_id: u64,
        column: &str,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let server_value = self.edit.overlay_value(insert_id, column).cloned();
        self.edit
            .begin_edit(
                EditRowId::Overlay { insert_id },
                column.into(),
                col_ix,
                server_value.as_ref(),
                false,
                None,
                None,
            )
            .expect("begin edit");
        let draft = self
            .edit
            .editor
            .as_ref()
            .map(|e| e.draft.clone())
            .unwrap_or_default();
        self.cell_editor.update(cx, |input, cx| {
            input.set_value(draft, window, cx);
        });
        self.cell_editor.read(cx).focus_handle(cx).focus(window, cx);
        self.sync_paint(cx);
        cx.notify();
    }
}

impl Render for GridEditHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        div()
            .size_full()
            .id("grid-edit-host")
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &InspectCell, _, cx| {
                this.cell_inspect = Some(CellInspect {
                    column: "note".into(),
                    type_name: "TEXT".into(),
                    body: "inspected".into(),
                    decode_error: false,
                    anchor_left: 0.,
                    anchor_top: 0.,
                });
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &AcceptCellEditor, _, cx| {
                let draft = this.cell_editor.read(cx).value().to_string();
                if let Some(ed) = &mut this.edit.editor {
                    if ed.forced_null && draft.is_empty() {
                    } else {
                        if ed.is_null && !draft.is_empty() {
                            ed.is_null = false;
                            ed.forced_null = false;
                        }
                        ed.draft = draft;
                    }
                }
                let _ = this.edit.accept_editor();
                this.sync_paint(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &DuplicateEditRow, _, cx| {
                // Same guard as Workspace: editor open → chord stays with the field.
                if !this.edit.row_chords_allowed() {
                    return;
                }
                let columns = vec![
                    ("id".into(), Value::Int(1)),
                    ("note".into(), Value::Null),
                    ("pages".into(), Value::Int(310)),
                ];
                let _ =
                    this.edit
                        .duplicate_server_row(&columns, &["id".into()], vec![Value::Int(1)]);
                this.sync_paint(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CopyAsInsert, _, cx| {
                let sql = {
                    let state = this.grid.read(cx);
                    let delegate = state.delegate();
                    let cells = delegate.copy_row_cells(1).expect("dup row");
                    let body = vec![vec![cells[1].clone(), cells[2].clone()]];
                    let cols = vec!["note".to_string(), "pages".to_string()];
                    format_selection(
                        &cols,
                        &body,
                        CopyFormat::Insert,
                        Some("public.books"),
                        Dialect::Postgres,
                    )
                    .expect("insert")
                };
                this.last_insert_sql = Some(sql.clone());
                cx.write_to_clipboard(ClipboardItem::new_string(sql));
                cx.notify();
            }))
            .child(Button::new("duplicate-row").label("Duplicate").on_click({
                let this = this.clone();
                move |_, _, cx| {
                    this.update(cx, |host, cx| {
                        let columns = vec![
                            ("id".into(), Value::Int(1)),
                            ("note".into(), Value::Null),
                            ("pages".into(), Value::Int(310)),
                        ];
                        let _ = host.edit.duplicate_server_row(
                            &columns,
                            &["id".into()],
                            vec![Value::Int(1)],
                        );
                        host.sync_paint(cx);
                        cx.notify();
                    });
                }
            }))
            .child(
                Button::new("copy-as-insert")
                    .label("Copy as INSERT")
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            this.update(cx, |host, cx| {
                                let sql = {
                                    let state = host.grid.read(cx);
                                    let delegate = state.delegate();
                                    let cells = delegate.copy_row_cells(1).expect("dup row");
                                    let body = vec![vec![cells[1].clone(), cells[2].clone()]];
                                    let cols = vec!["note".to_string(), "pages".to_string()];
                                    format_selection(
                                        &cols,
                                        &body,
                                        CopyFormat::Insert,
                                        Some("public.books"),
                                        Dialect::Postgres,
                                    )
                                    .expect("insert")
                                };
                                host.last_insert_sql = Some(sql.clone());
                                cx.write_to_clipboard(ClipboardItem::new_string(sql));
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(DataTable::new(&self.grid).stripe(true).bordered(true))
            .when_some(self.cell_inspect.clone(), |root, info| {
                root.child(
                    div()
                        .id("cell-inspect-popover")
                        .child(format!("{} · {}", info.column, info.body)),
                )
            })
    }
}

#[gpui_kit::test]
fn space_in_cell_editor_inserts_space_not_inspect(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(600.)), |window, cx| {
        cx.new(|cx| GridEditHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            let id = host.edit.insert_blank_row().expect("insert");
            host.begin_overlay_edit(id, "note", 1, window, cx);
        });
        window.render_frame(cx);
        window.click("cell-editor-input", cx);
        window.press("space", cx);
        window.render_frame(cx);
        assert_eq!(
            host.read(cx).cell_editor.read(cx).value().as_ref(),
            " ",
            "Space inside CellEditor must insert a character"
        );
        assert!(
            host.read(cx).cell_inspect.is_none(),
            "Space must not open the inspect popover while the editor is open"
        );
        assert!(window.try_find("cell-inspect-popover").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn empty_accept_on_unset_insert_cell_is_noop(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(600.)), |window, cx| {
        cx.new(|cx| GridEditHost::new(window, cx))
    });
    let insert_id = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            host.update(cx, |host, cx| {
                let id = host.edit.insert_blank_row().expect("insert");
                host.begin_overlay_edit(id, "note", 1, window, cx);
                id
            })
        })
        .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("cell-editor-input", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(
            host.read(cx).edit.editor.is_none(),
            "editor must close after accept"
        );
        assert!(
            host.read(cx)
                .edit
                .overlay_value(insert_id, "note")
                .is_none(),
            "empty accept on unset insert cell is a no-op (#77)"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn duplicate_then_copy_as_insert_keeps_null_and_int_unquoted(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(600.)), |window, cx| {
        cx.new(|cx| GridEditHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("duplicate-row", cx);
        window.render_frame(cx);
        window.click("copy-as-insert", cx);
        window.render_frame(cx);
        let sql = host
            .read(cx)
            .last_insert_sql
            .clone()
            .expect("Copy as INSERT should produce SQL");
        assert!(
            sql.contains("NULL") && !sql.contains("'NULL'"),
            "NULL must be unquoted: {sql}"
        );
        assert!(
            sql.contains("310") && !sql.contains("'310'"),
            "Int must be unquoted: {sql}"
        );
        let clip = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("clipboard");
        assert_eq!(clip, sql);
    })
    .unwrap();
}

#[gpui_kit::test]
fn duplicate_chord_is_noop_while_cell_editor_open(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(600.)), |window, cx| {
        cx.new(|cx| GridEditHost::new(window, cx))
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            let id = h.edit.insert_blank_row().unwrap();
            h.begin_overlay_edit(id, "note", 1, window, cx);
        });
        window.render_frame(cx);
        let before = host.read(cx).edit.overlay.len();
        window.dispatch_action(Box::new(DuplicateEditRow), cx);
        window.render_frame(cx);
        assert_eq!(
            host.read(cx).edit.overlay.len(),
            before,
            "Duplicate must no-op"
        );
        assert!(!host.read(cx).edit.row_chords_allowed());
        host.update(cx, |h, _| h.edit.cancel_editor());
        assert!(host.read(cx).edit.row_chords_allowed());
    })
    .unwrap();
}
