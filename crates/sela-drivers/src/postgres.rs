use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::PgPool;

use tokio_util::sync::CancellationToken;

use sela_core::{
    quote_ident, ColumnStructure, DatabaseSession, DbError, Dialect, ForeignKeyInfo, IndexInfo,
    QueryPage, RelationKind, SavedConnection, SchemaInfo, StreamBatch, StreamOutcome, TableInfo,
    TableStructure,
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

    async fn load_columns(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ColumnStructure>, DbError> {
        let rows = sqlx::query(
            "SELECT a.attname AS name,
                    format_type(a.atttypid, a.atttypmod) AS type_name,
                    NOT a.attnotnull AS nullable,
                    pg_get_expr(ad.adbin, ad.adrelid) AS default_expr,
                    col_description(c.oid, a.attnum) AS comment
             FROM pg_catalog.pg_attribute a
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_catalog.pg_attrdef ad
               ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
             WHERE n.nspname = $1
               AND c.relname = $2
               AND a.attnum > 0
               AND NOT a.attisdropped
             ORDER BY a.attnum",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            columns.push(ColumnStructure {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
                type_name: row.try_get("type_name").map_err(crate::sqlx_err)?,
                nullable: row.try_get("nullable").map_err(crate::sqlx_err)?,
                default: row.try_get("default_expr").map_err(crate::sqlx_err)?,
                comment: row.try_get("comment").map_err(crate::sqlx_err)?,
            });
        }
        Ok(columns)
    }

    async fn load_indexes(&self, schema: &str, table: &str) -> Result<Vec<IndexInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT i.relname AS name,
                    ix.indisunique AS is_unique,
                    ix.indisprimary AS is_primary,
                    pg_get_indexdef(ix.indexrelid) AS definition,
                    COALESCE(
                      (
                        SELECT array_agg(a.attname ORDER BY ord.ordinality)
                        FROM unnest(ix.indkey) WITH ORDINALITY AS ord(attnum, ordinality)
                        JOIN pg_attribute a
                          ON a.attrelid = ix.indrelid AND a.attnum = ord.attnum
                      ),
                      ARRAY[]::name[]
                    ) AS columns
             FROM pg_catalog.pg_index ix
             JOIN pg_catalog.pg_class t ON t.oid = ix.indrelid
             JOIN pg_catalog.pg_class i ON i.oid = ix.indexrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
             WHERE n.nspname = $1 AND t.relname = $2
             ORDER BY ix.indisprimary DESC, i.relname",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut indexes = Vec::with_capacity(rows.len());
        for row in rows {
            let columns: Vec<String> = row.try_get("columns").map_err(crate::sqlx_err)?;
            indexes.push(IndexInfo {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
                columns,
                unique: row.try_get("is_unique").map_err(crate::sqlx_err)?,
                primary: row.try_get("is_primary").map_err(crate::sqlx_err)?,
                definition: row.try_get("definition").map_err(crate::sqlx_err)?,
            });
        }
        Ok(indexes)
    }

    async fn load_foreign_keys(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKeyInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT con.conname AS name,
                    pg_get_constraintdef(con.oid) AS definition,
                    (
                      SELECT array_agg(a.attname ORDER BY ord.ordinality)
                      FROM unnest(con.conkey) WITH ORDINALITY AS ord(attnum, ordinality)
                      JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = ord.attnum
                    ) AS columns,
                    nf.nspname AS ref_schema,
                    cf.relname AS ref_table,
                    (
                      SELECT array_agg(a.attname ORDER BY ord.ordinality)
                      FROM unnest(con.confkey) WITH ORDINALITY AS ord(attnum, ordinality)
                      JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = ord.attnum
                    ) AS ref_columns,
                    CASE con.confupdtype
                      WHEN 'a' THEN 'NO ACTION'
                      WHEN 'r' THEN 'RESTRICT'
                      WHEN 'c' THEN 'CASCADE'
                      WHEN 'n' THEN 'SET NULL'
                      WHEN 'd' THEN 'SET DEFAULT'
                      ELSE NULL
                    END AS on_update,
                    CASE con.confdeltype
                      WHEN 'a' THEN 'NO ACTION'
                      WHEN 'r' THEN 'RESTRICT'
                      WHEN 'c' THEN 'CASCADE'
                      WHEN 'n' THEN 'SET NULL'
                      WHEN 'd' THEN 'SET DEFAULT'
                      ELSE NULL
                    END AS on_delete
             FROM pg_catalog.pg_constraint con
             JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             JOIN pg_catalog.pg_class cf ON cf.oid = con.confrelid
             JOIN pg_catalog.pg_namespace nf ON nf.oid = cf.relnamespace
             WHERE con.contype = 'f'
               AND n.nspname = $1
               AND c.relname = $2
             ORDER BY con.conname",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut foreign_keys = Vec::with_capacity(rows.len());
        for row in rows {
            foreign_keys.push(ForeignKeyInfo {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
                columns: row.try_get("columns").map_err(crate::sqlx_err)?,
                ref_schema: row.try_get("ref_schema").map_err(crate::sqlx_err)?,
                ref_table: row.try_get("ref_table").map_err(crate::sqlx_err)?,
                ref_columns: row.try_get("ref_columns").map_err(crate::sqlx_err)?,
                on_update: row.try_get("on_update").map_err(crate::sqlx_err)?,
                on_delete: row.try_get("on_delete").map_err(crate::sqlx_err)?,
            });
        }
        Ok(foreign_keys)
    }

    async fn load_constraint_defs(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<(char, String, String)>, DbError> {
        let rows = sqlx::query(
            "SELECT con.conname AS name,
                    con.contype::text AS kind,
                    pg_get_constraintdef(con.oid) AS definition
             FROM pg_catalog.pg_constraint con
             JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1
               AND c.relname = $2
               AND con.contype IN ('p', 'u', 'c', 'f')
             ORDER BY
               CASE con.contype
                 WHEN 'p' THEN 0
                 WHEN 'u' THEN 1
                 WHEN 'c' THEN 2
                 WHEN 'f' THEN 3
                 ELSE 4
               END,
               con.conname",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let kind: String = row.try_get("kind").map_err(crate::sqlx_err)?;
            let kind = kind.chars().next().unwrap_or('?');
            out.push((
                kind,
                row.try_get("name").map_err(crate::sqlx_err)?,
                row.try_get("definition").map_err(crate::sqlx_err)?,
            ));
        }
        Ok(out)
    }

    async fn load_standalone_index_defs(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<String>, DbError> {
        // Indexes that back constraints are already covered by CONSTRAINT clauses.
        let rows = sqlx::query(
            "SELECT pg_get_indexdef(ix.indexrelid) AS definition
             FROM pg_catalog.pg_index ix
             JOIN pg_catalog.pg_class t ON t.oid = ix.indrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
             WHERE n.nspname = $1
               AND t.relname = $2
               AND NOT EXISTS (
                 SELECT 1 FROM pg_catalog.pg_constraint con
                 WHERE con.conindid = ix.indexrelid
               )
             ORDER BY ix.indexrelid",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::sqlx_err)?;
        let mut defs = Vec::with_capacity(rows.len());
        for row in rows {
            defs.push(row.try_get("definition").map_err(crate::sqlx_err)?);
        }
        Ok(defs)
    }

    fn build_ddl(
        schema: &str,
        table: &str,
        columns: &[ColumnStructure],
        constraints: &[(char, String, String)],
        index_defs: &[String],
    ) -> Result<String, DbError> {
        let schema_q = quote_ident(Dialect::Postgres, schema)
            .map_err(|err| DbError::query(err.to_string()))?;
        let table_q =
            quote_ident(Dialect::Postgres, table).map_err(|err| DbError::query(err.to_string()))?;
        let mut parts = Vec::new();
        for column in columns {
            let name = quote_ident(Dialect::Postgres, &column.name)
                .map_err(|err| DbError::query(err.to_string()))?;
            let mut line = format!("  {name} {}", column.type_name);
            if !column.nullable {
                line.push_str(" NOT NULL");
            }
            if let Some(default) = &column.default {
                line.push_str(" DEFAULT ");
                line.push_str(default);
            }
            parts.push(line);
        }
        for (_kind, name, definition) in constraints {
            let name_q = quote_ident(Dialect::Postgres, name)
                .map_err(|err| DbError::query(err.to_string()))?;
            parts.push(format!("  CONSTRAINT {name_q} {definition}"));
        }
        let mut ddl = format!("CREATE TABLE {schema_q}.{table_q} (\n");
        ddl.push_str(&parts.join(",\n"));
        ddl.push_str("\n);");
        for column in columns {
            if let Some(comment) = &column.comment {
                let col_q = quote_ident(Dialect::Postgres, &column.name)
                    .map_err(|err| DbError::query(err.to_string()))?;
                let escaped = comment.replace('\'', "''");
                ddl.push_str(&format!(
                    "\nCOMMENT ON COLUMN {schema_q}.{table_q}.{col_q} IS '{escaped}';"
                ));
            }
        }
        for definition in index_defs {
            ddl.push('\n');
            ddl.push_str(definition);
            if !definition.ends_with(';') {
                ddl.push(';');
            }
        }
        Ok(ddl)
    }

    async fn relation_exists(&self, schema: &str, table: &str) -> Result<bool, DbError> {
        let row = sqlx::query(
            "SELECT 1 AS ok
             FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1
               AND c.relname = $2
               AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
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

    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableStructure, DbError> {
        if !self.relation_exists(schema, table).await? {
            return Err(DbError::query(format!(
                "table `{schema}.{table}` not found"
            )));
        }
        let columns = self.load_columns(schema, table).await?;
        let indexes = self.load_indexes(schema, table).await?;
        let foreign_keys = self.load_foreign_keys(schema, table).await?;
        let constraints = self.load_constraint_defs(schema, table).await?;
        let index_defs = self.load_standalone_index_defs(schema, table).await?;
        let ddl = Self::build_ddl(schema, table, &columns, &constraints, &index_defs)?;
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
        if !self.relation_exists(schema, table).await? {
            return Err(DbError::query(format!(
                "table `{schema}.{table}` not found"
            )));
        }
        let columns = self.load_columns(schema, table).await?;
        let constraints = self.load_constraint_defs(schema, table).await?;
        let index_defs = self.load_standalone_index_defs(schema, table).await?;
        Self::build_ddl(schema, table, &columns, &constraints, &index_defs)
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

    /// M1-9: arrays, enums, inet family, money, bit, point, ranges, tsvector, and
    /// domains must show psql-like text for both table preview and raw SELECT.
    #[tokio::test]
    async fn postgres_text_fallback_types_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres text fallback: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");

        session
            .execute("DROP TABLE IF EXISTS sela_text_fallback")
            .await
            .unwrap();
        session
            .execute("DROP TYPE IF EXISTS sela_mood")
            .await
            .unwrap();
        session
            .execute("DROP DOMAIN IF EXISTS sela_inet_dom")
            .await
            .unwrap();
        session
            .execute("CREATE TYPE sela_mood AS ENUM ('sad', 'ok', 'happy')")
            .await
            .unwrap();
        session
            .execute("CREATE DOMAIN sela_inet_dom AS inet")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_text_fallback (
                    id int PRIMARY KEY,
                    ints int[],
                    texts text[],
                    mood sela_mood,
                    addr inet,
                    net cidr,
                    mac macaddr,
                    cash money,
                    flags bit(4),
                    pt point,
                    span int4range,
                    search tsvector,
                    wrapped sela_inet_dom
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "INSERT INTO sela_text_fallback VALUES (
                    1,
                    ARRAY[1, 2, NULL, 3]::int[],
                    ARRAY['a', NULL, 'b,c', '\"x\"']::text[],
                    'happy',
                    '192.168.1.1',
                    '192.168.0.0/24',
                    '08:00:2b:01:02:03',
                    12.34::money,
                    B'1010',
                    point(1.5, 2.5),
                    int4range(1, 10),
                    to_tsvector('english', 'fat cats ate rats'),
                    '10.0.0.1'::sela_inet_dom
                ), (
                    2,
                    NULL, NULL, NULL, NULL, NULL, NULL,
                    NULL, NULL, NULL, NULL, NULL, NULL
                )",
            )
            .await
            .unwrap();

        let expected = [
            ("ints", "{1,2,NULL,3}"),
            ("texts", r#"{a,NULL,"b,c","\"x\""}"#),
            ("mood", "happy"),
            ("addr", "192.168.1.1/32"),
            ("net", "192.168.0.0/24"),
            ("mac", "08:00:2b:01:02:03"),
            ("cash", "$12.34"),
            ("flags", "1010"),
            ("pt", "(1.5,2.5)"),
            ("span", "[1,10)"),
            ("search", "'ate':3 'cat':2 'fat':1 'rat':4"),
            ("wrapped", "10.0.0.1/32"),
        ];

        let assert_payload = |label: &str, row: &[Value], null_row: bool| {
            // Skip the leading `id` column when present (preview SELECT *).
            let offset = if row.len() == expected.len() + 1 {
                1
            } else {
                0
            };
            assert_eq!(row.len() - offset, expected.len(), "{label}: column count");
            for (i, (name, want)) in expected.iter().enumerate() {
                let cell = &row[offset + i];
                if null_row {
                    assert!(
                        cell.is_null(),
                        "{label}: {name} should be NULL, got {cell:?}"
                    );
                    continue;
                }
                let got = cell.display().into_owned();
                assert_eq!(
                    got, *want,
                    "{label}: column {name} display mismatch (value={cell:?})"
                );
                assert!(
                    !(got.starts_with('<') && got.ends_with('>')),
                    "{label}: {name} must not be a <TYPE> placeholder"
                );
            }
        };

        let preview = preview_table(
            &session,
            &schema,
            "sela_text_fallback",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(preview_rows) = preview else {
            panic!("expected preview rows");
        };
        assert_eq!(preview_rows.page.rows.len(), 2);
        let (filled, nulls) = if preview_rows.page.rows[0][0] == Value::Int(1) {
            (&preview_rows.page.rows[0], &preview_rows.page.rows[1])
        } else {
            (&preview_rows.page.rows[1], &preview_rows.page.rows[0])
        };
        assert_payload("preview", filled, false);
        assert_payload("preview-null", nulls, true);

        let queried = run_user_sql(
            &session,
            "SELECT ints, texts, mood, addr, net, mac, cash, flags, pt, span, search, wrapped
             FROM sela_text_fallback
             ORDER BY id",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(query_rows) = queried else {
            panic!("expected query rows");
        };
        assert_eq!(query_rows.page.rows.len(), 2);
        assert_payload("select", &query_rows.page.rows[0], false);
        assert_payload("select-null", &query_rows.page.rows[1], true);

        session
            .execute("DROP TABLE IF EXISTS sela_text_fallback")
            .await
            .unwrap();
        session
            .execute("DROP TYPE IF EXISTS sela_mood")
            .await
            .unwrap();
        session
            .execute("DROP DOMAIN IF EXISTS sela_inet_dom")
            .await
            .unwrap();
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
    async fn postgres_cancel_does_not_poison_next_query() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres cancel reuse: set SELA_POSTGRES_URL");
            return;
        };
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        let cancel = CancellationToken::new();
        let cancel_later = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            cancel_later.cancel();
        });
        let err = run_user_sql(&session, "SELECT pg_sleep(10)", 10, cancel)
            .await
            .expect_err("expected cancel");
        assert!(matches!(err, DbError::Cancelled { .. }), "{err:?}");

        // Same pool must serve a fresh query that is not cancelled by a stale kill.
        let ok = run_user_sql(&session, "SELECT 1 AS n", 10, CancellationToken::new())
            .await
            .expect("next query after cancel");
        let StatementOutcome::Rows(rows) = ok else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
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
        // Stream thousands of rows, then block in pg_sleep on a UNION ALL arm.
        // Postgres buffers a small first arm until the query ends (~500 rows is not
        // enough); 5000+ flushes to the client before the sleep arm runs.
        // Also: do not describe() before fetch (see stream.rs) — that similarly
        // prevents row streaming on this connection.
        let sql = "(SELECT generate_series(1, 5000)::int AS id) \
                   UNION ALL \
                   (SELECT -1 FROM (SELECT pg_sleep(60)) s)";
        let (tx, rx) = async_channel::bounded(4);
        let cancel = CancellationToken::new();
        let cancel_after_first = cancel.clone();
        let produce = session.stream_query(sql, 50, None, true, cancel, tx);
        tokio::pin!(produce);
        let first = tokio::select! {
            biased;
            batch = rx.recv() => batch.expect("channel closed before first batch"),
            res = &mut produce => panic!("stream finished before first batch: {res:?}"),
        };
        assert!(
            !first.rows.is_empty(),
            "expected streamed rows before pg_sleep, got {first:?}"
        );
        assert!(first.rows.len() <= 50);
        // Unblock a producer stuck on a full channel, then cancel the sleep arm.
        cancel_after_first.cancel();
        while rx.try_recv().is_ok() {}
        let finished = tokio::time::timeout(std::time::Duration::from_secs(3), &mut produce)
            .await
            .expect("cancel should end the stream well before pg_sleep(60) finishes");
        match finished {
            Ok(summary) => assert!(summary.rows_delivered >= first.rows.len()),
            Err(DbError::Cancelled { .. }) => {}
            Err(other) => panic!("unexpected stream error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn postgres_describe_table_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres structure: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
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
                    id integer PRIMARY KEY,
                    label text NOT NULL,
                    region text NOT NULL,
                    UNIQUE (label, region)
                )",
            )
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_struct_child (
                    id integer PRIMARY KEY,
                    parent_id integer NOT NULL REFERENCES sela_struct_parent(id),
                    parent_label text NOT NULL,
                    parent_region text NOT NULL,
                    code text NOT NULL,
                    note text DEFAULT 'n/a',
                    score integer,
                    FOREIGN KEY (parent_label, parent_region)
                      REFERENCES sela_struct_parent(label, region)
                )",
            )
            .await
            .unwrap();
        session
            .execute("COMMENT ON COLUMN sela_struct_child.note IS 'optional note'")
            .await
            .unwrap();
        session
            .execute("CREATE UNIQUE INDEX sela_struct_child_code_uidx ON sela_struct_child(code)")
            .await
            .unwrap();
        session
            .execute(
                "CREATE INDEX sela_struct_child_parent_score_idx
                 ON sela_struct_child(parent_id, score)",
            )
            .await
            .unwrap();
        session
            .execute(r#"CREATE TABLE "Weird Name" ("Col Id" integer PRIMARY KEY)"#)
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
        let default = note.default.as_deref().unwrap_or_default();
        assert!(
            default.contains("n/a"),
            "expected default to mention n/a, got {default:?}"
        );
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
            fk.columns == vec!["parent_id".to_string()]
                && fk.ref_table == "sela_struct_parent"
                && fk.ref_columns == vec!["id".to_string()]
        }));
        assert!(structure.foreign_keys.iter().any(|fk| {
            fk.columns == vec!["parent_label".to_string(), "parent_region".to_string()]
                && fk.ref_columns == vec!["label".to_string(), "region".to_string()]
        }));

        let ddl = session
            .table_ddl(&schema, "sela_struct_child")
            .await
            .unwrap();
        assert!(ddl.contains("CREATE TABLE"));
        assert!(ddl.contains("sela_struct_child"));
        assert!(ddl.contains("PRIMARY KEY"));
        assert!(ddl.contains("CREATE INDEX") || ddl.contains("CREATE UNIQUE INDEX"));
        assert!(ddl.contains("optional note"));
        assert_eq!(structure.ddl, ddl);

        let quoted = session.describe_table(&schema, "Weird Name").await.unwrap();
        assert_eq!(quoted.columns[0].name, "Col Id");

        let missing = session.describe_table(&schema, "no_such_table").await;
        assert!(missing.is_err());

        session.execute(r#"DROP TABLE "Weird Name""#).await.unwrap();
        session
            .execute("DROP TABLE sela_struct_child")
            .await
            .unwrap();
        session
            .execute("DROP TABLE sela_struct_parent")
            .await
            .unwrap();
    }
}
