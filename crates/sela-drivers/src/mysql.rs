use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::MySqlPool;

use tokio_util::sync::CancellationToken;

use sela_core::{
    quote_ident, ApplyChangesResult, ChangePlan, ColumnStructure, DatabaseSession, DbError,
    Dialect, ForeignKeyInfo, IndexInfo, QueryPage, RelationKind, SafeMode, SavedConnection,
    SchemaInfo, StreamBatch, StreamOutcome, TableInfo, TableStructure,
};

use sqlx::Row;

pub struct MysqlSession {
    pool: MySqlPool,
    /// Keeps the local SSH forward alive for the session lifetime (SSH-10).
    _ssh_tunnel: Option<sela_ssh::SshTunnel>,
}

impl MysqlSession {
    pub async fn connect(connection: &SavedConnection, password: &str) -> Result<Self, DbError> {
        Self::connect_with_tunnel(connection, password, None, None).await
    }

    /// When `dial` is set (SSH tunnel), TCP goes to loopback while
    /// [`SavedConnection::host`] is used for TLS SNI / hostname verification.
    pub(crate) async fn connect_with_tunnel(
        connection: &SavedConnection,
        password: &str,
        dial: Option<SocketAddr>,
        ssh_tunnel: impl Into<Option<sela_ssh::SshTunnel>>,
    ) -> Result<Self, DbError> {
        let mut options = MySqlConnectOptions::new()
            .host(connection.host.trim())
            .port(connection.port)
            .username(connection.username.trim())
            .password(password)
            .database(connection.database.trim())
            // TIMESTAMP has no offset on the wire; sqlx's DateTime<Utc> decode assumes the
            // session time_zone is UTC. Pin +00:00 so TIMESTAMP -> TimestampTz stays consistent.
            .timezone(Some("+00:00".into()))
            // CLIENT_FOUND_ROWS: rows_affected = matched rows (sqlx 0.9.1 default is
            // true). Required so same-value UPDATE still reports 1 for the
            // change-plan exact-1-row guard — see change_exec.rs.
            .found_rows(true);
        if let Some(addr) = dial {
            let logical_host = connection.host.trim().to_string();
            options = options
                .host(&addr.ip().to_string())
                .port(addr.port())
                .tls_sni_servername(&logical_host);
        }
        let options = crate::tls::apply_mysql_tls(options, &connection.tls)?;
        let pool = Self::pool_with_safe_mode(options, connection.safe_mode).await?;
        Ok(Self {
            pool,
            _ssh_tunnel: ssh_tunnel.into(),
        })
    }

