//! Long-value popover editor (#72, v0.6.0): open, accept, cancel.

use gpui_kit::component::input::{EditorState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    div, px, size, AppContext, Context, ElementId, Entity, Focusable, TestAppContext, Window,
};
use sela_core::{
    ColumnKind, ColumnMeta, ColumnStructure, Dialect, IndexInfo, QueryPage, RelationKind,
    TableStructure, Value,
};

use crate::ui::long_value_popover::{long_value_popover, LongValuePopover, LongValuePopoverInput};
use crate::ui::workspace::edit_session::{EditRowId, EditSession};
use crate::ui::workspace::Workspace;
use crate::ui::{AcceptCellEditor, CancelCellEditor};

fn col(name: &str, type_name: &str, nullable: bool) -> ColumnStructure {
    ColumnStructure {
        name: name.into(),
        type_name: type_name.into(),
        nullable,
        default: None,
        comment: None,
        generated: false,
    }
}

fn structure() -> TableStructure {
    TableStructure {
        schema: "public".into(),
        table: "docs".into(),
        columns: vec![
            col("id", "integer", false),
            col("body", "text", true),
            col("meta", "jsonb", true),
        ],
        indexes: vec![IndexInfo {
            name: "docs_pkey".into(),
            columns: vec!["id".into()],
            unique: true,
            primary: true,
            definition: None,
        }],
        foreign_keys: vec![],
        ddl: String::new(),
    }
}

struct PopoverHost {
    edit: EditSession,
    textarea: Entity<TextareaState>,
    json_editor: Entity<EditorState>,
    focus: gpui_kit::FocusHandle,
}

impl PopoverHost {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            edit: EditSession::from_structure(&structure(), Dialect::Postgres, RelationKind::Table),
            textarea: cx.new(|cx| TextareaState::new(window, cx)),
            json_editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .auto_close(false)
                    .smart_indent(false)
            }),
            focus: cx.focus_handle(),
        }
    }

    fn popover_kind(&self) -> Option<ColumnKind> {
        self.edit
            .editor
            .as_ref()
            .filter(|e| e.popover)
            .map(|e| e.kind)
    }

    fn draft_entity_for_kind(&self, kind: ColumnKind) -> ElementId {
        match kind {
            ColumnKind::Json => ("input", self.json_editor.entity_id()).into(),
            _ => ("input", self.textarea.entity_id()).into(),
        }
    }

    /// Open the editor on a new insert row cell holding `value`.
    fn open(
        &mut self,
        column: &str,
        col_ix: usize,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let insert_id = self.edit.insert_blank_row().expect("insert");
        self.edit
            .begin_edit(
                EditRowId::Overlay { insert_id },
                column.into(),
                col_ix,
                Some(&value),
                false,
                None,
                None,
            )
            .expect("begin edit");
        let ed = self.edit.editor.as_ref().expect("editor");
        if ed.popover {
            let draft = ed.draft.clone();
            match ed.kind {
                ColumnKind::Json => {
                    self.json_editor
                        .update(cx, |input, cx| input.set_value(draft, window, cx));
                    self.json_editor.read(cx).focus_handle(cx).focus(window, cx);
                }
                _ => {
                    self.textarea
                        .update(cx, |input, cx| input.set_value(draft, window, cx));
                    self.textarea.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
        }
        cx.notify();
    }
}

impl Render for PopoverHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popover = self.edit.editor.as_ref().filter(|e| e.popover).map(|e| {
            let input = if matches!(e.kind, ColumnKind::Json) {
                LongValuePopoverInput::Json(&self.json_editor)
            } else {
                LongValuePopoverInput::Text(&self.textarea)
            };
            (
                input,
                LongValuePopover {
                    title: e.column.clone(),
                    error: e.validation_error.clone(),
                    anchor_left: 8.,
                    anchor_top: 8.,
                },
            )
        });
        div()
            .id("popover-host")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .on_action(cx.listener(|this, _: &AcceptCellEditor, _window, cx| {
                let draft = match this.popover_kind() {
                    Some(ColumnKind::Json) => this.json_editor.read(cx).value().to_string(),
                    Some(_) => this.textarea.read(cx).value().to_string(),
                    None => String::new(),
                };
                if let Some(ed) = &mut this.edit.editor {
                    ed.draft = draft;
                }
                let _ = this.edit.accept_editor();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CancelCellEditor, _, cx| {
                this.edit.cancel_editor();
                cx.notify();
            }))
            .when_some(popover, |root, (input, info)| {
                root.child(long_value_popover(input, info, cx))
            })
    }
}

