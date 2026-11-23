use std::time::Duration;

use async_trait::async_trait;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode};
use sqlx::MySqlPool;

use sela_core::{
    DatabaseSession, DbError, Dialect, QueryPage, RelationKind, SavedConnection, SchemaInfo,
    TableInfo,
};

use sqlx::Row;

pub struct MysqlSession {
    pool: MySqlPool,
}

impl MysqlSession {
    pub async fn connect(connection: &SavedConnection, password: &str) -> Result<Self, DbError> {
        let options = MySqlConnectOptions::new()
            .host(connection.host.trim())
            .port(connection.port)
            .username(connection.username.trim())
            .password(password)
            .database(connection.database.trim())
            .ssl_mode(if connection.ssl {
                MySqlSslMode::Required
            } else {
                MySqlSslMode::Disabled
            })
            // TIMESTAMP has no offset on the wire; sqlx's DateTime<Utc> decode assumes the
            // session time_zone is UTC. Pin +00:00 so TIMESTAMP -> TimestampTz stays consistent.
            .timezone(Some("+00:00".into()));
        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub async fn connect_url(url: &str) -> Result<Self, DbError> {
        let options: MySqlConnectOptions = url
            .parse()
            .map_err(|err| DbError::config(format!("invalid MySQL URL: {err}")))?;
        // Same pin as connect(): TIMESTAMP -> TimestampTz requires session UTC.
        let options = options.timezone(Some("+00:00".into()));
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(crate::sqlx_err)?;
        Ok(Self { pool })
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

    async fn count_query(&self, sql: &str) -> Result<u64, DbError> {
        crate::count_sql!(&self.pool, sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
    use rust_decimal::Decimal;
    use sela_core::{run_user_sql, StatementOutcome, Value};
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
            0,
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
            0,
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
        assert_eq!(row[8], Value::Text("hello".into()));
        assert_eq!(row[9], Value::Bytes(b"hi".to_vec()));
        assert_eq!(row[10], Value::Json(serde_json::json!({"a": 1})));
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
}