    async fn pool_with_safe_mode(
        options: MySqlConnectOptions,
        safe_mode: SafeMode,
    ) -> Result<MySqlPool, DbError> {
        let read_only = safe_mode == SafeMode::ReadOnly;
        let mut builder = MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10));
        if read_only {
            builder = builder.after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("SET SESSION TRANSACTION READ ONLY")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            });
        }
        builder.connect_with(options).await.map_err(crate::sqlx_err)
    }

    #[cfg(test)]
    pub async fn connect_url(url: &str) -> Result<Self, DbError> {
        Self::connect_url_with_safe_mode(url, SafeMode::Off).await
    }

    #[cfg(test)]
    pub async fn connect_url_with_safe_mode(
        url: &str,
        safe_mode: SafeMode,
    ) -> Result<Self, DbError> {
        let options: MySqlConnectOptions = url
            .parse()
            .map_err(|err| DbError::config(format!("invalid MySQL URL: {err}")))?;
        // Same pin as connect(): TIMESTAMP -> TimestampTz requires session UTC.
        // Re-assert found_rows so URL parse cannot leave CLIENT_FOUND_ROWS off.
        let options = options.timezone(Some("+00:00".into())).found_rows(true);
        let read_only = safe_mode == SafeMode::ReadOnly;
        let mut builder = MySqlPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10));
        if read_only {
            builder = builder.after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("SET SESSION TRANSACTION READ ONLY")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            });
        }
        let pool = builder
            .connect_with(options)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self {
            pool,
            _ssh_tunnel: None,
        })
    }

    async fn load_columns(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ColumnStructure>, DbError> {
        // MySQL 8.4 exposes information_schema string columns as BINARY; CAST so sqlx can decode.
        let rows = sqlx::query(
            "SELECT CAST(column_name AS CHAR) AS name,
                    CAST(column_type AS CHAR) AS type_name,
                    CAST(is_nullable AS CHAR) AS is_nullable,
                    CAST(column_default AS CHAR) AS column_default,
                    CAST(column_comment AS CHAR) AS column_comment,
                    CAST(extra AS CHAR) AS extra
             FROM information_schema.columns
             WHERE table_schema = ? AND table_name = ?
             ORDER BY ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            let nullable: String = row.try_get("is_nullable").map_err(crate::sqlx_err)?;
            let default: Option<String> = row.try_get("column_default").map_err(crate::sqlx_err)?;
            let comment: Option<String> = row.try_get("column_comment").map_err(crate::sqlx_err)?;
            let extra: String = row.try_get("extra").map_err(crate::sqlx_err)?;
            let extra_upper = extra.to_ascii_uppercase();
            let generated = extra_upper.contains("AUTO_INCREMENT")
                || extra_upper.contains("GENERATED")
                || extra_upper.contains("VIRTUAL")
                || extra_upper.contains("STORED");
            columns.push(ColumnStructure {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
                type_name: row.try_get("type_name").map_err(crate::sqlx_err)?,
                nullable: nullable.eq_ignore_ascii_case("YES"),
                default,
                comment: comment.filter(|c| !c.is_empty()),
                generated,
            });
        }
        Ok(columns)
    }

    async fn load_indexes(&self, schema: &str, table: &str) -> Result<Vec<IndexInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT CAST(index_name AS CHAR) AS name,
                    CAST(column_name AS CHAR) AS column_name,
                    CAST(non_unique AS UNSIGNED) AS non_unique,
                    CAST(seq_in_index AS UNSIGNED) AS seq_in_index,
                    CAST(index_type AS CHAR) AS index_type
             FROM information_schema.statistics
             WHERE table_schema = ? AND table_name = ?
             ORDER BY index_name, seq_in_index",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;

        let mut indexes: Vec<IndexInfo> = Vec::new();
        for row in rows {
            let name: String = row.try_get("name").map_err(crate::sqlx_err)?;
            let column: String = row.try_get("column_name").map_err(crate::sqlx_err)?;
            let non_unique: u64 = row.try_get("non_unique").map_err(crate::sqlx_err)?;
            let primary = name.eq_ignore_ascii_case("PRIMARY");
            if let Some(existing) = indexes.iter_mut().find(|index| index.name == name) {
                existing.columns.push(column);
            } else {
                indexes.push(IndexInfo {
                    name,
                    columns: vec![column],
                    unique: non_unique == 0,
                    primary,
                    definition: None,
                });
            }
        }
        Ok(indexes)
    }

    async fn load_foreign_keys(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKeyInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT CAST(kcu.constraint_name AS CHAR) AS name,
                    CAST(kcu.column_name AS CHAR) AS column_name,
                    CAST(kcu.ordinal_position AS UNSIGNED) AS ordinal_position,
                    CAST(kcu.referenced_table_schema AS CHAR) AS ref_schema,
                    CAST(kcu.referenced_table_name AS CHAR) AS ref_table,
                    CAST(kcu.referenced_column_name AS CHAR) AS ref_column,
                    CAST(rc.update_rule AS CHAR) AS on_update,
                    CAST(rc.delete_rule AS CHAR) AS on_delete
             FROM information_schema.key_column_usage kcu
             JOIN information_schema.referential_constraints rc
               ON rc.constraint_schema = kcu.constraint_schema
              AND rc.constraint_name = kcu.constraint_name
             WHERE kcu.table_schema = ?
               AND kcu.table_name = ?
               AND kcu.referenced_table_name IS NOT NULL
             ORDER BY kcu.constraint_name, kcu.ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;

        let mut foreign_keys: Vec<ForeignKeyInfo> = Vec::new();
        for row in rows {
            let name: String = row.try_get("name").map_err(crate::sqlx_err)?;
            let column: String = row.try_get("column_name").map_err(crate::sqlx_err)?;
            let ref_column: String = row.try_get("ref_column").map_err(crate::sqlx_err)?;
            if let Some(existing) = foreign_keys.iter_mut().find(|fk| fk.name == name) {
                existing.columns.push(column);
                existing.ref_columns.push(ref_column);
            } else {
                foreign_keys.push(ForeignKeyInfo {
                    name,
                    columns: vec![column],
                    ref_schema: row.try_get("ref_schema").map_err(crate::sqlx_err)?,
                    ref_table: row.try_get("ref_table").map_err(crate::sqlx_err)?,
                    ref_columns: vec![ref_column],
                    on_update: row.try_get("on_update").map_err(crate::sqlx_err)?,
                    on_delete: row.try_get("on_delete").map_err(crate::sqlx_err)?,
                });
            }
        }
        Ok(foreign_keys)
    }

    async fn load_ddl(&self, schema: &str, table: &str) -> Result<String, DbError> {
        let table_q =
            quote_ident(Dialect::Mysql, table).map_err(|err| DbError::query(err.to_string()))?;
        // Qualify with schema so SHOW CREATE works even when the session DB differs.
        let schema_q =
            quote_ident(Dialect::Mysql, schema).map_err(|err| DbError::query(err.to_string()))?;
        let sql = format!("SHOW CREATE TABLE {schema_q}.{table_q}");
        let row = crate::query(&sql)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::sqlx_err)?
            .ok_or_else(|| DbError::query(format!("table `{schema}.{table}` not found")))?;
        // Column 1 is "Create Table" (or "Create View").
        let ddl: String = row
            .try_get(1)
            .or_else(|_| row.try_get("Create Table"))
            .or_else(|_| row.try_get("Create View"))
            .map_err(crate::sqlx_err)?;
        Ok(ddl)
    }

    async fn relation_exists(&self, schema: &str, table: &str) -> Result<bool, DbError> {
        let row = sqlx::query(
            "SELECT 1 AS ok
             FROM information_schema.tables
             WHERE table_schema = ? AND table_name = ?
             LIMIT 1",
        )
        .bind(schema)
        .bind(table)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        Ok(row.is_some())
    }
}

