//! M3-3 headless UI: export dialog, import mapping / safe-mode, cancel copy.
//!
//! Paths are injected in tests; Browse uses native `prompt_for_*` (M-EX-01).
//! Never log passwords, URLs, or full row payloads.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{div, px, size, App, Context, Entity, SharedString, TestAppContext, Window};
use tokio_util::sync::CancellationToken;

use sela_core::{
    discard_export_partial, export_rows, ColumnStructure, Dialect, ExportFormat, IndexInfo,
    SafeMode, TableStructure, Value, EXPORT_CANCELLED_STATUS, IMPORT_REFUSED_READ_ONLY_STATUS,
};

use crate::ui::workspace::export_import::export_dialog::{
    ExportDialog, ExportDialogContext, ExportDialogEvent, ExportScope,
};
use crate::ui::workspace::export_import::import_dialog::{
    ImportDialog, ImportDialogContext, ImportDialogEvent,
};
use crate::ui::workspace::export_import::logic::{
    export_paths_to_discard_on_failure, export_temp_path, should_cancel_transfer_on_stop,
};
use crate::ui::workspace::export_import::messages::{
    export_progress_status, export_success_status, format_export_failure, format_import_failure,
    import_confirm_title, import_discard_pending_body, import_discard_pending_title,
    import_progress_status, EXPORT_ALREADY_RUNNING, WHOLE_TABLE_DISABLED_REASON,
};

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
            partial: false,
            expression: false,
            definition: None,
        }],
        foreign_keys: vec![],
        ddl: String::new(),
    }
}

/// Host that opens ExportDialog the same way the workspace does.
struct ExportHost {
    form: Entity<ExportDialog>,
    events: Arc<Mutex<Vec<&'static str>>>,
    _subscription: gpui_kit::Subscription,
}

impl ExportHost {
    fn new(table_data: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let ctx = ExportDialogContext {
            table_data,
            schema: Some("public".into()),
            table: Some("books".into()),
            dialect: Dialect::Postgres,
            sql_result_tab: !table_data,
        };
        let form = cx.new(|cx| ExportDialog::new(ctx, window, cx));
        let events_for_sub = events.clone();
        let subscription = cx.subscribe_in(&form, window, {
            let events = events_for_sub;
            move |_this, _, event, window, cx| match event {
                ExportDialogEvent::Cancel => {
                    events.lock().expect("events").push("cancel");
                    window.close_dialog(cx);
                }
                ExportDialogEvent::Export(_) => {
                    events.lock().expect("events").push("export");
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
        let form = self.form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(SharedString::from("Export"))
                .width(px(560.))
                .on_ok(|_, _, _| false)
                .child(form.clone())
        });
    }
}

impl Render for ExportHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().id("export-host").child("export host")
    }
}

async fn open_export_host(
    cx: &mut TestAppContext,
    table_data: bool,
) -> (gpui_kit::AnyWindowHandle, Entity<ExportHost>) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(700.)), |window, cx| {
        cx.new(|cx| ExportHost::new(table_data, window, cx))
    });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| host.show_dialog(window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(2), |window, _| {
        window.try_find("dialog").is_some()
    })
    .await;
    (handle, host)
}

/// EX-01 / EX-04: inject path; Whole table disabled on SQL-only context.
#[gpui_kit::test]
async fn ex01_ex04_export_dialog_path_and_whole_table_gate(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let (handle, host) = open_export_host(cx, false).await;
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            assert!(
                !host.form.read(cx).whole_table_enabled(),
                "SQL-only: Whole table disabled"
            );
            assert_eq!(
                host.form.read(cx).whole_table_disabled_reason(),
                Some(WHOLE_TABLE_DISABLED_REASON)
            );
            host.form.update(cx, |form, cx| {
                form.set_path(path.to_string_lossy().as_ref(), window, cx);
            });
            assert!(host.form.read(cx).test_path(cx).contains("out.csv"));
            assert_eq!(
                host.form.read(cx).test_format(),
                crate::ui::workspace::export_import::export_dialog::ExportFormatChoice::Csv
            );
        });
        window.render_frame(cx);
        window.within("dialog").click("export-dialog-export", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let events = host.read_with(cx, |h, _| h.events.lock().unwrap().clone());
    assert_eq!(events, vec!["export"]);
}

