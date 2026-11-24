//! Persist query history in `history.sqlite` (FTS5 on statement text).
//!
//! Never stores passwords. The file is created with Unix mode `0600`. SQLite
//! journaling is the durability path; a corrupt file is renamed to
//! `history.sqlite.bad` and replaced with an empty store.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use sela_core::{history_fts_query, HistoryEntry, HistoryFilter, HistoryOutcome, HISTORY_ROW_CAP};
use uuid::Uuid;

use crate::store::{config_dir, ConfigError};

/// Result of opening the store, including a one-shot reset notice.
#[derive(Debug)]
pub struct HistoryOpen {
    pub store: HistoryStore,
    /// Set when a corrupt file was renamed to `.bad` and a fresh store created.
    pub was_reset: bool,
}

#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl HistoryStore {
    pub fn default_path() -> PathBuf {
        config_dir().join("history.sqlite")
    }

    pub fn open_default() -> Result<HistoryOpen, ConfigError> {
        Self::open(Self::default_path())
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<HistoryOpen, ConfigError> {
        let path = path.into();
        match Self::open_fresh(&path) {
            Ok(store) => Ok(HistoryOpen {
                store,
                was_reset: false,
            }),
            Err(first) => {
                if !path.exists() {
                    return Err(first);
                }
                let bad = path.with_extension("sqlite.bad");
                let _ = fs::remove_file(&bad);
                fs::rename(&path, &bad).map_err(|source| ConfigError::Write {
                    path: bad.clone(),
                    source,
                })?;
                let store = Self::open_fresh(&path)?;
                Ok(HistoryOpen {
                    store,
                    was_reset: true,
                })
            }
        }
    }

    fn open_fresh(path: &Path) -> Result<Self, ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        if !path.exists() {
            create_private_file(path).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        let conn = Connection::open(path).map_err(|source| ConfigError::History {
            path: path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS history (
                rowid INTEGER PRIMARY KEY,
                id TEXT NOT NULL UNIQUE,
                connection_id TEXT NOT NULL,
                connection_name TEXT NOT NULL,
                database TEXT NOT NULL,
                sql TEXT NOT NULL,
                started_at TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                rows_label TEXT NOT NULL,
                outcome TEXT NOT NULL,
                error_message TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS history_conn_started
                ON history(connection_id, started_at);
            CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(
                sql,
                content='history',
                content_rowid='rowid',
                tokenize='unicode61'
            );
            CREATE TRIGGER IF NOT EXISTS history_ai AFTER INSERT ON history BEGIN
                INSERT INTO history_fts(rowid, sql) VALUES (new.rowid, new.sql);
            END;
            CREATE TRIGGER IF NOT EXISTS history_ad AFTER DELETE ON history BEGIN
                INSERT INTO history_fts(history_fts, rowid, sql)
                    VALUES('delete', old.rowid, old.sql);
            END;
            CREATE TRIGGER IF NOT EXISTS history_au AFTER UPDATE OF sql ON history BEGIN
                INSERT INTO history_fts(history_fts, rowid, sql)
                    VALUES('delete', old.rowid, old.sql);
                INSERT INTO history_fts(rowid, sql) VALUES (new.rowid, new.sql);
            END;
            ",
        )
        .map_err(|source| ConfigError::History {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            conn: Mutex::new(conn),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Insert one row and trim to [`HISTORY_ROW_CAP`] in the same transaction.
    pub fn insert(&self, entry: &HistoryEntry) -> Result<(), ConfigError> {
        let conn = self.conn.lock().expect("history mutex");
        let tx = conn
            .unchecked_transaction()
            .map_err(|source| ConfigError::History {
                path: self.path.clone(),
                source,
            })?;
        tx.execute(
            "INSERT INTO history (
                id, connection_id, connection_name, database, sql,
                started_at, duration_ms, rows_label, outcome, error_message
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.id,
                entry.connection_id,
                entry.connection_name,
                entry.database,
                entry.sql,
                entry.started_at.to_rfc3339(),
                entry.duration_ms as i64,
                entry.rows_label,
                entry.outcome.as_str(),
                entry.error_message,
            ],
        )
        .map_err(|source| ConfigError::History {
            path: self.path.clone(),
            source,
        })?;
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))
            .map_err(|source| ConfigError::History {
                path: self.path.clone(),
                source,
            })?;
        let overflow = count - HISTORY_ROW_CAP as i64;
        if overflow > 0 {
            tx.execute(
                "DELETE FROM history WHERE rowid IN (
                    SELECT rowid FROM history ORDER BY started_at ASC, rowid ASC LIMIT ?1
                )",
                params![overflow],
            )
            .map_err(|source| ConfigError::History {
                path: self.path.clone(),
                source,
            })?;
        }
        tx.commit().map_err(|source| ConfigError::History {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn new_entry_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// Newest first. FTS + outcome/scope filters. Each row id appears once.
    pub fn query(
        &self,
        filter: &HistoryFilter,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, ConfigError> {
        let conn = self.conn.lock().expect("history mutex");
        let fts = history_fts_query(&filter.search);
        let limit = limit.min(HISTORY_ROW_CAP);
        let mut sql = String::from(
            "SELECT h.id, h.connection_id, h.connection_name, h.database, h.sql,
                    h.started_at, h.duration_ms, h.rows_label, h.outcome, h.error_message
             FROM history h",
        );
        if fts.is_some() {
            sql.push_str(" JOIN history_fts f ON f.rowid = h.rowid");
        }
        sql.push_str(" WHERE 1=1");
        if filter.connection_id.is_some() {
            sql.push_str(" AND h.connection_id = ?");
        }
        if filter.outcome.is_some() {
            sql.push_str(" AND h.outcome = ?");
        }
        if fts.is_some() {
            sql.push_str(" AND history_fts MATCH ?");
        }
        sql.push_str(" ORDER BY h.started_at DESC, h.rowid DESC LIMIT ?");

        let mut stmt = conn.prepare(&sql).map_err(|source| ConfigError::History {
            path: self.path.clone(),
            source,
        })?;

        let mut bind_ix = 1;
        if let Some(id) = &filter.connection_id {
            stmt.raw_bind_parameter(bind_ix, id)
                .map_err(|source| ConfigError::History {
                    path: self.path.clone(),
                    source,
                })?;
            bind_ix += 1;
        }
        if let Some(outcome) = filter.outcome {
            stmt.raw_bind_parameter(bind_ix, outcome.as_str())
                .map_err(|source| ConfigError::History {
                    path: self.path.clone(),
                    source,
                })?;
            bind_ix += 1;
        }
        if let Some(match_q) = &fts {
            stmt.raw_bind_parameter(bind_ix, match_q)
                .map_err(|source| ConfigError::History {
                    path: self.path.clone(),
                    source,
                })?;
            bind_ix += 1;
        }
        stmt.raw_bind_parameter(bind_ix, limit as i64)
            .map_err(|source| ConfigError::History {
                path: self.path.clone(),
                source,
            })?;

        let rows = stmt
            .raw_query()
            .mapped(map_entry)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| ConfigError::History {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows)
    }

    pub fn get(&self, id: &str) -> Result<Option<HistoryEntry>, ConfigError> {
        let conn = self.conn.lock().expect("history mutex");
        conn.query_row(
            "SELECT id, connection_id, connection_name, database, sql,
                    started_at, duration_ms, rows_label, outcome, error_message
             FROM history WHERE id = ?1",
            params![id],
            map_entry,
        )
        .optional()
        .map_err(|source| ConfigError::History {
            path: self.path.clone(),
            source,
        })
    }

    pub fn delete(&self, id: &str) -> Result<(), ConfigError> {
        let conn = self.conn.lock().expect("history mutex");
        conn.execute("DELETE FROM history WHERE id = ?1", params![id])
            .map_err(|source| ConfigError::History {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    pub fn clear(&self, connection_id: Option<&str>) -> Result<(), ConfigError> {
        let conn = self.conn.lock().expect("history mutex");
        match connection_id {
            Some(id) => {
                conn.execute("DELETE FROM history WHERE connection_id = ?1", params![id])
                    .map_err(|source| ConfigError::History {
                        path: self.path.clone(),
                        source,
                    })?;
            }
            None => {
                conn.execute("DELETE FROM history", [])
                    .map_err(|source| ConfigError::History {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }
        Ok(())
    }

    pub fn count(&self, connection_id: Option<&str>) -> Result<usize, ConfigError> {
        let conn = self.conn.lock().expect("history mutex");
        let n: i64 = match connection_id {
            Some(id) => conn
                .query_row(
                    "SELECT COUNT(*) FROM history WHERE connection_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .map_err(|source| ConfigError::History {
                    path: self.path.clone(),
                    source,
                })?,
            None => conn
                .query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))
                .map_err(|source| ConfigError::History {
                    path: self.path.clone(),
                    source,
                })?,
        };
        Ok(n as usize)
    }
}

fn map_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let started_raw: String = row.get(5)?;
    let started_at = DateTime::parse_from_rfc3339(&started_raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(err))
        })?;
    let outcome_raw: String = row.get(8)?;
    let outcome = HistoryOutcome::parse(&outcome_raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown history outcome: {outcome_raw}"),
            )),
        )
    })?;
    Ok(HistoryEntry {
        id: row.get(0)?,
        connection_id: row.get(1)?,
        connection_name: row.get(2)?,
        database: row.get(3)?,
        sql: row.get(4)?,
        started_at,
        duration_ms: row.get::<_, i64>(6)? as u64,
        rows_label: row.get(7)?,
        outcome,
        error_message: row.get(9)?,
    })
}

fn create_private_file(path: &Path) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let _ = options.open(path)?;
    Ok(())
}
