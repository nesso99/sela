use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::PgPool;

use tokio_util::sync::CancellationToken;

use sela_core::{
    DatabaseSession, DbError, Dialect, QueryPage, RelationKind, SavedConnection, SchemaInfo,
    StreamBatch, StreamOutcome, TableInfo,
};

use sqlx::Row;

pub struct PostgresSession {
    pool: PgPool,
}

impl PostgresSession {
    pub async fn connect(connection: &SavedConnection, password: &str) -> Result<Self, DbError> {
        let options = PgConnectOptions::new()
            .host(connection.host.trim())
            .port(connection.port)
            .username(connection.username.trim())
            .password(password)
            .database(connection.database.trim())
            .ssl_mode(if connection.ssl {
                PgSslMode::Require
            } else {
                PgSslMode::Disable
            });
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub async fn connect_url(url: &str) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl DatabaseSession for PostgresSession {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT schema_name AS name
             FROM information_schema.schemata
             WHERE schema_name NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
               AND schema_name NOT LIKE 'pg_temp_%'
               AND schema_name NOT LIKE 'pg_toast_temp_%'
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
        let rows = sqlx::query(
            "SELECT table_name AS name, table_type AS kind
             FROM information_schema.tables
             WHERE table_schema = $1
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
        crate::query_page!(&self.pool, sql, crate::read_postgres_cell)
    }

    async fn execute(&self, sql: &str) -> Result<u64, DbError> {
        crate::exec_sql!(&self.pool, sql)
    }

    async fn count_query(&self, sql: &str) -> Result<u64, DbError> {
        crate::count_sql!(&self.pool, sql)
    }

    async fn estimate_table_rows(&self, schema: &str, table: &str) -> Result<Option<u64>, DbError> {
        let row = sqlx::query(
            "SELECT c.reltuples::bigint AS estimate
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2
               AND c.relkind IN ('r', 'p', 'm', 'v', 'f')
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
        let estimate: i64 = row.try_get("estimate").map_err(crate::sqlx_err)?;
        if estimate < 0 {
            Ok(None)
        } else {
            Ok(Some(estimate as u64))
        }
    }

    async fn stream_query(
        &self,
        sql: &str,
        batch_size: usize,
        row_cap: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
        finish_at_cap: bool,
        cancel: CancellationToken,
        sink: async_channel::Sender<StreamBatch>,
    ) -> Result<StreamOutcome, DbError> {
        crate::stream::stream_postgres(
            &self.pool,
            sql,
            batch_size,
            row_cap,
            finish_at_cap,
            cancel,
            sink,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
    use rust_decimal::Decimal;
    use sela_core::{preview_table, run_user_sql, Interval, StatementOutcome, Value, PAGE_SIZE};
    use std::str::FromStr;
    use uuid::Uuid;

    #[tokio::test]
    async fn postgres_round_trip_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
        session
            .execute("DROP TABLE IF EXISTS sela_probe")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_probe (
                    id integer PRIMARY KEY,
                    name text,
                    in_print boolean,
                    published date
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_probe (id, name, in_print, published)
                 VALUES (1, 'Ada', true, DATE '1815-12-10'), (2, NULL, false, NULL)",
            )
            .await
            .unwrap();

        let schemas = session.list_schemas().await.unwrap();
        assert!(schemas.iter().any(|item| item.name == schema));
        let tables = session.list_tables(&schema).await.unwrap();
        assert!(tables.iter().any(|item| item.name == "sela_probe"));

        let outcome = preview_table(
            &session,
            &schema,
            "sela_probe",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows.len(), 2);
        assert!(rows.page.columns.iter().any(|column| column.name == "name"));
        // Estimate may be stale until ANALYZE; exact count is on demand.
        let exact = sela_core::exact_table_rows(&session, &schema, "sela_probe")
            .await
            .unwrap();
        assert_eq!(exact, 2);

        let queried = run_user_sql(
            &session,
            "SELECT name FROM sela_probe WHERE id = 2",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = queried else {
            panic!("expected rows");
        };
        assert!(rows.page.rows[0][0].is_null());

        session.execute("DROP TABLE sela_probe").await.unwrap();
    }

    #[tokio::test]
    async fn postgres_typed_value_round_trip_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres typed values: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        session
            .execute("DROP TABLE IF EXISTS sela_types")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_types (
                    flag boolean,
                    small int2,
                    mid int4,
                    big int8,
                    real4 float4,
                    real8 float8,
                    amount numeric(12,4),
                    label text,
                    payload bytea,
                    id uuid,
                    doc json,
                    docb jsonb,
                    born date,
                    punched time,
                    seen timestamp,
                    seen_tz timestamptz,
                    gap interval,
                    missing text
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_types VALUES (
                    true,
                    1,
                    -42,
                    9223372036854775807,
                    1.25,
                    2.5,
                    1234.5678,
                    'hello',
                    decode('6869', 'hex'),
                    '550e8400-e29b-41d4-a716-446655440000',
                    '{\"a\":1}',
                    '{\"b\":true}',
                    DATE '2024-01-02',
                    TIME '03:04:05',
                    TIMESTAMP '2024-01-02 03:04:05',
                    TIMESTAMPTZ '2024-01-02 03:04:05+00',
                    INTERVAL '1 month 2 days 01:00:00',
                    NULL
                )",
            )
            .await
            .unwrap();

        let outcome = run_user_sql(
            &session,
            "SELECT flag, small, mid, big, real4, real8, amount, label, payload, id,
                    doc, docb, born, punched, seen, seen_tz, gap, missing
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
        assert_eq!(row[1], Value::Int(1));
        assert_eq!(row[2], Value::Int(-42));
        assert_eq!(row[3], Value::Int(i64::MAX));
        assert!(matches!(row[4], Value::Float(v) if (v - 1.25).abs() < f64::EPSILON));
        assert_eq!(row[5], Value::Float(2.5));
        assert_eq!(
            row[6],
            Value::Decimal(Decimal::from_str("1234.5678").unwrap())
        );
        assert_eq!(row[7], Value::text("hello"));
        assert_eq!(row[8], Value::Bytes(b"hi".to_vec()));
        assert_eq!(
            row[9],
            Value::Uuid(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap())
        );
        assert_eq!(row[10].parse_json().unwrap(), serde_json::json!({"a": 1}));
        assert_eq!(
            row[11].parse_json().unwrap(),
            serde_json::json!({"b": true})
        );
        assert_eq!(
            row[12],
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap())
        );
        assert_eq!(
            row[13],
            Value::Time(NaiveTime::from_hms_opt(3, 4, 5).unwrap())
        );
        assert_eq!(
            row[14],
            Value::Timestamp(
                NaiveDateTime::parse_from_str("2024-01-02 03:04:05", "%Y-%m-%d %H:%M:%S").unwrap()
            )
        );
        let expected_tz = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .unwrap();
        assert_eq!(row[15], Value::TimestampTz(expected_tz));
        assert_eq!(
            row[16],
            Value::Interval(Interval {
                months: 1,
                days: 2,
                microseconds: 3_600_000_000,
            })
        );
        assert!(row[17].is_null());
        assert_eq!(row[10].display(), "{\"a\":1}");
        assert_eq!(row[15].display(), "2024-01-02T03:04:05Z");

        session.execute("DROP TABLE sela_types").await.unwrap();
    }

    #[tokio::test]
    async fn postgres_cancel_sleep_within_one_second() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres cancel: set SELA_POSTGRES_URL");
            return;
        };
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        let cancel = CancellationToken::new();
        let cancel_later = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cancel_later.cancel();
        });
        let started = std::time::Instant::now();
        let err = run_user_sql(&session, "SELECT pg_sleep(10)", 10, cancel)
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
    async fn postgres_stream_first_batch_before_finish() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres stream: set SELA_POSTGRES_URL");
            return;
        };
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        session
            .execute("DROP TABLE IF EXISTS sela_stream")
            .await
            .unwrap();
        session
            .execute("CREATE TABLE sela_stream (id int primary key)")
            .await
            .unwrap();
        for i in 0..250 {
            session
                .execute(&format!("INSERT INTO sela_stream VALUES ({i})"))
                .await
                .unwrap();
        }
        let (tx, rx) = async_channel::bounded(4);
        let cancel = CancellationToken::new();
        let produce = session.stream_query(
            "SELECT id FROM sela_stream ORDER BY id",
            50,
            Some(std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(
                1000,
            ))),
            true,
            cancel,
            tx,
        );
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("first batch timed out")
            .expect("channel closed");
        assert!(
            !first.rows.is_empty() || first.columns.is_some(),
            "first batch should arrive before the query finishes"
        );
        let outcome = produce.await.expect("stream ok");
        assert!(outcome.rows_delivered >= first.rows.len());
        // Drain remaining.
        while rx.recv().await.is_ok() {}
        session.execute("DROP TABLE sela_stream").await.unwrap();
    }
}
