use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

use tokio_util::sync::CancellationToken;

use sela_core::{
    quote_ident, ApplyChangesResult, ChangePlan, ColumnStructure, DatabaseSession, DbError,
    Dialect, ForeignKeyInfo, IndexInfo, QueryPage, RelationKind, SafeMode, SavedConnection,
    SchemaInfo, StreamBatch, StreamOutcome, TableInfo, TableStructure, Value,
};

use sqlx::Row;

pub struct PostgresSession {
    pool: PgPool,
    /// Keeps the local SSH forward alive for the session lifetime (SSH-10).
    _ssh_tunnel: Option<sela_ssh::SshTunnel>,
}

impl PostgresSession {
    pub async fn connect(connection: &SavedConnection, password: &str) -> Result<Self, DbError> {
        Self::connect_with_tunnel(connection, password, None).await
    }

    pub(crate) async fn connect_with_tunnel(
        connection: &SavedConnection,
        password: &str,
        ssh_tunnel: impl Into<Option<sela_ssh::SshTunnel>>,
    ) -> Result<Self, DbError> {
        let mut options = PgConnectOptions::new()
            .host(connection.host.trim())
            .port(connection.port)
            .username(connection.username.trim())
            .password(password)
            .database(connection.database.trim());
        if connection.safe_mode == SafeMode::ReadOnly {
            // Server-side guard for M2-4 Read-only (blocks nextval, DML, etc.).
            options = options.options([("default_transaction_read_only", "on")]);
        }
        let options = crate::tls::apply_postgres_tls(options, &connection.tls)?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self {
            pool,
            _ssh_tunnel: ssh_tunnel.into(),
        })
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
        let mut options: PgConnectOptions = url
            .parse()
            .map_err(|err| DbError::config(format!("invalid Postgres URL: {err}")))?;
        if safe_mode == SafeMode::ReadOnly {
            options = options.options([("default_transaction_read_only", "on")]);
        }
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
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
        let rows = sqlx::query(
            "SELECT a.attname AS name,
                    format_type(a.atttypid, a.atttypmod) AS type_name,
                    NOT a.attnotnull AS nullable,
                    pg_get_expr(ad.adbin, ad.adrelid) AS default_expr,
                    col_description(c.oid, a.attnum) AS comment,
                    a.attidentity::text AS identity,
                    a.attgenerated::text AS generated
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
            let identity: String = row.try_get("identity").map_err(crate::sqlx_err)?;
            let generated_attr: String = row.try_get("generated").map_err(crate::sqlx_err)?;
            let generated = identity == "a" || identity == "d" || !generated_attr.is_empty();
            columns.push(ColumnStructure {
                name: row.try_get("name").map_err(crate::sqlx_err)?,
                type_name: row.try_get("type_name").map_err(crate::sqlx_err)?,
                nullable: row.try_get("nullable").map_err(crate::sqlx_err)?,
                default: row.try_get("default_expr").map_err(crate::sqlx_err)?,
                comment: row.try_get("comment").map_err(crate::sqlx_err)?,
                generated,
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

    async fn count_query(&self, sql: &str, params: &[Value]) -> Result<u64, DbError> {
        crate::count_sql!(&self.pool, sql, params)
    }

    async fn apply_changes(
        &self,
        plan: &ChangePlan,
        cancel: CancellationToken,
    ) -> Result<ApplyChangesResult, DbError> {
        crate::change_exec::apply_postgres(&self.pool, plan, cancel).await
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
        params: &[Value],
        batch_size: usize,
        row_cap: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
        finish_at_cap: bool,
        cancel: CancellationToken,
        sink: async_channel::Sender<StreamBatch>,
    ) -> Result<StreamOutcome, DbError> {
        crate::stream::stream_postgres(
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
        // Tunnel drops when the session is dropped (SSH-10). Pool close first so
        // in-flight queries fail closed before the listener goes away.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
    use rust_decimal::Decimal;
    use sela_core::{
        preview_table, run_user_sql, ChangePlan, ChangeSet, Interval, StatementOutcome,
        TablePreview, TableRef, Value, PAGE_SIZE,
    };
    use std::collections::BTreeMap;
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
            TablePreview::default(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows.len(), 2);
        assert!(rows.page.columns.iter().any(|column| column.name == "name"));
        // Estimate may be stale until ANALYZE; exact count is on demand.
        let exact = sela_core::exact_table_rows(&session, &schema, "sela_probe", &[])
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
    /// Unsupported binary types (polygon, tstzrange, composites, tsquery) must fail
    /// safe as `<TYPE>` — never control-character junk.
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
            .execute("DROP TYPE IF EXISTS sela_pair")
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
            .execute("CREATE TYPE sela_pair AS (a int, b text)")
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
                    wrapped sela_inet_dom,
                    poly polygon,
                    window_tz tstzrange,
                    pair sela_pair,
                    query tsquery
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
                    '10.0.0.1'::sela_inet_dom,
                    '((0,0),(1,0),(0,1))'::polygon,
                    tstzrange('2024-01-01 00:00:00+00', '2024-01-02 00:00:00+00'),
                    ROW(1, 'x')::sela_pair,
                    to_tsquery('english', 'fat & cat')
                ), (
                    2,
                    NULL, NULL, NULL, NULL, NULL, NULL,
                    NULL, NULL, NULL, NULL, NULL, NULL,
                    NULL, NULL, NULL, NULL
                )",
            )
            .await
            .unwrap();

        // Decoded types: exact psql-like text. Unsupported binary types: `<TYPE>` only.
        let expected_exact = [
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
        let expected_placeholder = ["poly", "window_tz", "pair", "query"];

        let assert_no_control_chars = |label: &str, name: &str, text: &str| {
            assert!(
                !text.chars().any(char::is_control),
                "{label}: {name} display must not contain control characters, got {text:?}"
            );
        };

        let assert_payload = |label: &str, row: &[Value], null_row: bool| {
            // Skip the leading `id` column when present (preview SELECT *).
            let payload_len = expected_exact.len() + expected_placeholder.len();
            let offset = if row.len() == payload_len + 1 { 1 } else { 0 };
            assert_eq!(row.len() - offset, payload_len, "{label}: column count");

            for (i, cell) in row.iter().enumerate().skip(offset) {
                let got = cell.display().into_owned();
                assert_no_control_chars(label, &format!("col{i}"), &got);
            }

            for (i, (name, want)) in expected_exact.iter().enumerate() {
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

            let placeholder_base = offset + expected_exact.len();
            for (i, name) in expected_placeholder.iter().enumerate() {
                let cell = &row[placeholder_base + i];
                if null_row {
                    assert!(
                        cell.is_null(),
                        "{label}: {name} should be NULL, got {cell:?}"
                    );
                    continue;
                }
                let got = cell.display().into_owned();
                assert!(
                    got.starts_with('<') && got.ends_with('>') && got.len() > 2,
                    "{label}: {name} must fail safe as <TYPE>, got {got:?} ({cell:?})"
                );
                assert_no_control_chars(label, name, &got);
            }
        };

        let preview = preview_table(
            &session,
            &schema,
            "sela_text_fallback",
            PAGE_SIZE,
            0,
            CancellationToken::new(),
            TablePreview::default(),
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
            "SELECT ints, texts, mood, addr, net, mac, cash, flags, pt, span, search, wrapped,
                    poly, window_tz, pair, query
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
            .execute("DROP TYPE IF EXISTS sela_pair")
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

    /// `SELECT pg_sleep(n)` returns void — must be a NULL-style cell with header
    /// type `void`, never a red `!<VOID>` decode marker (#62).
    #[tokio::test]
    async fn postgres_void_pg_sleep_is_null_style_cell() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres void: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        let outcome = run_user_sql(&session, "SELECT pg_sleep(0)", 10, CancellationToken::new())
            .await
            .expect("pg_sleep");
        let StatementOutcome::Rows(rows) = outcome else {
            panic!("expected rows from pg_sleep");
        };
        assert_eq!(rows.page.columns.len(), 1);
        assert_eq!(rows.page.columns[0].type_name.to_ascii_lowercase(), "void");
        assert_eq!(rows.page.rows.len(), 1);
        assert!(
            rows.page.rows[0][0].is_null(),
            "void must render as NULL-style, got {:?}",
            rows.page.rows[0][0]
        );
        assert!(!rows.page.rows[0][0].is_decode_error());
        assert!(!rows.page.rows[0][0].is_unsupported_placeholder());
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
        let produce = session.stream_query(sql, &[], 50, None, true, cancel, tx);
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

    #[tokio::test]
    async fn postgres_filter_and_sort_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres filter: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        session
            .execute("DROP TABLE IF EXISTS sela_filter")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_filter (
                    id integer PRIMARY KEY,
                    name text NOT NULL,
                    note text
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
                 (4, E'back\\slash', 'esc'),
                 (5, 'O''Brien', 'quote'),
                 (6, 'ユニコード', 'unicode'),
                 (7, 'drop; --', 'inject'),
                 (8, 'wow!', 'bang')"#,
            )
            .await
            .unwrap();

        use sela_core::{FilterCondition, FilterOp, SortSpec, TablePreview};

        let contains = preview_table(
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
        let StatementOutcome::Rows(rows) = contains else {
            panic!("rows");
        };
        assert_eq!(rows.page.rows.len(), 1);
        assert_eq!(rows.page.rows[0][1], Value::text("100%"));
        assert_eq!(rows.row_count_kind, sela_core::RowCountKind::Unknown);

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
    #[tokio::test]
    async fn postgres_like_escape_matches_literals_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres like escape: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        session
            .execute("DROP TABLE IF EXISTS sela_like_escape")
            .await
            .unwrap();
        session
            .execute(
                "CREATE TABLE sela_like_escape (
                    id integer PRIMARY KEY,
                    name text NOT NULL
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

        use sela_core::{FilterCondition, FilterOp, SortSpec, TablePreview};

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

        session
            .execute("DROP TABLE sela_like_escape")
            .await
            .unwrap();
    }

    /// Run-all path for a plpgsql body: dollar quotes must keep the inner `;`
    /// so `split_statements` + sequential execute matches the SQL editor.
    #[tokio::test]
    async fn postgres_plpgsql_create_function_via_split_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres plpgsql: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let session = PostgresSession::connect_url(&url)
            .await
            .expect("postgres connect");
        let sql = "CREATE OR REPLACE FUNCTION sela_plpgsql_probe() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT sela_plpgsql_probe()";
        let parts = sela_core::split_statements(sql, sela_core::Dialect::Postgres)
            .expect("dollar-quoted CREATE FUNCTION must be one statement before SELECT");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].text.contains("RETURN 1;"));
        for part in &parts {
            run_user_sql(&session, &part.text, 10, CancellationToken::new())
                .await
                .unwrap_or_else(|err| panic!("run {:?}: {err}", part.text));
        }
        let queried = run_user_sql(
            &session,
            "SELECT sela_plpgsql_probe()",
            10,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let StatementOutcome::Rows(rows) = queried else {
            panic!("expected rows");
        };
        assert_eq!(rows.page.rows[0][0], Value::Int(1));
        session
            .execute("DROP FUNCTION IF EXISTS sela_plpgsql_probe()")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn postgres_apply_change_plan_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres change plan: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
        let qschema = quote_ident(Dialect::Postgres, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_changes"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_changes (
                    id INT PRIMARY KEY,
                    name TEXT NOT NULL,
                    note TEXT
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

        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
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
        let bad_plan = ChangePlan::build(&bad, Dialect::Postgres).unwrap();
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
    async fn postgres_apply_change_plan_exact_one_row_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres change guard: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
        let qschema = quote_ident(Dialect::Postgres, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_guard"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_guard (
                    id INT PRIMARY KEY,
                    name TEXT NOT NULL
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
                &ChangePlan::build(&cs, Dialect::Postgres).unwrap(),
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
                &ChangePlan::build(&cs, Dialect::Postgres).unwrap(),
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

        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_guard"))
            .await
            .unwrap();
    }

    #[tokio::test]
    /// CAN-02 (blocked lock + cancel), CAN-04 (token), OW-02 (optimistic conflict).
    async fn can_ow_postgres_apply_changes_cancel_and_optimistic_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres apply cancel/optimistic: set SELA_POSTGRES_URL");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
        let qschema = quote_ident(Dialect::Postgres, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_opt68"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_opt68 (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    note TEXT
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

        // Pre-cancelled apply: nothing applied.
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
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = session
            .apply_changes(&plan, cancel)
            .await
            .expect_err("pre-cancelled");
        assert!(matches!(err, DbError::Cancelled { .. }), "{err:?}");

        // Concurrent non-PK edit → optimistic ChangeConflict + full rollback.
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
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
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

        // CAN-02: 2-statement plan — stmt1 updates unlocked row 2, stmt2 blocks on
        // row 1 locked by another connection. Cancel must roll back stmt1 too.
        session
            .execute(&format!(
                "UPDATE {qschema}.sela_opt68 SET name = 'a' WHERE id = 1"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "UPDATE {qschema}.sela_opt68 SET name = 'b' WHERE id = 2"
            ))
            .await
            .unwrap();
        let lock_pool = sqlx::PgPool::connect(&url).await.expect("lock pool");
        let mut lock_conn = lock_pool.acquire().await.expect("lock conn");
        sqlx::query("BEGIN").execute(&mut *lock_conn).await.unwrap();
        crate::query(&format!(
            "SELECT id FROM {qschema}.sela_opt68 WHERE id = 1 FOR UPDATE"
        ))
        .execute(&mut *lock_conn)
        .await
        .unwrap();
        let mut cs = ChangeSet::try_new(
            TableRef::new(&schema, "sela_opt68"),
            vec!["id".into()],
            vec!["id".into(), "name".into(), "note".into()],
        )
        .unwrap();
        // DELETE id=2 runs first (unlocked); UPDATE id=1 blocks on the lock.
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
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
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
        sqlx::query("ROLLBACK")
            .execute(&mut *lock_conn)
            .await
            .unwrap();
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
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        let result = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.statements_executed, 1);

        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_opt68"))
            .await
            .unwrap();
    }

    /// Optimistic WHERE: float + jsonb kept; plain json / oversized bytea skipped.
    #[tokio::test]
    async fn ow_postgres_optimistic_float_json_blob_update_and_delete_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres typed optimistic: set SELA_POSTGRES_URL");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
        let qschema = quote_ident(Dialect::Postgres, &schema).unwrap();
        session
            .execute(&format!("DROP TABLE IF EXISTS {qschema}.sela_typed68"))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_typed68 (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    score DOUBLE PRECISION NOT NULL,
                    meta JSON NOT NULL,
                    doc JSONB NOT NULL,
                    payload BYTEA NOT NULL
                )"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO {qschema}.sela_typed68 (id, name, score, meta, doc, payload)
                 VALUES (1, 'a', 1.5, '{{\"k\":1}}'::json, '{{\"j\":1}}'::jsonb, '\\x0102'::bytea),
                        (2, 'b', 2.5, '{{\"k\":2}}'::json, '{{\"j\":2}}'::jsonb, '\\x0304'::bytea)"
            ))
            .await
            .unwrap();

        let mut types = BTreeMap::new();
        types.insert("id".into(), "integer".into());
        types.insert("name".into(), "text".into());
        types.insert("score".into(), "double precision".into());
        types.insert("meta".into(), "json".into());
        types.insert("doc".into(), "jsonb".into());
        types.insert("payload".into(), "bytea".into());
        let cols = vec![
            "id".into(),
            "name".into(),
            "score".into(),
            "meta".into(),
            "doc".into(),
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
            "doc",
            Value::json_raw(r#"{"j":1}"#),
            Value::json_raw(r#"{"j":9}"#),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Int(1)],
            "payload",
            Value::Bytes(vec![1, 2]),
            Value::Bytes(vec![9, 9]),
        )
        .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        let where_sql = plan.statements[0].sql.split(" WHERE ").nth(1).unwrap_or("");
        assert!(
            where_sql.contains("AND \"score\" =")
                && where_sql.contains("AND \"doc\" =")
                && where_sql.contains("::jsonb"),
            "{}",
            plan.statements[0].sql
        );
        assert!(
            !where_sql.contains("\"meta\""),
            "plain json skipped in WHERE: {}",
            plan.statements[0].sql
        );
        assert!(plan.statements[0]
            .unchecked_optimistic
            .iter()
            .any(|c| c == "meta"));
        session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();

        // Concurrent float → conflict.
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
                &ChangePlan::build(&cs, Dialect::Postgres).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::ChangeConflict { .. }), "{err:?}");

        // DELETE with oversized bytea skipped.
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
        expected.insert("doc".into(), Value::json_raw(r#"{"j":2}"#));
        expected.insert("payload".into(), Value::Bytes(big));
        cs.delete_row_with_expected(&[Value::Int(2)], expected)
            .unwrap();
        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        assert!(plan.statements[0]
            .unchecked_optimistic
            .contains(&"payload".to_string()));
        assert!(plan.statements[0]
            .unchecked_optimistic
            .contains(&"meta".to_string()));
        session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        let page = session
            .query(&format!(
                "SELECT COUNT(*)::bigint FROM {qschema}.sela_typed68 WHERE id = 2"
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
    async fn postgres_apply_changes_uuid_and_typed_columns_when_configured() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres typed changes: set SELA_POSTGRES_URL to run this test");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let session = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
        let qschema = quote_ident(Dialect::Postgres, &schema).unwrap();
        session
            .execute(&format!(
                "DROP TABLE IF EXISTS {qschema}.sela_typed_changes"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "CREATE TABLE {qschema}.sela_typed_changes (
                    id uuid PRIMARY KEY,
                    d date NOT NULL,
                    amt numeric(10,2) NOT NULL,
                    doc jsonb NOT NULL,
                    ts timestamptz NOT NULL
                )"
            ))
            .await
            .unwrap();

        let id1 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let id2 = Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap();
        let id3 = Uuid::parse_str("6ba7b811-9dad-11d1-80b4-00c04fd430c8").unwrap();
        let tz = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .unwrap();
        let tz2 = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 6, 15, 12, 0, 0)
            .unwrap();

        session
            .execute(&format!(
                "INSERT INTO {qschema}.sela_typed_changes (id, d, amt, doc, ts)
                 VALUES (
                   '{id1}'::uuid,
                   DATE '2024-01-01',
                   1.25,
                   '{{\"n\":1}}'::jsonb,
                   TIMESTAMPTZ '2024-01-02 03:04:05+00'
                 )"
            ))
            .await
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO {qschema}.sela_typed_changes (id, d, amt, doc, ts)
                 VALUES (
                   '{id2}'::uuid,
                   DATE '2024-02-01',
                   9.99,
                   '{{\"n\":2}}'::jsonb,
                   TIMESTAMPTZ '2024-02-02 03:04:05+00'
                 )"
            ))
            .await
            .unwrap();

        let mut types = BTreeMap::new();
        types.insert("id".into(), "uuid".into());
        types.insert("d".into(), "date".into());
        types.insert("amt".into(), "numeric".into());
        types.insert("doc".into(), "jsonb".into());
        types.insert("ts".into(), "timestamp with time zone".into());
        let mut cs = ChangeSet::try_new_with_types(
            TableRef::new(&schema, "sela_typed_changes"),
            vec!["id".into()],
            vec![
                "id".into(),
                "d".into(),
                "amt".into(),
                "doc".into(),
                "ts".into(),
            ],
            types,
        )
        .unwrap();
        cs.set_cell(
            &[Value::Uuid(id1)],
            "d",
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            Value::Date(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap()),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Uuid(id1)],
            "amt",
            Value::Decimal(Decimal::from_str("1.25").unwrap()),
            Value::Decimal(Decimal::from_str("2.50").unwrap()),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Uuid(id1)],
            "doc",
            Value::json_raw(r#"{"n":1}"#),
            Value::json_raw(r#"{"n":3,"ok":true}"#),
        )
        .unwrap();
        cs.set_cell(
            &[Value::Uuid(id1)],
            "ts",
            Value::TimestampTz(tz),
            Value::TimestampTz(tz2),
        )
        .unwrap();
        cs.delete_row(&[Value::Uuid(id2)]).unwrap();
        let mut insert = BTreeMap::new();
        insert.insert("id".into(), Value::Uuid(id3));
        insert.insert(
            "d".into(),
            Value::Date(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()),
        );
        insert.insert(
            "amt".into(),
            Value::Decimal(Decimal::from_str("0.00").unwrap()),
        );
        insert.insert("doc".into(), Value::json_raw("[]"));
        insert.insert("ts".into(), Value::TimestampTz(tz2));
        cs.insert_row(insert).unwrap();

        let plan = ChangePlan::build(&cs, Dialect::Postgres).unwrap();
        assert!(plan.statements.iter().any(|s| s.sql.contains("::uuid")));
        assert!(plan.statements.iter().any(|s| s.sql.contains("::date")));
        assert!(plan.statements.iter().any(|s| s.sql.contains("::numeric")));
        assert!(plan.statements.iter().any(|s| s.sql.contains("::jsonb")));
        assert!(plan
            .statements
            .iter()
            .any(|s| s.sql.contains("::timestamptz")));

        let result = session
            .apply_changes(&plan, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.statements_executed, 3);

        let page = session
            .query(&format!(
                "SELECT id, d, amt, doc::text, ts FROM {qschema}.sela_typed_changes ORDER BY d"
            ))
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0][0], Value::Uuid(id1));
        assert_eq!(
            page.rows[0][1],
            Value::Date(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap())
        );
        assert_eq!(
            page.rows[0][2],
            Value::Decimal(Decimal::from_str("2.50").unwrap())
        );
        assert_eq!(page.rows[1][0], Value::Uuid(id3));

        session
            .execute(&format!(
                "DROP TABLE IF EXISTS {qschema}.sela_typed_changes"
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn postgres_read_only_safe_mode_rejects_nextval() {
        let Ok(url) = std::env::var("SELA_POSTGRES_URL") else {
            eprintln!("skip postgres read-only safe mode: set SELA_POSTGRES_URL");
            return;
        };
        let schema = std::env::var("SELA_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".into());
        let rw = PostgresSession::connect_url(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("SELA_POSTGRES_URL is set but Postgres is unreachable: {err}")
            });
        let qschema = quote_ident(Dialect::Postgres, &schema).unwrap();
        rw.execute(&format!("DROP SEQUENCE IF EXISTS {qschema}.sela_ro_seq"))
            .await
            .unwrap();
        rw.execute(&format!("CREATE SEQUENCE {qschema}.sela_ro_seq"))
            .await
            .unwrap();
        rw.close().await;

        let session = PostgresSession::connect_url_with_safe_mode(&url, SafeMode::ReadOnly)
            .await
            .unwrap();
        let err = session
            .query(&format!("SELECT nextval('{schema}.sela_ro_seq')"))
            .await
            .expect_err("nextval must fail under default_transaction_read_only");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("read-only") || msg.contains("read only"),
            "unexpected error: {err}"
        );
        // Plain SELECT still works.
        let page = session.query("SELECT 1 AS n").await.unwrap();
        assert_eq!(page.rows[0][0], Value::Int(1));
        session.close().await;

        let cleanup = PostgresSession::connect_url(&url).await.unwrap();
        cleanup
            .execute(&format!("DROP SEQUENCE IF EXISTS {qschema}.sela_ro_seq"))
            .await
            .unwrap();
    }
}
