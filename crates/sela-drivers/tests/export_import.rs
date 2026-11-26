//! Live driver integration for M3-3 export/import (#127).
//!
//! Gating matches `db-integration`: Postgres/MySQL skip when env URLs are unset;
//! SQLite always runs. Plan ids from `docs/qa/v0.3-test-plan.md`.

#[path = "support/export_import.rs"]
mod support;

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sela_core::{
    build_table_select, discard_export_partial, CsvExportOptions, CsvImportOptions, ExportError,
    ExportFormat, FilterCondition, FilterOp, ImportError, JsonExportMode, JsonExportOptions,
    SortSpec, SqlInsertExportOptions,
};
use support::{
    all_engine_fixtures, create_exim_table, csv_data_row_count, driver_import_cancel_during_apply,
    driver_import_csv, drop_table, duplicate_pk_import_csv, export_to_vec, large_import_csv,
    row_count, seed_exim_many, seed_exim_sample, unique_table_name,
};
use tokio_util::sync::CancellationToken;

/// EX-01: stream a live session result as CSV on every configured engine.
#[tokio::test]
async fn ex01_live_session_csv_export_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        seed_exim_sample(fx.session(), &fx.schema, &table).await;

        let qual = sela_core::qualify(fx.dialect, &fx.schema, &table).unwrap();
        let sql = format!("SELECT id, title, note FROM {qual} ORDER BY id");
        let (n, bytes) = export_to_vec(
            fx.session(),
            &sql,
            &[],
            &ExportFormat::Csv(CsvExportOptions::default()),
            CancellationToken::new(),
        )
        .await
        .expect("csv export");
        assert_eq!(n, 3, "engine={}", fx.label);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("id,title,note\n"), "{}", fx.label);
        assert!(text.contains("a,b"), "{}", fx.label);
        assert!(text.contains("café"), "{}", fx.label);

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// EX-02: JSON array and NDJSON; SQL NULL → `null`.
#[tokio::test]
async fn ex02_live_session_json_export_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        seed_exim_sample(fx.session(), &fx.schema, &table).await;
        let qual = sela_core::qualify(fx.dialect, &fx.schema, &table).unwrap();
        let sql = format!("SELECT id, title, note FROM {qual} ORDER BY id");

        for (mode, label) in [
            (JsonExportMode::Array, "array"),
            (JsonExportMode::Ndjson, "ndjson"),
        ] {
            let (_, bytes) = export_to_vec(
                fx.session(),
                &sql,
                &[],
                &ExportFormat::Json(JsonExportOptions {
                    mode,
                    ..JsonExportOptions::default()
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|e| panic!("json {label} export {}: {e}", fx.label));
            let text = String::from_utf8(bytes).unwrap();
            assert!(
                text.contains(r#""note":null"#) || text.contains(r#""note": null"#),
                "engine={} mode={label}",
                fx.label
            );
            if mode == JsonExportMode::Ndjson {
                assert_eq!(
                    text.lines().filter(|l| !l.is_empty()).count(),
                    3,
                    "ndjson rows {}",
                    fx.label
                );
            } else {
                assert!(text.trim_start().starts_with('['), "{}", fx.label);
            }
        }

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// EX-03: SQL INSERT export honours dialect quoting and batches (100 rows).
#[tokio::test]
async fn ex03_live_session_sql_insert_export_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        seed_exim_many(fx.session(), &fx.schema, &table, 101).await;
        let qual = sela_core::qualify(fx.dialect, &fx.schema, &table).unwrap();
        let sql = format!("SELECT id, title, note FROM {qual} ORDER BY id");
        let opts = SqlInsertExportOptions::with_schema(fx.dialect, &fx.schema, &table);
        let (n, bytes) = export_to_vec(
            fx.session(),
            &sql,
            &[],
            &ExportFormat::SqlInsert(opts),
            CancellationToken::new(),
        )
        .await
        .expect("insert export");
        assert_eq!(n, 101, "{}", fx.label);
        let text = String::from_utf8(bytes).unwrap();
        let inserts = text.matches("INSERT INTO").count();
        assert!(
            inserts >= 2,
            "expected batched INSERT (EX-12), got {inserts} on {}",
            fx.label
        );
        assert!(
            text.contains(&table),
            "INSERT should name the table on {}",
            fx.label
        );

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// EX-04: whole-table export via `build_table_select` streams every filtered/sorted row.
#[tokio::test]
async fn ex04_whole_table_export_filtered_sorted_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        seed_exim_many(fx.session(), &fx.schema, &table, 120).await;

        let filters = vec![FilterCondition::new("id", FilterOp::Gt, "100".to_string())];
        let sorts = vec![SortSpec::desc("id")];
        let bound = build_table_select(fx.dialect, &fx.schema, &table, &filters, &sorts).unwrap();

        let expected = row_count(fx.session(), &fx.schema, &table).await;
        // Manual count for filtered set: ids 101..120 = 20 rows.
        assert_eq!(expected, 120);
        let (_, bytes) = export_to_vec(
            fx.session(),
            &bound.sql,
            &bound.params,
            &ExportFormat::Csv(CsvExportOptions::default()),
            CancellationToken::new(),
        )
        .await
        .expect("filtered export");
        let text = String::from_utf8(bytes).unwrap();
        let data_lines = text.lines().count().saturating_sub(1);
        assert_eq!(data_lines, 20, "filtered rows {}", fx.label);
        let first_line = text.lines().nth(1).unwrap_or("");
        assert!(
            first_line.starts_with("120,"),
            "sort DESC expected id 120 first on {}",
            fx.label
        );

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// EX-06: cancel mid-export on a live session; partial output is discarded.
#[tokio::test]
async fn ex06_cancel_mid_export_discards_partial_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        seed_exim_many(fx.session(), &fx.schema, &table, 80).await;
        let qual = sela_core::qualify(fx.dialect, &fx.schema, &table).unwrap();
        let sql = format!("SELECT id, title, note FROM {qual} ORDER BY id");

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("partial.csv");
        let tmp = dest.with_extension("sela-export-tmp");
        const DEST_SENTINEL: &[u8] = b"PREEXISTING_DEST_SENTINEL_BYTES_V1";
        fs::write(&dest, DEST_SENTINEL).unwrap();

        let cancel = CancellationToken::new();
        let cancel_rows = cancel.clone();
        let rows_written = Arc::new(AtomicUsize::new(0));
        let rows_written_cb = rows_written.clone();

        let file = std::fs::File::create(&tmp).unwrap();
        let mut writer = std::io::BufWriter::new(file);
        let err = sela_core::export_query(
            fx.session(),
            &sql,
            &[],
            &mut writer,
            &ExportFormat::Csv(CsvExportOptions::default()),
            cancel,
            |n| {
                rows_written_cb.store(n as usize, Ordering::Relaxed);
                if n >= 5 {
                    cancel_rows.cancel();
                }
            },
        )
        .await
        .expect_err("cancel expected");
        assert!(matches!(err, ExportError::Cancelled));
        let written = rows_written.load(Ordering::Relaxed);
        assert!((1..80).contains(&written), "partial export on {}", fx.label);
        drop(writer);
        assert!(tmp.exists(), "tmp partial must exist on {}", fx.label);
        let partial = fs::read(&tmp).unwrap();
        assert!(!partial.is_empty());
        discard_export_partial(&tmp).unwrap();
        assert!(!tmp.exists(), "tmp discarded on {}", fx.label);
        assert_eq!(
            fs::read(&dest).unwrap(),
            DEST_SENTINEL,
            "dest must stay byte-identical on {}",
            fx.label
        );

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// IM-01: import CSV into an existing table.
#[tokio::test]
async fn im01_import_csv_into_existing_table_all_engines() {
    let import_opts = CsvImportOptions::default();
    let csv = b"id,title,note\n10,Imported,\n11,Also,\n";
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;

        let n = driver_import_csv(
            fx.session(),
            fx.dialect,
            &fx.schema,
            &table,
            csv,
            import_opts.clone(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("import");
        assert_eq!(n, 2, "{}", fx.label);
        assert_eq!(row_count(fx.session(), &fx.schema, &table).await, 2);

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// IM-02: export CSV then import round-trip (NULL, quoting, Unicode).
#[tokio::test]
async fn im02_export_import_round_trip_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        seed_exim_sample(fx.session(), &fx.schema, &table).await;

        let qual = sela_core::qualify(fx.dialect, &fx.schema, &table).unwrap();
        let sql = format!("SELECT id, title, note FROM {qual} ORDER BY id");
        let export_opts = CsvExportOptions::default();
        let (_, bytes) = export_to_vec(
            fx.session(),
            &sql,
            &[],
            &ExportFormat::Csv(export_opts.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let dest = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &dest).await;
        let import_opts = CsvImportOptions::matching_export(&export_opts);
        driver_import_csv(
            fx.session(),
            fx.dialect,
            &fx.schema,
            &dest,
            &bytes,
            import_opts,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("round-trip import");

        let page = fx
            .session()
            .query(&format!(
                "SELECT id, title, note FROM {} ORDER BY id",
                sela_core::qualify(fx.dialect, &fx.schema, &dest).unwrap()
            ))
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 3, "{}", fx.label);
        assert!(page.rows[0][2].is_null());
        assert_eq!(page.rows[1][2].display(), "");

        drop_table(fx.session(), &fx.schema, &table).await;
        drop_table(fx.session(), &fx.schema, &dest).await;
    }
}

/// IM-04: cancel during `apply_changes` rolls back; table stays empty.
#[tokio::test]
async fn im04_cancel_import_single_transaction_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        let csv = large_import_csv(2500);

        let err = driver_import_cancel_during_apply(
            fx.session(),
            fx.dialect,
            &fx.schema,
            &table,
            &csv,
            CsvImportOptions::default(),
        )
        .await
        .expect_err("cancel import during apply");
        assert!(matches!(err, ImportError::Cancelled), "{}", fx.label);
        assert_eq!(
            row_count(fx.session(), &fx.schema, &table).await,
            0,
            "{}",
            fx.label
        );

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// IM-06: CSV parse type error reports line number; nothing applied.
#[tokio::test]
async fn im06_type_mismatch_reports_line_no_rows_written_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        let csv = b"id,title,note\n1,ok,\n2,ok,\nnot-int,bad,\n";
        let err = driver_import_csv(
            fx.session(),
            fx.dialect,
            &fx.schema,
            &table,
            csv,
            CsvImportOptions::default(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect_err("type error");
        match err {
            ImportError::Row { line, .. } => assert_eq!(line, 4, "{}", fx.label),
            other => panic!("expected Row error on {}, got {other:?}", fx.label),
        }
        assert_eq!(row_count(fx.session(), &fx.schema, &table).await, 0);

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// IM-06: duplicate PK fails inside `apply_changes`; full transaction rolled back.
#[tokio::test]
async fn im06_duplicate_pk_apply_rolls_back_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        let csv = duplicate_pk_import_csv();
        let err = driver_import_csv(
            fx.session(),
            fx.dialect,
            &fx.schema,
            &table,
            &csv,
            CsvImportOptions::default(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect_err("duplicate pk");
        assert!(
            matches!(err, ImportError::Sink(_)),
            "expected Sink/db error on {}, got {err:?}",
            fx.label
        );
        assert_eq!(
            row_count(fx.session(), &fx.schema, &table).await,
            0,
            "{}",
            fx.label
        );

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}

/// IM-07: pre-scan `{N}/{total}` matches data-row count; import in one transaction.
#[tokio::test]
async fn im07_prescan_count_matches_single_transaction_all_engines() {
    for fx in all_engine_fixtures().await {
        let table = unique_table_name();
        create_exim_table(fx.session(), &fx.schema, &table).await;
        let csv = b"id,title,note\n1,a,\n2,b,\n3,c,\n4,d,\n5,e,\n";
        let opts = CsvImportOptions::default();
        let prescan = csv_data_row_count(csv, &opts);
        assert_eq!(prescan, 5);

        let before = row_count(fx.session(), &fx.schema, &table).await;
        assert_eq!(before, 0);
        let imported = driver_import_csv(
            fx.session(),
            fx.dialect,
            &fx.schema,
            &table,
            csv,
            opts,
            CancellationToken::new(),
            |done| assert!(done <= prescan),
        )
        .await
        .unwrap();
        assert_eq!(imported, prescan);
        assert_eq!(
            row_count(fx.session(), &fx.schema, &table).await,
            before + prescan
        );

        drop_table(fx.session(), &fx.schema, &table).await;
    }
}
