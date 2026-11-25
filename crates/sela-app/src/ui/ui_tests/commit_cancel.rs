//! #68 headless UI: optimistic preview (OW-04) and cancel-commit (CAN-01, CAN-03).
//!
//! Fixtures are injected in-memory (no native file dialogs / catalogs).

use gpui_kit::component::button::Button;
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{div, px, size, Context, TestAppContext, Window};
use tokio_util::sync::CancellationToken;

use sela_core::{
    ColumnStructure, DbError, Dialect, IndexInfo, RelationKind, TableStructure, Value,
};

use crate::ui::workspace::edit_session::{format_commit_failure, EditSession};

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
            ColumnStructure {
                name: "note".into(),
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

/// Mirrors the Commit / Stop cancel wiring in `edit_ui` + `stop_query` without a
/// full Workspace or live database (EditSession fixture injected in-memory).
struct CommitCancelHost {
    edit: EditSession,
    /// Same token the Commit path would register on the tab (CAN-01).
    cancel: Option<CancellationToken>,
    busy: bool,
    status: String,
    banner: Option<String>,
    preview: String,
}

impl CommitCancelHost {
    fn new() -> Self {
        let mut edit =
            EditSession::from_structure(&books_structure(), Dialect::Postgres, RelationKind::Table);
        let pk = vec![Value::Int(3)];
        edit.changeset
            .as_mut()
            .unwrap()
            .set_cell(&pk, "title", Value::text("Old"), Value::text("The Hobbit"))
            .unwrap();
        edit.changeset
            .as_mut()
            .unwrap()
            .set_cell(&pk, "note", Value::Null, Value::text("n"))
            .unwrap();
        Self {
            edit,
            cancel: None,
            busy: false,
            status: String::new(),
            banner: None,
            preview: String::new(),
        }
    }

    fn refresh_preview(&mut self) {
        self.preview = self.edit.preview_sheet_text().unwrap_or_else(|e| e);
    }

    /// Same registration shape as `perform_commit_changes` (token + busy).
    fn begin_commit_like_ui(&mut self) -> u64 {
        let generation = self.edit.begin_commit();
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel);
        self.busy = true;
        self.status = "Committing…".into();
        generation
    }

    /// Same as `stop_query` while committing: cancel the token.
    fn stop_like_ui(&mut self) {
        if !self.busy && !self.edit.committing {
            return;
        }
        if let Some(token) = self.cancel.as_ref() {
            token.cancel();
        }
        self.status = "Cancelling…".into();
    }

    /// Apply a cancelled `apply_changes` result the same way `edit_ui` does.
    fn finish_cancelled(&mut self, generation: u64) {
        let err = DbError::cancelled("apply cancelled");
        let (banner, status) = format_commit_failure(&err, 1);
        let _ = self.edit.note_commit_failure(generation);
        self.busy = false;
        self.cancel = None;
        self.banner = Some(banner);
        self.status = status;
    }
}

impl Render for CommitCancelHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        div()
            .size_full()
            .id("commit-cancel-host")
            .flex()
            .flex_col()
            .gap_2()
            .child(Button::new("preview-sql").label("Preview").on_click({
                let this = this.clone();
                move |_, _, cx| {
                    this.update(cx, |host, cx| {
                        host.refresh_preview();
                        cx.notify();
                    });
                }
            }))
            .child(Button::new("stop").label("Stop").on_click({
                let this = this.clone();
                move |_, _, cx| {
                    this.update(cx, |host, cx| {
                        host.stop_like_ui();
                        cx.notify();
                    });
                }
            }))
            .child(div().id("preview-body").child(self.preview.clone()))
            .child(div().id("status-line").child(self.status.clone()))
            .child(
                div()
                    .id("banner-line")
                    .child(self.banner.clone().unwrap_or_default()),
            )
    }
}

/// OW-04: Preview sheet lists optimistic non-PK predicates (injected EditSession).
#[gpui_kit::test]
fn ow04_preview_lists_optimistic_where(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(700.), px(400.)), |_, cx| {
        cx.new(|_cx| CommitCancelHost::new())
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("preview-sql", cx);
        window.render_frame(cx);
        let preview = host.read(cx).preview.clone();
        assert!(
            preview.contains("AND \"title\" =") && preview.contains("AND \"note\" IS NULL"),
            "OW-04 preview must list optimistic WHERE: {preview}"
        );
        assert!(preview.contains("The Hobbit"));
    })
    .unwrap();
}

/// CAN-01 + CAN-03: Commit path holds a CancellationToken; cancel is not success.
#[gpui_kit::test]
fn can01_can03_stop_cancels_commit_keeps_edits(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(700.), px(400.)), |_, cx| {
        cx.new(|_cx| CommitCancelHost::new())
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let generation = host.update(cx, |host, _| {
            let gen = host.begin_commit_like_ui();
            assert!(host.cancel.as_ref().is_some_and(|t| !t.is_cancelled()));
            assert!(host.busy);
            assert!(host.edit.committing);
            gen
        });
        window.click("stop", cx);
        window.render_frame(cx);
        host.update(cx, |host, _| {
            assert!(
                host.cancel.as_ref().is_some_and(|t| t.is_cancelled()),
                "CAN-01: Stop must cancel the commit CancellationToken"
            );
            assert_eq!(host.status, "Cancelling…");
            // Driver returns Cancelled → UI status (CAN-03).
            host.finish_cancelled(generation);
        });
        window.render_frame(cx);
        let host = host.read(cx);
        assert_eq!(host.status, "Commit cancelled · changes kept");
        assert_eq!(host.banner.as_deref(), Some("Commit cancelled."));
        assert!(!host.status.starts_with("Committed"));
        assert!(host.edit.is_dirty());
        assert!(host.edit.commit_failed);
        assert!(!host.edit.committing);
        assert!(!host.busy);
    })
    .unwrap();
}
