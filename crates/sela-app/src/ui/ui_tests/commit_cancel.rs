//! #68 headless UI: optimistic preview (OW-04), cancel-commit (CAN-01, CAN-03),
//! and conflict Refresh (OW-05).
//!
//! Fixtures are injected in-memory (no native file dialogs / catalogs).

use gpui_kit::component::button::Button;
use gpui_kit::prelude::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{div, px, size, Context, TestAppContext, Window};
use tokio_util::sync::CancellationToken;

use sela_core::{
    ColumnStructure, DbError, Dialect, IndexInfo, RelationKind, TableRef, TableStructure, Value,
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
        let n = self.edit.pending_counts().total();
        let generation = self.edit.begin_commit();
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel);
        self.busy = true;
        self.status = format!("Committing {n} changes…");
        generation
    }

    /// Same as Stop / pending-bar Cancel while committing.
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
        let _ = self.edit.note_commit_failure(generation, false, true);
        self.busy = false;
        self.cancel = None;
        self.banner = Some(banner);
        self.status = status;
    }

    /// Apply a ChangeConflict the same way `edit_ui` does (OW-05).
    fn finish_conflict(&mut self, generation: u64) {
        let err =
            DbError::change_conflict(&TableRef::new("public", "books"), 0, 0, &[Value::Int(3)]);
        let (banner, status) = format_commit_failure(&err, 1);
        let _ = self.edit.note_commit_failure(generation, true, false);
        self.busy = false;
        self.cancel = None;
        self.banner = Some(banner);
        self.status = status;
    }

    /// OW-05 Refresh: drop ChangeSet (no confirm) and clear the banner.
    fn refresh_after_conflict(&mut self) {
        if !self.edit.commit_conflict {
            return;
        }
        self.edit.discard();
        self.banner = None;
        self.status = "Refreshed.".into();
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
            .child(Button::new("pending-cancel").label("Cancel").on_click({
                let this = this.clone();
                move |_, _, cx| {
                    this.update(cx, |host, cx| {
                        host.stop_like_ui();
                        cx.notify();
                    });
                }
            }))
            .child(Button::new("conflict-refresh").label("Refresh").on_click({
                let this = this.clone();
                move |_, _, cx| {
                    this.update(cx, |host, cx| {
                        host.refresh_after_conflict();
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
            assert!(
                host.status.starts_with("Committing ") && host.status.ends_with(" changes…"),
                "CAN-01 status: {}",
                host.status
            );
            gen
        });
        window.click("pending-cancel", cx);
        window.render_frame(cx);
        host.update(cx, |host, _| {
            assert!(
                host.cancel.as_ref().is_some_and(|t| t.is_cancelled()),
                "CAN-01: pending Cancel must cancel the commit CancellationToken"
            );
            assert_eq!(host.status, "Cancelling…");
            host.finish_cancelled(generation);
        });
        window.render_frame(cx);
        let host = host.read(cx);
        assert_eq!(host.status, "Commit cancelled. No changes were saved.");
        assert_eq!(
            host.banner.as_deref(),
            Some("Commit cancelled. No changes were saved.")
        );
        assert!(!host.status.starts_with("Committed"));
        assert!(host.edit.is_dirty());
        assert!(host.edit.commit_cancelled);
        assert!(!host.edit.commit_failed);
        assert!(!host.edit.committing);
        assert!(!host.busy);
    })
    .unwrap();
}

/// OW-05: conflict banner copy + Refresh drops pending edits.
#[gpui_kit::test]
fn ow05_conflict_refresh_drops_pending(cx: &mut TestAppContext) {
    let (handle, host) = super::harness::open_window(cx, size(px(700.), px(400.)), |_, cx| {
        cx.new(|_cx| CommitCancelHost::new())
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let generation = host.update(cx, |host, _| host.begin_commit_like_ui());
        host.update(cx, |host, _| host.finish_conflict(generation));
        window.render_frame(cx);
        {
            let host = host.read(cx);
            let primary =
                "Row changed since it was loaded. No changes were saved. Refresh and try again.";
            assert_eq!(host.status, primary);
            assert!(
                host.banner
                    .as_deref()
                    .is_some_and(|b| b.starts_with(primary)),
                "OW-05 banner: {:?}",
                host.banner
            );
            assert!(host.edit.commit_conflict);
            assert!(host.edit.is_dirty());
        }
        window.click("conflict-refresh", cx);
        window.render_frame(cx);
        let host = host.read(cx);
        assert!(!host.edit.is_dirty());
        assert!(!host.edit.commit_conflict);
        assert!(host.banner.is_none());
    })
    .unwrap();
}
