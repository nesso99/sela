use std::fs;

use chrono::{DateTime, TimeZone, Utc};
use sela_core::{
    should_record_history_sql, HistoryEntry, HistoryFilter, HistoryOutcome, HISTORY_ROW_CAP,
};

use super::HistoryStore;

fn sample(id: &str, conn: &str, sql: &str, at: DateTime<Utc>) -> HistoryEntry {
    HistoryEntry {
        id: id.into(),
        connection_id: conn.into(),
        connection_name: format!("name-{conn}"),
        database: "sample".into(),
        sql: sql.into(),
        started_at: at,
        duration_ms: 12,
        rows_label: "1 rows".into(),
        outcome: HistoryOutcome::Success,
        error_message: String::new(),
    }
}

#[test]
fn insert_query_and_mode_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite");
    let open = HistoryStore::open(&path).unwrap();
    assert!(!open.was_reset);
    let store = open.store;
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    store
        .insert(&sample("a", "c1", "select 1 from books", t0))
        .unwrap();
    store
        .insert(&sample(
            "b",
            "c2",
            "delete from books",
            t0 + chrono::Duration::seconds(1),
        ))
        .unwrap();

    let all = store.query(&HistoryFilter::default(), 100).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, "b");
    assert_eq!(all[1].id, "a");

    let scoped = store
        .query(
            &HistoryFilter {
                connection_id: Some("c1".into()),
                ..HistoryFilter::default()
            },
            100,
        )
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].sql, "select 1 from books");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn fts_search_returns_each_row_once_dedupe() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::open(dir.path().join("history.sqlite"))
        .unwrap()
        .store;
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    // Identical SQL on two connections must both appear; FTS join must not
    // multiply a single row.
    store
        .insert(&sample("a", "c1", "select title from books", t0))
        .unwrap();
    store
        .insert(&sample(
            "b",
            "c2",
            "select title from books",
            t0 + chrono::Duration::seconds(1),
        ))
        .unwrap();
    store
        .insert(&sample(
            "c",
            "c1",
            "select name from authors",
            t0 + chrono::Duration::seconds(2),
        ))
        .unwrap();

    let hits = store
        .query(
            &HistoryFilter {
                search: "books".into(),
                ..HistoryFilter::default()
            },
            100,
        )
        .unwrap();
    assert_eq!(hits.len(), 2);
    let mut ids: Vec<_> = hits.iter().map(|e| e.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["a", "b"]);
}

#[test]
fn retention_trims_oldest_past_global_cap() {
    assert_eq!(HISTORY_ROW_CAP, 10_000);
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::open(dir.path().join("history.sqlite"))
        .unwrap()
        .store;
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let total = HISTORY_ROW_CAP + 5;
    for i in 0..total {
        store
            .insert(&sample(
                &format!("id-{i}"),
                if i % 2 == 0 { "c1" } else { "c2" },
                &format!("select {i}"),
                t0 + chrono::Duration::milliseconds(i as i64),
            ))
            .unwrap();
    }
    assert_eq!(store.count(None).unwrap(), HISTORY_ROW_CAP);
    // Oldest five (id-0..id-4) are gone; newest remain.
    assert!(store.get("id-0").unwrap().is_none());
    assert!(store.get("id-4").unwrap().is_none());
    assert!(store
        .get(&format!("id-{}", HISTORY_ROW_CAP + 4))
        .unwrap()
        .is_some());
    assert!(should_record_history_sql("select 1"));
}

#[test]
fn corrupt_file_is_renamed_and_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite");
    fs::write(&path, b"not a sqlite database").unwrap();
    let open = HistoryStore::open(&path).unwrap();
    assert!(open.was_reset);
    assert!(path.with_extension("sqlite.bad").exists());
    open.store
        .insert(&sample(
            "x",
            "c",
            "select 1",
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        ))
        .unwrap();
    assert_eq!(open.store.count(None).unwrap(), 1);
}

#[test]
fn clear_scoped_and_delete_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::open(dir.path().join("history.sqlite"))
        .unwrap()
        .store;
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    store.insert(&sample("a", "c1", "select 1", t0)).unwrap();
    store
        .insert(&sample(
            "b",
            "c2",
            "select 2",
            t0 + chrono::Duration::seconds(1),
        ))
        .unwrap();
    store.clear(Some("c1")).unwrap();
    assert_eq!(store.count(None).unwrap(), 1);
    assert_eq!(store.count(Some("c2")).unwrap(), 1);
    store.delete("b").unwrap();
    assert_eq!(store.count(None).unwrap(), 0);
}

#[test]
fn checkpoint_and_close_flushes_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite");
    let store = HistoryStore::open(&path).unwrap().store;
    let t0 = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
    store.insert(&sample("ckpt", "c1", "select 1", t0)).unwrap();
    store.checkpoint().unwrap();
    // WAL companion files may still exist until close; after checkpoint+close
    // the main db must reopen cleanly with the row intact.
    store.checkpoint_and_close().unwrap();
    let reopened = HistoryStore::open(&path).unwrap().store;
    assert_eq!(reopened.count(None).unwrap(), 1);
    let rows = reopened.query(&HistoryFilter::default(), 10).unwrap();
    assert_eq!(rows[0].id, "ckpt");
}

#[test]
fn checkpoint_history_connection_helper() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite");
    let store = HistoryStore::open(&path).unwrap().store;
    let t0 = Utc.with_ymd_and_hms(2026, 3, 2, 0, 0, 0).unwrap();
    store
        .insert(&sample("helper", "c1", "select 2", t0))
        .unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    crate::checkpoint_history_connection(&conn).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn outcome_filter_and_no_secrets_in_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite");
    let store = HistoryStore::open(&path).unwrap().store;
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let mut err = sample("e", "c1", "select bad", t0);
    err.outcome = HistoryOutcome::Error;
    err.error_message = "relation missing".into();
    err.rows_label.clear();
    store.insert(&err).unwrap();
    store
        .insert(&sample(
            "ok",
            "c1",
            "select 1",
            t0 + chrono::Duration::seconds(1),
        ))
        .unwrap();
    let errors = store
        .query(
            &HistoryFilter {
                outcome: Some(HistoryOutcome::Error),
                ..HistoryFilter::default()
            },
            10,
        )
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error_message, "relation missing");

    let probe = rusqlite::Connection::open(&path).unwrap();
    let schema: String = probe
        .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name='history'")
        .unwrap()
        .query_row([], |row| row.get(0))
        .unwrap();
    let lower = schema.to_ascii_lowercase();
    assert!(!lower.contains("password"));
    assert!(!lower.contains("secret"));
    assert!(!lower.contains("keychain"));
}