#[async_trait]
impl DatabaseSession for MysqlSession {
    fn dialect(&self) -> Dialect {
        Dialect::Mysql
    }

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError> {
        // MySQL 8.4 exposes information_schema string columns as BINARY; CAST so sqlx can decode.
        let rows = sqlx::query(
            "SELECT CAST(schema_name AS CHAR) AS name
             FROM information_schema.schemata
             WHERE schema_name NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
             ORDER BY schema_name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut schemas = Vec::with_capacity(rows.len());
        for row in rows {
            schemas.push(SchemaInfo {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
            });
        }
        Ok(schemas)
    }

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError> {
        // MySQL 8.4 exposes information_schema string columns as BINARY; CAST so sqlx can decode.
        let rows = sqlx::query(
            "SELECT CAST(table_name AS CHAR) AS name, CAST(table_type AS CHAR) AS kind
             FROM information_schema.tables
             WHERE table_schema = ?
             ORDER BY table_name",
        )
        .bind(schema)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut tables = Vec::with_capacity(rows.len());
        for row in rows {
            let kind: String = row.try_get("kind").map_err(crate::sqlx_err)?;
            tables.push(TableInfo {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
                kind: RelationKind::parse(&kind),
            });
        }
        Ok(tables)
    }

    async fn query(&self, sql: &str) -> Result<QueryPage, DbError> {
        crate::query_page!(&self.pool, sql, crate::read_mysql_cell)
    }

    async fn execute(&self, sql: &str) -> Result<u64, DbError> {
        crate::exec_sql!(&self.pool, sql)
    }

    async fn count_query(&self, sql: &str, params: &[sela_core::Value]) -> Result<u64, DbError> {
        crate::count_sql!(&self.pool, sql, params)
    }

    async fn apply_changes(
        &self,
        plan: &ChangePlan,
        cancel: CancellationToken,
    ) -> Result<ApplyChangesResult, DbError> {
        crate::change_exec::apply_mysql(
            &self.pool,
            crate::change_exec::StatementSource::plan(plan),
            cancel,
        )
        .await
    }

    async fn apply_statement_stream(
        &self,
        dialect: Dialect,
        table: &sela_core::TableRef,
        statements: async_channel::Receiver<sela_core::PlanStreamItem>,
        cancel: CancellationToken,
    ) -> Result<ApplyChangesResult, DbError> {
        crate::change_exec::apply_mysql(
            &self.pool,
            crate::change_exec::StatementSource::stream(dialect, table, statements),
            cancel,
        )
        .await
    }

    async fn estimate_table_rows(&self, schema: &str, table: &str) -> Result<Option<u64>, DbError> {
        let row = sqlx::query(
            "SELECT CAST(table_rows AS UNSIGNED) AS estimate
             FROM information_schema.tables
             WHERE table_schema = ? AND table_name = ?
             LIMIT 1",
        )
        .bind(schema)
        .bind(table)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let estimate: Option<u64> = row.try_get("estimate").ok();
        Ok(estimate)
    }

    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableStructure, DbError> {
        if !self.relation_exists(schema, table).await? {
            return Err(DbError::query(format!(
                "table `{schema}.{table}` not found"
            )));
        }
        let columns = self.load_columns(schema, table).await?;
        let indexes = self.load_indexes(schema, table).await?;
        let foreign_keys = self.load_foreign_keys(schema, table).await?;
        let ddl = self.load_ddl(schema, table).await?;
        Ok(TableStructure {
            schema: schema.to_string(),
            table: table.to_string(),
            columns,
            indexes,
            foreign_keys,
            ddl,
        })
    }

    async fn table_ddl(&self, schema: &str, table: &str) -> Result<String, DbError> {
        self.load_ddl(schema, table).await
    }

    async fn stream_query(
        &self,
        sql: &str,
        params: &[sela_core::Value],
        batch_size: usize,
        row_cap: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
        finish_at_cap: bool,
        cancel: CancellationToken,
        sink: async_channel::Sender<StreamBatch>,
    ) -> Result<StreamOutcome, DbError> {
        crate::stream::stream_mysql(
            &self.pool,
            sql,
            params,
            batch_size,
            row_cap,
            finish_at_cap,
            cancel,
            sink,
        )
        .await
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
    use rust_decimal::Decimal;
    use sela_core::{run_user_sql, ChangePlan, ChangeSet, StatementOutcome, TableRef, Value};
    use sqlx::Connection;
    use std::collections::BTreeMap;
    use std::str::FromStr;

    #[tokio::test]
    async fn mysql_round_trip_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql: set SELA_MYSQL_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
        session
            .execute("DROP TABLE IF EXISTS sela_probe")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_probe (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NULL,
                    rating DOUBLE NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_probe (id, name, rating) VALUES (1, 'Ada', 1.5), (2, NULL, NULL)",
            )
            .await
            .unwrap();

        let tables = session.list_tables(&schema).await.unwrap();
        assert!(tables.iter().any(|item| item.name == "sela_probe"));
        let outcome = run_user_sql(
            &session,
            "SELECT name, rating FROM sela_probe ORDER BY id",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows.len(), 2);
        assert!(rows.page.rows[1][0].is_null());
        assert_eq!(rows.total_rows, Some(2));
        session.execute("DROP TABLE sela_probe").await.unwrap();
    }

    #[tokio::test]
    async fn mysql_typed_value_round_trip_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql typed values: set SELA_MYSQL_URL to run this test");
            return;
        };
        let session = MysqlSession::connect_url(&url)
            .await
            .expect("mysql connect");
        session
            .execute("DROP TABLE IF EXISTS sela_types")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_types (
                    flag TINYINT(1),
                    bit_flag BIT(1),
                    small SMALLINT,
                    mid INT,
                    big BIGINT,
                    big_u BIGINT UNSIGNED,
                    real_num DOUBLE,
                    amount DECIMAL(12,4),
                    label VARCHAR(64),
                    payload BLOB,
                    doc JSON,
                    born DATE,
                    punched TIME,
                    seen DATETIME,
                    seen_ts TIMESTAMP NULL,
                    missing TEXT
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_types VALUES (
                    1,
                    b'1',
                    1,
                    -42,
                    9223372036854775807,
                    18446744073709551615,
                    2.5,
                    1234.5678,
                    'hello',
                    X'6869',
                    JSON_OBJECT('a', 1),
                    '2024-01-02',
                    '03:04:05',
                    '2024-01-02 03:04:05',
                    '2024-01-02 03:04:05',
                    NULL
                )",
            )
            .await
            .unwrap();

