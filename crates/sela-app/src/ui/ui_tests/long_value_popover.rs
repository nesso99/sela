//! Long-value popover editor (#72, v0.6.0): open, accept, cancel.

use gpui_kit::component::input::EditorState;
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{div, px, size, AppContext, Context, Entity, Focusable, TestAppContext, Window};

use sela_core::{ColumnStructure, Dialect, IndexInfo, RelationKind, TableStructure, Value};

use crate::ui::long_value_popover::{long_value_popover, LongValuePopover};
use crate::ui::workspace::edit_session::{EditRowId, EditSession};
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
    editor: Entity<EditorState>,
    focus: gpui_kit::FocusHandle,
}

impl PopoverHost {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            edit: EditSession::from_structure(&structure(), Dialect::Postgres, RelationKind::Table),
            editor: cx.new(|cx| EditorState::new(window, cx)),
            focus: cx.focus_handle(),
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
            self.editor
                .update(cx, |input, cx| input.set_value(draft, window, cx));
            self.editor.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }
}

impl Render for PopoverHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popover = self
            .edit
            .editor
            .as_ref()
            .filter(|e| e.popover)
            .map(|e| LongValuePopover {
                title: e.column.clone(),
                error: e.validation_error.clone(),
                anchor_left: 8.,
                anchor_top: 8.,
            });
        div()
            .id("popover-host")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .on_action(cx.listener(|this, _: &AcceptCellEditor, _, cx| {
                let draft = this.editor.read(cx).value().to_string();
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
            .when_some(popover, |root, info| {
                root.child(long_value_popover(&self.editor, info, cx))
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
            h.editor.update(cx, |e, cx| {
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
            host.read(cx).editor.read(cx).value().contains('\n'),
            "Enter adds a newline"
        );
        host.update(cx, |h, cx| {
            h.editor
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
fn popover_cancel_discards_draft(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("a\nb"), window, cx)
        });
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.editor
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
fn popover_accept_button_stores_value(cx: &mut TestAppContext) {
    let (handle, host) = host(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.open("body", 1, Value::text("x\ny"), window, cx)
        });
        window.render_frame(cx);
        host.update(cx, |h, cx| {
            h.editor.update(cx, |e, cx| e.set_value("p\nq", window, cx))
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