fn host(
    cx: &mut TestAppContext,
) -> (
    gpui_kit::WindowHandle<gpui_kit::base::Root>,
    Entity<PopoverHost>,
) {
    super::harness::open_window(cx, size(px(900.), px(600.)), |window, cx| {
        cx.new(|cx| PopoverHost::new(window, cx))
    })
}

#[gpui_kit::test]
fn multiline_text_and_json_open_popover_but_single_line_does_not(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("a\nb"), window, cx)
        });
        window.render_frame(cx);
        assert!(
            window.try_find("long-value-accept").is_some(),
            "multi-line text"
        );
        host.update(cx, |h, _| h.edit.cancel_editor());
        window.render_frame(cx);
        assert!(window.try_find("long-value-accept").is_none());

        host.update(cx, |h, cx| {
            h.open("meta", 2, Value::json_raw("{\"a\":1}"), window, cx)
        });
        window.render_frame(cx);
        assert!(window.try_find("long-value-accept").is_some(), "json");
        host.update(cx, |h, _| h.edit.cancel_editor());

        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("one line"), window, cx)
        });
        window.render_frame(cx);
        assert!(
            window.try_find("long-value-accept").is_none(),
            "single-line stays inline"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn popover_accept_keeps_newlines_and_closes(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("a\nb"), window, cx)
        });
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.textarea.update(cx, |e, cx| {
                e.set_value("line one\nline two\nthree", window, cx)
            })
        });
        window.press("secondary-enter", cx);
        window.render_frame(cx);
        let h = host.read(cx);
        assert!(h.edit.editor.is_none(), "accept closes the popover");
        assert!(window.try_find("long-value-accept").is_none());
        let stored = h
            .edit
            .overlay
            .first()
            .map(|o| o.insert_id)
            .and_then(|id| h.edit.overlay_value(id, "body").cloned());
        assert_eq!(
            stored.map(|v| v.display().into_owned()).as_deref(),
            Some("line one\nline two\nthree")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn popover_enter_inserts_newline_and_invalid_json_stays_open(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("meta", 2, Value::json_raw("{\"a\":1}"), window, cx)
        });
        window.render_frame(cx);
        window.press("enter", cx);
        assert!(host.read(cx).edit.editor.is_some(), "Enter must not accept");
        assert!(
            host.read(cx).json_editor.read(cx).value().contains('\n'),
            "Enter adds a newline"
        );
        host.update(cx, |h, cx| {
            h.json_editor
                .update(cx, |e, cx| e.set_value("{oops", window, cx))
        });
        window.press("secondary-enter", cx);
        window.render_frame(cx);
        assert!(
            host.read(cx).edit.editor.is_some(),
            "invalid JSON keeps the popover open"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn popover_typing_stores_text_and_braces_as_typed(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("a\nb"), window, cx)
        });
        window.render_frame(cx);
        let target = host.update(cx, |h, _| h.draft_entity_for_kind(ColumnKind::Text));
        window.click(target, cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        window.input("it's", cx);
        assert_eq!(host.read(cx).textarea.read(cx).value(), "it's");

        host.update(cx, |h, _| h.edit.cancel_editor());
        window.render_frame(cx);

        host.update(cx, |h, cx| {
            h.open("meta", 2, Value::json_raw("{}"), window, cx)
        });
        window.render_frame(cx);
        let target = host.update(cx, |h, _| h.draft_entity_for_kind(ColumnKind::Json));
        window.click(target, cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        window.input("{", cx);
        assert_eq!(host.read(cx).json_editor.read(cx).value(), "{");
    })
    .unwrap();
}