/// EX-04: Whole table enabled on table Data.
#[gpui_kit::test]
async fn ex04_whole_table_enabled_on_table_data(cx: &mut TestAppContext) {
    let (handle, host) = open_export_host(cx, true).await;
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            assert!(host.form.read(cx).whole_table_enabled());
            host.form.update(cx, |form, cx| {
                // Switch scope via button.
                form.set_path("/tmp/x.csv", window, cx);
            });
        });
        window.render_frame(cx);
        window.within("dialog").click("export-scope-whole", cx);
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            assert_eq!(host.form.read(cx).test_scope(), ExportScope::WholeTable);
        });
    })
    .unwrap();
}

/// EX-05 / EX-13: progress copy and concurrent refusal string.
#[test]
fn ex05_ex13_progress_and_already_running_copy() {
    let s = export_progress_status(4, Some(10), Duration::from_millis(2500));
    assert!(s.contains("Exporting · 4 / 10 rows written"));
    assert_eq!(EXPORT_ALREADY_RUNNING, "An export is already running.");
    let path = PathBuf::from("/tmp/books.csv");
    let ok = export_success_status(4, &path, 40);
    assert!(ok.contains("Exported 4 rows"));
    assert!(ok.contains("/tmp/books.csv"));
}

/// EX-06: cancel deletes only the temp sibling; destination stays byte-identical.
#[test]
fn ex06_cancel_deletes_temp_keeps_destination() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("keep.csv");
    let original = b"id,title\n1,a\n";
    fs::write(&dest, original).unwrap();
    let tmp = export_temp_path(&dest);
    fs::write(&tmp, b"partial").unwrap();
    for path in export_paths_to_discard_on_failure(&tmp) {
        discard_export_partial(&path).unwrap();
    }
    assert!(!tmp.exists());
    assert_eq!(fs::read(&dest).unwrap(), original);
    let (b, s) = format_export_failure(&sela_core::ExportError::Cancelled);
    assert_eq!(b, EXPORT_CANCELLED_STATUS);
    assert_eq!(s, EXPORT_CANCELLED_STATUS);
}

/// Stop cancels a transfer only when the active tab has no running query.
#[test]
fn ex06_stop_routing_skips_transfer_when_query_running() {
    assert!(should_cancel_transfer_on_stop(true, false));
    assert!(!should_cancel_transfer_on_stop(true, true));
}

/// EX-01 capped note + streaming write of injected rows (no hang).
#[test]
fn ex01_export_loaded_rows_to_injected_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.csv");
    let mut file = fs::File::create(&path).unwrap();
    let columns = vec!["id".into(), "title".into()];
    let rows = vec![
        vec![Value::Int(1), Value::text("Hobbit")],
        vec![Value::Null, Value::text("")],
    ];
    let cancel = CancellationToken::new();
    let n = export_rows(
        &mut file,
        &columns,
        rows,
        &ExportFormat::Csv(Default::default()),
        &cancel,
        |_| {},
    )
    .unwrap();
    assert_eq!(n, 2);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("id,title"));
    assert!(text.contains("Hobbit"));
    // NULL default empty field; empty string as "".
    assert!(text.contains(",\"\"") || text.lines().any(|l| l.contains(",\"\"")));
}

struct ImportHost {
    form: Entity<ImportDialog>,
    events: Arc<Mutex<Vec<&'static str>>>,
    _subscription: gpui_kit::Subscription,
}

impl ImportHost {
    fn new(read_only: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let ctx = ImportDialogContext {
            schema: "public".into(),
            table: "books".into(),
            structure: books_structure(),
            read_only,
        };
        let form = cx.new(|cx| ImportDialog::new(ctx, window, cx));
        let events_for_sub = events.clone();
        let subscription = cx.subscribe_in(&form, window, {
            let events = events_for_sub;
            move |_this, _, event, window, cx| match event {
                ImportDialogEvent::Cancel => {
                    events.lock().expect("events").push("cancel");
                    window.close_dialog(cx);
                }
                ImportDialogEvent::Ready(_) => {
                    events.lock().expect("events").push("ready");
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
        let form = self.form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(SharedString::from("Import CSV"))
                .width(px(560.))
                .on_ok(|_, _, _| false)
                .child(form.clone())
        });
    }
}

impl Render for ImportHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().id("import-host").child("import host")
    }
}

async fn open_import_host(
    cx: &mut TestAppContext,
    read_only: bool,
) -> (gpui_kit::AnyWindowHandle, Entity<ImportHost>) {
    let (handle, host) = super::harness::open_window(cx, size(px(900.), px(800.)), |window, cx| {
        cx.new(|cx| ImportHost::new(read_only, window, cx))
    });
    let handle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| host.show_dialog(window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(2), |window, _| {
        window.try_find("dialog").is_some()
    })
    .await;
    (handle, host)
}