        let outcome = run_user_sql(
            &session,
            "SELECT flag, bit_flag, small, mid, big, big_u, real_num, amount, label, payload,
                    doc, born, punched, seen, seen_ts, missing
             FROM sela_types",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("rows");
        };
        let row = &rows.page.rows[0];
        assert_eq!(row[0], Value::Bool(true));
        assert_eq!(row[1], Value::Bool(true));
        assert_eq!(row[2], Value::Int(1));
        assert_eq!(row[3], Value::Int(-42));
        assert_eq!(row[4], Value::Int(i64::MAX));
        assert_eq!(row[5], Value::UInt(u64::MAX));
        assert_eq!(row[6], Value::Float(2.5));
        assert_eq!(
            row[7],
            Value::Decimal(Decimal::from_str("1234.5678").unwrap())
        );
        assert_eq!(row[8], Value::text("hello"));
        assert_eq!(row[9], Value::Bytes(b"hi".to_vec()));
        assert_eq!(row[10].parse_json().unwrap(), serde_json::json!({"a": 1}));
        assert_eq!(
            row[11],
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap())
        );
        assert_eq!(
            row[12],
            Value::Time(NaiveTime::from_hms_opt(3, 4, 5).unwrap())
        );
        assert_eq!(
            row[13],
            Value::Timestamp(
                NaiveDateTime::parse_from_str("2024-01-02 03:04:05", "%Y-%m-%d %H:%M:%S").unwrap()
            )
        );
        let expected_tz = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .unwrap();
        assert_eq!(row[14], Value::TimestampTz(expected_tz));
        assert!(row[15].is_null());
        assert_eq!(row[10].display(), "{\"a\":1}");

        session.execute("DROP TABLE sela_types").await.unwrap();
    }

    #[tokio::test]
    async fn mysql_cancel_sleep_within_one_second() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql cancel: set SELA_MYSQL_URL");
            return;
        };
        let session = MysqlSession::connect_url(&url)
            .await
            .expect("mysql connect");
        let cancel = CancellationToken::new();
        let cancel_later = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cancel_later.cancel();
        });
        let started = std::time::Instant::now();
        let err = run_user_sql(&session, "SELECT SLEEP(10)", 10, cancel)
            .await
            .expect_err("expected cancel");
        assert!(
            matches!(err, DbError::Cancelled { .. }),
            "expected Cancelled, got {err:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "cancel took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn mysql_cancel_does_not_poison_next_query() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql cancel reuse: set SELA_MYSQL_URL");
            return;
        };
        let session = MysqlSession::connect_url(&url)
            .await
            .expect("mysql connect");
        let cancel = CancellationToken::new();
        let cancel_later = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            cancel_later.cancel();
        });
        let err = run_user_sql(&session, "SELECT SLEEP(10)", 10, cancel)
            .await
            .expect_err("expected cancel");
        assert!(matches!(err, DbError::Cancelled { .. }), "{err:?}");

        let ok = run_user_sql(&session, "SELECT 1 AS n", 10, CancellationToken::new())
            .await
            .expect("next query after cancel");
        let StatementOutcome::Rows(rows) = ok else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
    }

    #[tokio::test]
    async fn mysql_stream_first_batch_before_finish() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql stream: set SELA_MYSQL_URL");
            return;
        };
        let session = MysqlSession::connect_url(&url)
            .await
            .expect("mysql connect");
        session
            .execute("DROP TABLE IF EXISTS sela_stream")
            .await
            .unwrap();
        session
            .execute("CREATE TABLE sela_stream (id INT PRIMARY KEY)")
            .await
            .unwrap();
        // Bulk-load enough rows that the stream cannot finish before batch 1.
        // (MySQL buffers result sets that embed SLEEP/BENCHMARK, so we cannot use
        // a yield-then-sleep query here; race the consumer against completion.)
        let mut values = String::from("INSERT INTO sela_stream (id) VALUES ");
        for i in 1..=5_000 {
            if i > 1 {
                values.push(',');
            }
            values.push_str(&format!("({i})"));
        }
        session.execute(&values).await.unwrap();
        let sql = "SELECT id FROM sela_stream ORDER BY id";
        let (tx, rx) = async_channel::bounded(4);
        let cancel = CancellationToken::new();
        let cancel_after_first = cancel.clone();
        let produce = session.stream_query(sql, &[], 50, None, true, cancel, tx);
        tokio::pin!(produce);
        let first = tokio::select! {
            biased;
            batch = rx.recv() => batch.expect("channel closed before first batch"),
            res = &mut produce => panic!("stream finished before first batch: {res:?}"),
        };
        assert!(
            !first.rows.is_empty(),
            "expected first batch before stream completion, got {first:?}"
        );
        assert!(first.rows.len() <= 50);
        cancel_after_first.cancel();
        while rx.try_recv().is_ok() {}
        let finished = tokio::time::timeout(std::time::Duration::from_secs(5), &mut produce)
            .await
            .expect("cancelled stream should finish promptly");
        match finished {
            Ok(summary) => assert!(summary.rows_delivered >= first.rows.len()),
            Err(DbError::Cancelled { .. }) => {}
            Err(other) => panic!("unexpected stream error: {other:?}"),
        }
        session.execute("DROP TABLE sela_stream").await.unwrap();
    }

    #[tokio::test]
    async fn mysql_describe_table_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql structure: set SELA_MYSQL_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
        session
            .execute("DROP TABLE IF EXISTS sela_struct_child")
            .await
            .unwrap();
        session
            .execute("DROP TABLE IF EXISTS sela_struct_parent")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_struct_parent (
                    id INT PRIMARY KEY,
                    label VARCHAR(64) NOT NULL,
                    region VARCHAR(64) NOT NULL,
                    UNIQUE KEY sela_struct_parent_label_region (label, region)
                ) ENGINE=InnoDB",
            )
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_struct_child (
                    id INT PRIMARY KEY,
                    parent_id INT NOT NULL,
                    parent_label VARCHAR(64) NOT NULL,
                    parent_region VARCHAR(64) NOT NULL,
                    code VARCHAR(64) NOT NULL,
                    note VARCHAR(64) NULL DEFAULT 'n/a' COMMENT 'optional note',
                    score INT NULL,
                    CONSTRAINT sela_struct_child_parent_fk
                      FOREIGN KEY (parent_id) REFERENCES sela_struct_parent(id),
                    CONSTRAINT sela_struct_child_label_fk
                      FOREIGN KEY (parent_label, parent_region)
                      REFERENCES sela_struct_parent(label, region),
                    UNIQUE KEY sela_struct_child_code_uidx (code),
                    KEY sela_struct_child_parent_score_idx (parent_id, score)
                ) ENGINE=InnoDB",
            )
            .await
            .unwrap();

        let structure = session
            .describe_table(&schema, "sela_struct_child")
            .await
            .unwrap();
        assert_eq!(structure.schema, schema);
        assert_eq!(structure.table, "sela_struct_child");

        let note = structure
            .columns
            .iter()
            .find(|column| column.name == "note")
            .expect("note column");
        assert!(note.nullable);
        assert_eq!(note.default.as_deref(), Some("n/a"));
        assert_eq!(note.comment.as_deref(), Some("optional note"));

        assert!(structure.indexes.iter().any(|index| index.primary));
        assert!(structure.indexes.iter().any(|index| {
            index.name == "sela_struct_child_code_uidx" && index.unique && !index.primary
        }));
        assert!(structure.indexes.iter().any(|index| {
            index.name == "sela_struct_child_parent_score_idx"
                && index.columns == vec!["parent_id".to_string(), "score".to_string()]
        }));

        assert_eq!(structure.foreign_keys.len(), 2);
        assert!(structure.foreign_keys.iter().any(|fk| {
            fk.name == "sela_struct_child_parent_fk"
                && fk.columns == vec!["parent_id".to_string()]
                && fk.ref_table == "sela_struct_parent"
                && fk.ref_columns == vec!["id".to_string()]
        }));
        assert!(structure.foreign_keys.iter().any(|fk| {
            fk.name == "sela_struct_child_label_fk"
                && fk.columns == vec!["parent_label".to_string(), "parent_region".to_string()]
                && fk.ref_columns == vec!["label".to_string(), "region".to_string()]
        }));

        let ddl = session
            .table_ddl(&schema, "sela_struct_child")
            .await
            .unwrap();
        assert!(ddl.to_uppercase().contains("CREATE TABLE"));
        assert!(ddl.contains("sela_struct_child"));
        assert!(ddl.contains("sela_struct_child_code_uidx"));
        assert!(ddl.contains("optional note"));
        assert_eq!(structure.ddl, ddl);

        let missing = session.describe_table(&schema, "no_such_table").await;
        assert!(missing.is_err());

        session
            .execute("DROP TABLE sela_struct_child")
            .await
            .unwrap();
        session
            .execute("DROP TABLE sela_struct_parent")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn mysql_filter_and_sort_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql filter: set SELA_MYSQL_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .expect("mysql connect");
        session
            .execute("DROP TABLE IF EXISTS sela_filter")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_filter (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NOT NULL,
                    note VARCHAR(64) NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                r#"INSERT INTO sela_filter (id, name, note) VALUES
                 (1, 'Ada', 'plain'),
                 (2, '100%', 'pct'),
                 (3, 'a_b', 'under'),
                 (4, 'back\\slash', 'esc'),
                 (5, 'O''Brien', 'quote'),
                 (6, 'ユニコード', 'unicode'),
                 (7, 'drop; --', 'inject'),
                 (8, 'wow!', 'bang')"#,
            )
            .await
            .unwrap();

        use sela_core::{
            preview_table, FilterCondition, FilterOp, SortSpec, TablePreview, PAGE_SIZE,
        };

        // Literal `%` (not a wildcard).
        let contains_pct = preview_table(
            &session,
            &schema,
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, "%")],
                sorts: &[SortSpec::asc("id")],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = contains_pct else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("100%"));
        assert_eq!(rows.row_count_kind, sela_core::RowCountKind::Unknown);

        // Literal `_` (not a single-char wildcard).
        let contains_under = preview_table(
            &session,
            &schema,
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, "_")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = contains_under else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("a_b"));

        // Escape char itself (`!`) is a literal.
        let contains_bang = preview_table(
            &session,
            &schema,
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, "!")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = contains_bang else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("wow!"));

        // Backslash in the value is a literal (MySQL stores one `\`).
        let contains_bs = preview_table(
            &session,
            &schema,
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Contains, r"\")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = contains_bs else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text(r"back\slash"));

        let inject = preview_table(
            &session,
            &schema,
            "sela_filter",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new(
                    "name",
                    FilterOp::Eq,
                    "1; DROP TABLE sela_filter;--",
                )],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = inject else {
            panic!("rows");
        };
        assert!(rows.page.rows.is_empty());
        assert_eq!(
            sela_core::exact_table_rows(&session, &schema, "sela_filter", &[])
                .await
                .unwrap(),
            8
        );

        let sorted =
            preview_table(
                &session,
                &schema,
                "sela_filter",
                PAGE_SIZE,
                0,
                CancellationToken::new(),
                TablePreview {
                    filters: &[
                        FilterCondition::new("id", FilterOp::In, "2, 6, 7").with_type("integer")
                    ],
                    sorts: &[SortSpec::desc("id")],
                },
            )
            .await
            .unwrap();
        let StatementOutcome::Rows(rows) = sorted else {
            panic!("rows");
        };
        assert_eq!(
            rows.page
                .rows
                .iter()
                .map(|r| r[0].clone())
                .collect::<Vec<_>>(),
            vec![Value::Int(7), Value::Int(6), Value::Int(2)]
        );

        session.execute("DROP TABLE sela_filter").await.unwrap();
    }

    /// Menu `LIKE` with `ESCAPE '!'`: `!%` / `!_` / `!!` match literal `%` / `_` / `!`.
    /// Bound params keep MySQL's default backslash string rules from interfering.
    #[tokio::test]
    async fn mysql_like_escape_matches_literals_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql like escape: set SELA_MYSQL_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .expect("mysql connect");
        session
            .execute("DROP TABLE IF EXISTS sela_like_escape")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_like_escape (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NOT NULL
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                r#"INSERT INTO sela_like_escape (id, name) VALUES
                 (1, '100%'),
                 (2, '100x'),
                 (3, 'a_b'),
                 (4, 'axb'),
                 (5, 'wow!')"#,
            )
            .await
            .unwrap();

        use sela_core::{
            preview_table, FilterCondition, FilterOp, SortSpec, TablePreview, PAGE_SIZE,
        };

        let pct = preview_table(
            &session,
            &schema,
            "sela_like_escape",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Like, "100!%")],
                sorts: &[SortSpec::asc("id")],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = pct else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("100%"));

        let under = preview_table(
            &session,
            &schema,
            "sela_like_escape",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Like, "a!_b")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = under else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("a_b"));

        let bang = preview_table(
            &session,
            &schema,
            "sela_like_escape",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview {
                filters: &[FilterCondition::new("name", FilterOp::Like, "wow!!")],
                sorts: &[],
            },
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = bang else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("wow!"));

        // Backslash in a LIKE value is a bound literal; ESCAPE '!' keeps SQL well-formed.
        let built = sela_core::build_table_select(
            sela_core::Dialect::Mysql,
            &schema,
            "sela_like_escape",
            &[FilterCondition::new("name", FilterOp::Like, r"100!%\")],
            &[],
        )
        .unwrap();
        assert!(built.sql.contains("LIKE ? ESCAPE '!'"));
        assert!(
            !built.sql.contains(r"ESCAPE '\'"),
            "backslash ESCAPE breaks MySQL string parsing"
        );
        assert_eq!(built.params[0], Value::text(r"100!%\"));

        session
            .execute("DROP TABLE sela_like_escape")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn mysql_apply_change_plan_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql change plan: set SELA_MYSQL_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
        let qschema = quote_ident(Dialect::Mysql, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_changes"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_changes (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NOT NULL,
                    note VARCHAR(64) NULL
                )"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO {qschema}.sela_changes (id, name, note) VALUES (1, 'a', NULL), (2, 'b', 'keep')"
            ))
            .await
            .unwrap();

        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_changes"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("alpha"),
        )
        .unwrap();
        cs.delete_row(&[Value::Int(2)]).unwrap();
        let mut values = BTreeMap::new();
        values.insert("id".into(), Value::Int(3));
        values.insert("name".into(), Value::text("c"));
        values.insert("note".into(), Value::Null);
        cs.insert_row(values).unwrap();

        let plan = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        let result = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.statements_executed, 3);

        let page = session
            .query(&format!(
                "SELECT id, name FROM {qschema}.sela_changes ORDER BY id"
            ))
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0][1], Value::text("alpha"));
        assert_eq!(page.rows[1][0], Value::Int(3));

        let mut bad = ChangeSet::try_new(
            TableRef::new(&schema, "sela_changes"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        bad.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("alpha"),
            Value::text("should_rollback"),
        )
        .unwrap();
        let mut dup = BTreeMap::new();
        dup.insert("id".into(), Value::Int(3));
        dup.insert("name".into(), Value::text("dup"));
        bad.insert_row(dup).unwrap();
        let bad_plan = ChangePlan::build(&bad, Dialect::Mysql).unwrap();
        assert!(session
            .apply_changes(&bad_plan, CancellationToken::new())
            .await
            .is_err());

        let page = session
            .query(&format!(
                "SELECT name FROM {qschema}.sela_changes WHERE id = 1"
            ))
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::text("alpha"));

        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_changes"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn mysql_apply_change_plan_exact_one_row_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql change guard: set SELA_MYSQL_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
        let qschema = quote_ident(Dialect::Mysql, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_guard"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_guard (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NOT NULL
                )"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO {qschema}.sela_guard (id, name) VALUES (1, 'a'), (2, 'b')"
            ))
            .await
            .unwrap();

        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_guard"),
            vec!["id".into()],
            vec!["id".into(), "name".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("changed"),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(999)],
            "name",
            Value::text("ghost"),
            Value::text("nope"),
        )
        .unwrap();
        let err = session
            .apply_changes(
                &ChangePlan::build(&cs, Dialect::Mysql).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DbError::ChangeConflict {
                    statement_index: 1,
                    rows_affected: 0,
                    ..
                }
            ),
            "{err:?}"
        );
        let page = session
            .query(&format!(
                "SELECT name FROM {qschema}.sela_guard WHERE id = 1"
            ))
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::text("a"));

        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_guard"),
            vec!["id".into()],
            vec!["id".into(), "name".into()],
        )
        .unwrap();
        cs.delete_row(&[Value::Int(2)]).unwrap();
        cs.delete_row(&[Value::Int(999)]).unwrap();
        let err = session
            .apply_changes(
                &ChangePlan::build(&cs, Dialect::Mysql).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DbError::ChangeConflict {
                    statement_index: 1,
                    rows_affected: 0,
                    ..
                }
            ),
            "{err:?}"
        );
        let page = session
            .query(&format!("SELECT id FROM {qschema}.sela_guard WHERE id = 2"))
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);

        // Same-value UPDATE must succeed: sqlx enables CLIENT_FOUND_ROWS by default
        // (and we set found_rows(true) explicitly), so matched rows count as 1.
        // ChangeSet merge rules drop a no-op cell edit, so build the plan manually.
        let plan = ChangePlan {
            dialect: Dialect::Mysql,
            table: TableRef::new(&schema, "sela_guard"),
            statements: vec![sela_core::PlannedStatement {
                op: sela_core::PlanOp::Update,
                sql: format!("UPDATE {qschema}.`sela_guard` SET `name` = ? WHERE `id` = ?"),
                params: vec![Value::text("a"), Value::Int(1)],
                pk: vec![Value::Int(1)],
                unchecked_optimistic: Vec::new(),
            }],
        };
        let result = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.statements_executed, 1);
        assert_eq!(result.rows_affected, 1);

        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_guard"))
            .await
            .unwrap();
    }

    #[tokio::test]
    /// CAN-04 (pre-cancel), OW-02 (optimistic conflict). CAN-02 lock cancel added below.
    async fn can_ow_mysql_apply_changes_cancel_and_optimistic_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql apply cancel/optimistic: set SELA_MYSQL_URL");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
        let qschema = sela_core::quote_ident(Dialect::Mysql, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_opt68"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_opt68 (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NOT NULL,
                    note VARCHAR(64) NULL
                )"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO {qschema}.sela_opt68 (id, name, note) VALUES (1, 'a', NULL), (2, 'b', 'x')"
            ))
            .await
            .unwrap();

        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_opt68"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("cancelled"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = session
            .apply_changes(&plan, cancel)
            .await
            .expect_err("pre-cancelled");
        assert!(matches!(err, DbError::Cancelled { .. }), "{err:?}");

        session
            .execute(&format!(
                "UPDATE {qschema}.sela_opt68 SET name = 'concurrent' WHERE id = 1"
            ))
            .await
            .unwrap();
        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_opt68"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("mine"),
        )
        .unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("name".into(), Value::text("b"));
        expected.insert("note".into(), Value::text("x"));
        cs.delete_row_with_expected(&[Value::Int(2)], expected)
            .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        let err = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DbError::ChangeConflict {
                    statement_index: 1,
                    rows_affected: 0,
                    ..
                }
            ),
            "{err:?}"
        );
        let page = session
            .query(&format!(
                "SELECT id, name FROM {qschema}.sela_opt68 ORDER BY id"
            ))
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0][1], Value::text("concurrent"));
        assert_eq!(page.rows[1][1], Value::text("b"));

        // OW-03: optimistic WHERE with NULL old non-PK still commits.
        session
            .execute(&format!(
                "UPDATE {qschema}.sela_opt68 SET name = 'a', note = NULL WHERE id = 1"
            ))
            .await
            .unwrap();
        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_opt68"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("ok"),
        )
        .unwrap();
        cs.set_cell(&[Value::Int(1)], "note", Value::Null, Value::text("filled"))
            .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        let result = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.statements_executed, 1);

        // CAN-02: 2-statement plan — stmt1 updates unlocked row 2, stmt2 blocks on
        // row 1. Cancel must roll back stmt1. MySQL rejects prepared BEGIN (1295).
        session
            .execute(&format!(
                "UPDATE {qschema}.sela_opt68 SET name = 'a', note = NULL WHERE id = 1"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "UPDATE {qschema}.sela_opt68 SET name = 'b', note = 'x' WHERE id = 2"
            ))
            .await
            .unwrap();
        let lock_pool = sqlx::MySqlPool::connect(&url).await.expect("lock pool");
        let mut lock_conn = lock_pool.acquire().await.expect("lock conn");
        let mut lock_tx = lock_conn.begin().await.expect("lock BEGIN");
        crate::query(&format!(
            "SELECT id FROM {qschema}.sela_opt68 WHERE id = 1 FOR UPDATE"
        ))
        .execute(&mut *lock_tx)
        .await
        .unwrap();
        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_opt68"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("name".into(), Value::text("b"));
        expected.insert("note".into(), Value::text("x"));
        cs.delete_row_with_expected(&[Value::Int(2)], expected)
            .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("blocked"),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        assert_eq!(plan.statements.len(), 2);
        assert_eq!(plan.statements[0].op, sela_core::PlanOp::Delete);
        assert_eq!(plan.statements[1].op, sela_core::PlanOp::Update);
        let cancel = CancellationToken::new();
        let cancel_later = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cancel_later.cancel();
        });
        let started = std::time::Instant::now();
        let err = session
            .apply_changes(&plan, cancel)
            .await
            .expect_err("blocked apply must cancel");
        assert!(
            matches!(err, DbError::Cancelled { .. }),
            "expected Cancelled, got {err:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "cancel took {:?}",
            started.elapsed()
        );
        lock_tx.rollback().await.expect("lock ROLLBACK");
        drop(lock_conn);
        lock_pool.close().await;
        let page = session
            .query(&format!(
                "SELECT id, name FROM {qschema}.sela_opt68 ORDER BY id"
            ))
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 2, "CAN-02: stmt1 DELETE must roll back");
        assert_eq!(page.rows[0][1], Value::text("a"));
        assert_eq!(page.rows[1][1], Value::text("b"));

        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_opt68"))
            .await
            .unwrap();
    }

    /// Optimistic WHERE: float kept; MySQL JSON and oversized BLOB skipped.
    #[tokio::test]
    async fn ow_mysql_optimistic_float_json_blob_update_and_delete_when_configured() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql typed optimistic: set SELA_MYSQL_URL");
            return;
        };
        let schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let session = MysqlSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
        let qschema = sela_core::quote_ident(Dialect::Mysql, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_typed68"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_typed68 (
                    id INT PRIMARY KEY,
                    name VARCHAR(64) NOT NULL,
                    score DOUBLE NOT NULL,
                    meta JSON NOT NULL,
                    payload BLOB NOT NULL
                )"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO {qschema}.sela_typed68 (id, name, score, meta, payload)
                 VALUES (1, 'a', 1.5, '{{\"k\":1}}', X'0102'),
                        (2, 'b', 2.5, '{{\"k\":2}}', X'0304')"
            ))
            .await
            .unwrap();

        let mut types = BTreeMap::new();
        types.insert("id".into(), "int".into());
        types.insert("name".into(), "varchar".into());
        types.insert("score".into(), "double".into());
        types.insert("meta".into(), "json".into());
        types.insert("payload".into(), "blob".into());
        let cols = vec![
            "id".into(),
            "name".into(),
            "score".into(),
            "meta".into(),
            "payload".into(),
        ];
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new(&schema, "sela_typed68"),
            vec!["id".into()],
            cols.clone(),
            types.clone(),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "name",
            Value::text("a"),
            Value::text("a2"),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "score",
            Value::Float(1.5),
            Value::Float(1.75),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "meta",
            Value::json_raw(r#"{"k":1}"#),
            Value::json_raw(r#"{"k":9}"#),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "payload",
            Value::Bytes(vec![1, 2]),
            Value::Bytes(vec![9, 9]),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        let where_sql = plan.statements[0].sql.split(" WHERE ").nth(1).unwrap_or("");
        assert!(
            where_sql.contains("AND `score` =")
                && where_sql.contains("AND `payload` =")
                && !where_sql.contains("`meta`"),
            "{}",
            plan.statements[0].sql
        );
        assert_eq!(
            plan.statements[0].unchecked_optimistic,
            vec!["meta".to_string()]
        );
        session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();

        session
            .execute(&format!(
                "UPDATE {qschema}.sela_typed68 SET score = 9.0 WHERE id = 1"
            ))
            .await
            .unwrap();
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new(&schema, "sela_typed68"),
            vec!["id".into()],
            cols.clone(),
            types.clone(),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "score",
            Value::Float(1.75),
            Value::Float(3.0),
        )
        .unwrap();
        let err = session
            .apply_changes(
                &ChangePlan::build(&cs, Dialect::Mysql).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ChangeConflict { .. }), "{err:?}");

        let big = vec![0u8; sela_core::OPTIMISTIC_BLOB_MAX_BYTES + 8];
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new(&schema, "sela_typed68"),
            vec!["id".into()],
            cols,
            types,
        )
        .unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("name".into(), Value::text("b"));
        expected.insert("score".into(), Value::Float(2.5));
        expected.insert("meta".into(), Value::json_raw(r#"{"k":2}"#));
        expected.insert("payload".into(), Value::Bytes(big));
        cs.delete_row_with_expected(&[Value::Int(2)], expected)
            .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Mysql).unwrap();
        assert!(plan.statements[0]
            .unchecked_optimistic
            .contains(&"meta".to_string()));
        assert!(plan.statements[0]
            .unchecked_optimistic
            .contains(&"payload".to_string()));
        session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        let page = session
            .query(&format!(
                "SELECT COUNT(*) AS c FROM {qschema}.sela_typed68 WHERE id = 2"
            ))
            .await
            .unwrap();
        assert_eq!(page.rows[0][0], Value::Int(0));

        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_typed68"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn mysql_read_only_safe_mode_rejects_insert() {
        let Ok(url) = std::env::var("SELA_MYSQL_URL") else {
            eprintln!("skip mysql read-only safe mode: set SELA_MYSQL_URL");
            return;
        };
        let _schema = std::env::var("SELA_MYSQL_SCHEMA").unwrap_or_else(|_| "sample".into());
        let rw = MysqlSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| panic!("SELA_MYSQL_URL is set but MySQL is unreachable: {err}"));
        rw.execute("DROP TABLE IF EXISTS sela_ro").await.unwrap();
        rw.execute("CREATE TABLE sela_ro (id INT PRIMARY KEY, name VARCHAR(64) NOT NULL)")
            .await
            .unwrap();
        rw.close().await;

        let session = MysqlSession::connect_url_with_safe_mode(&url, SafeMode::ReadOnly)
            .await
            .unwrap();
        let err = session
            .execute("INSERT INTO sela_ro (id, name) VALUES (1, 'x')")
            .await
            .expect_err("INSERT must fail under SESSION TRANSACTION READ ONLY");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("read only") || msg.contains("read-only") || msg.contains("1792"),
            "unexpected error: {err}"
        );
        let page = session.query("SELECT 1 AS n").await.unwrap();
        assert_eq!(page.rows.len(), 1);
        session.close().await;

        let cleanup = MysqlSession::connect_url(&url).await.unwrap();
        cleanup
            .execute("DROP TABLE IF EXISTS sela_ro")
            .await
            .unwrap();
    }
}