#[gpui_kit::test]
fn popover_cancel_discards_draft(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("a\nb"), window, cx)
        });
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.textarea
                .update(cx, |e, cx| e.set_value("changed", window, cx))
        });
        window.press("escape", cx);
        window.render_frame(cx);
        let h = host.read(cx);
        assert!(h.edit.editor.is_none(), "escape closes the popover");
        let id = h.edit.overlay.first().map(|o| o.insert_id).unwrap();
        assert_eq!(
            h.edit
                .overlay_value(id, "body")
                .map(|v| v.display().into_owned()),
            None,
            "nothing stored"
        );
        assert!(window.try_find("long-value-accept").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn popover_cancel_button_closes(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("x\ny"), window, cx)
        });
        window.render_frame(cx);
        window.click("long-value-cancel", cx);
    })
    .unwrap();
    // `dispatch_action` is deferred; the action runs once the update returns.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(host.read(cx).edit.editor.is_none(), "Cancel button closes");
    })
    .unwrap();
}

#[gpui_kit::test]
fn begin_cell_edit_opens_popover_when_only_selection_anchor_is_set(cx: &mut TestAppContext) {
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let dir_path: PathBuf = dir.path().to_path_buf();
    let structure = structure();
    let page = QueryPage {
        columns: vec![
            ColumnMeta {
                name: "id".into(),
                type_name: "INTEGER".into(),
            },
            ColumnMeta {
                name: "body".into(),
                type_name: "TEXT".into(),
            },
            ColumnMeta {
                name: "meta".into(),
                type_name: "JSONB".into(),
            },
        ],
        rows: vec![vec![
            Value::Int(1),
            Value::text("line one\nline two"),
            Value::json_raw("{\"a\":1}"),
        ]],
        duration: Duration::from_millis(0),
    };
    let (handle, workspace) =
        super::harness::open_window(cx, size(px(900.), px(600.)), |window, cx| {
            cx.new(|cx| Workspace::new_for_tests(&dir_path, window, cx))
        });
    cx.update_window(handle.into(), |_, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.test_new_query_tab(window, cx);
            ws.test_seed_editable_table(structure, page, cx);
        });
        window.render_frame(cx);

        for (col, label) in [(1usize, "multiline text"), (2, "JSON")] {
            let grid = workspace
                .read(cx)
                .test_active_grid()
                .expect("grid on table tab");
            grid.update(cx, |state, _| {
                // Highlight without kit cell selection — the QA failure path.
                state.delegate_mut().note_cell_focus(0, col, false);
            });
            grid.read(cx).focus_handle(cx).focus(window, cx);
            window.render_frame(cx);
            workspace.update(cx, |ws, cx| ws.test_begin_cell_edit(window, cx));
            window.render_frame(cx);
            assert!(
                workspace.read(cx).test_active_status().as_deref() != Some("Select a cell first."),
                "{label} must not fail cell resolution"
            );
            assert!(
                window.try_find("long-value-accept").is_some(),
                "{label} must open the popover"
            );
            window.press("escape", cx);
            window.render_frame(cx);
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn popover_accept_button_stores_value(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("x\ny"), window, cx)
        });
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.textarea
                .update(cx, |e, cx| e.set_value("p\nq", window, cx))
        });
        window.click("long-value-accept", cx);
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let h = host.read(cx);
        assert!(h.edit.editor.is_none());
        let id = h.edit.overlay.first().map(|o| o.insert_id).unwrap();
        assert_eq!(
            h.edit
                .overlay_value(id, "body")
                .map(|v| v.display().into_owned())
                .as_deref(),
            Some("p\nq")
        );
    })
    .unwrap();
}