/// IM-01 / IM-03: inject path, preview headers, case-insensitive auto-map, pre-scan count.
#[gpui_kit::test]
async fn im01_im03_import_preview_and_mapping(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.csv");
    // Header case differs; third column unmatched → Skip.
    fs::write(&path, "ID,Title,extra\n1,A,x\n2,B,y\n3,C,z\n").unwrap();
    let (handle, host) = open_import_host(cx, false).await;
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.form.update(cx, |form, cx| {
                form.set_path_and_load(path.to_string_lossy().as_ref(), window, cx);
            });
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            let form = host.form.read(cx);
            let headers = form.test_preview_headers().unwrap();
            assert_eq!(
                headers,
                &["ID".to_string(), "Title".to_string(), "extra".into()]
            );
            let mapping = form.test_mapping().unwrap();
            assert_eq!(mapping[0].as_deref(), Some("id"));
            assert_eq!(mapping[1].as_deref(), Some("title"));
            assert_eq!(mapping[2], None); // Skip
            assert_eq!(form.test_total_rows(), Some(3));
            assert!(form.test_path(cx).contains("in.csv"));
            assert!(form.import_enabled());
        });
        window.within("dialog").click("import-dialog-import", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let events = host.read_with(cx, |h, _| h.events.lock().unwrap().clone());
    assert_eq!(events, vec!["ready"]);
}

/// IM-03: required column left unmapped blocks Import.
#[gpui_kit::test]
async fn im03_required_unmapped_blocks_import(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.csv");
    // No id column — required id stays unmapped.
    fs::write(&path, "title\nA\n").unwrap();
    let (handle, host) = open_import_host(cx, false).await;
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            host.form.update(cx, |form, cx| {
                form.set_path_and_load(path.to_string_lossy().as_ref(), window, cx);
            });
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            let form = host.form.read(cx);
            assert!(!form.import_enabled());
            assert_eq!(
                form.test_mapping_blocked(),
                Some("Map every required column or cancel.")
            );
        });
    })
    .unwrap();
}

/// IM-05: Read-only disables import with the specified copy.
#[gpui_kit::test]
async fn im05_read_only_disables_import(cx: &mut TestAppContext) {
    let (handle, host) = open_import_host(cx, true).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            assert!(!host.form.read(cx).import_enabled());
        });
        // Load should refuse.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.csv");
        fs::write(&path, "id,title\n1,A\n").unwrap();
        host.update(cx, |host, cx| {
            host.form.update(cx, |form, cx| {
                form.set_path_and_load(path.to_string_lossy().as_ref(), window, cx);
            });
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, _, cx| {
        host.update(cx, |host, cx| {
            assert_eq!(
                host.form.read(cx).test_error(),
                Some(IMPORT_REFUSED_READ_ONLY_STATUS)
            );
        });
    })
    .unwrap();
    let _ = SafeMode::ReadOnly; // document IM-05 gate uses SafeMode
}

/// IM-05 confirm title uses pre-scan N; IM-09 pending copy.
#[test]
fn im05_im09_confirm_copy() {
    assert_eq!(
        import_confirm_title(250, "public", "books"),
        "Import 250 rows into public.books?"
    );
    assert_eq!(
        import_discard_pending_title(),
        "Import and discard pending edits?"
    );
    let body = import_discard_pending_body(2, "public", "books");
    assert!(body.contains("2 pending changes on public.books"));
}

/// IM-06 / IM-07: row error and progress copy (no row payload in status).
#[test]
fn im06_im07_error_and_progress_copy() {
    let (b, _) = format_import_failure(&sela_core::ImportError::Row {
        line: 12,
        message: "invalid integer".into(),
    });
    assert!(b.contains("row 12"));
    assert!(b.contains("No rows were written."));
    assert!(!b.contains("password"));
    let s = import_progress_status(5, 100, Duration::from_secs(1));
    assert_eq!(s, "Importing · 5 / 100 rows · 1.0 s");
}

/// EX-06 status copy for a cancelled transfer (real discard path is in jobs e2e).
#[test]
fn ex06_cancelled_status_copy() {
    assert_eq!(EXPORT_CANCELLED_STATUS, "Export cancelled.");
}
