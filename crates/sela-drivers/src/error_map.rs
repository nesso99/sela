//! Map `sqlx::Error` into classified [`sela_core::DbError`] values.

use sela_core::DbError;
use sqlx::error::ErrorKind;
use sqlx::Error as SqlxError;

/// Convert a sqlx failure into a [`DbError`], preserving message and codes.
pub(crate) fn sqlx_err(err: SqlxError) -> DbError {
    map_sqlx_error(&err)
}

fn map_sqlx_error(err: &SqlxError) -> DbError {
    let message = err.to_string();
    match err {
        SqlxError::Configuration(_) | SqlxError::InvalidArgument(_) => DbError::Config {
            message,
            sqlstate: None,
            vendor_code: None,
        },
        SqlxError::Database(db) => classify_database(db.as_ref()),
        SqlxError::Tls(_) => DbError::Tls {
            message,
            sqlstate: None,
            vendor_code: None,
        },
        SqlxError::Io(_) | SqlxError::Protocol(_) | SqlxError::PoolClosed => DbError::Connection {
            message,
            sqlstate: None,
            vendor_code: None,
        },
        SqlxError::PoolTimedOut => DbError::Timeout {
            message,
            sqlstate: None,
            vendor_code: None,
        },
        SqlxError::Decode(_)
        | SqlxError::Encode(_)
        | SqlxError::ColumnDecode { .. }
        | SqlxError::TypeNotFound { .. } => DbError::Decode {
            message,
            sqlstate: None,
            vendor_code: None,
        },
        SqlxError::WorkerCrashed
        | SqlxError::RowNotFound
        | SqlxError::ColumnNotFound(_)
        | SqlxError::ColumnIndexOutOfBounds { .. }
        | SqlxError::AnyDriverError(_)
        | SqlxError::InvalidSavePointStatement
        | SqlxError::BeginFailed => DbError::Internal {
            message,
            sqlstate: None,
            vendor_code: None,
        },
        _ => DbError::Internal {
            message,
            sqlstate: None,
            vendor_code: None,
        },
    }
}

fn classify_database(db: &dyn sqlx::error::DatabaseError) -> DbError {
    let message = db.message().to_string();
    let (sqlstate, vendor_code) = split_codes(db.code().as_deref());
    let kind = db.kind();
    let class = ErrorClass::from_codes(sqlstate.as_deref(), vendor_code.as_deref(), &kind);

    match class {
        ErrorClass::Auth => DbError::Auth {
            message,
            sqlstate,
            vendor_code,
        },
        ErrorClass::Query => DbError::Query {
            message,
            sqlstate,
            vendor_code,
        },
        ErrorClass::Cancelled => DbError::Cancelled {
            message,
            sqlstate,
            vendor_code,
        },
        ErrorClass::Connection => DbError::Connection {
            message,
            sqlstate,
            vendor_code,
        },
        ErrorClass::Timeout => DbError::Timeout {
            message,
            sqlstate,
            vendor_code,
        },
        ErrorClass::Internal => DbError::Internal {
            message,
            sqlstate,
            vendor_code,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorClass {
    Auth,
    Query,
    Cancelled,
    Connection,
    Timeout,
    Internal,
}

impl ErrorClass {
    fn from_codes(sqlstate: Option<&str>, vendor_code: Option<&str>, kind: &ErrorKind) -> Self {
        if let Some(state) = sqlstate {
            match state {
                // Invalid authorization specification.
                s if s.starts_with("28") => return Self::Auth,
                // Syntax error or access rule violation.
                s if s.starts_with("42") => return Self::Query,
                // Connection exception.
                s if s.starts_with("08") => return Self::Connection,
                // query_canceled — also the SQLSTATE Postgres uses for
                // statement_timeout (same class as an explicit cancel).
                "57014" => return Self::Cancelled,
                // Admin shutdown / crash shutdown / cannot connect now.
                "57P01" | "57P02" | "57P03" => return Self::Connection,
                // Other operator-intervention codes are unexpected internals.
                s if s.starts_with("57") => return Self::Internal,
                // too_many_connections is a connection capacity failure.
                "53300" => return Self::Connection,
                // Other insufficient-resources (disk_full, out_of_memory, …).
                s if s.starts_with("53") => return Self::Internal,
                // System error / external IO — not a client timeout.
                s if s.starts_with("58") => return Self::Internal,
                // lock_not_available — wait for a lock timed out / failed.
                "55P03" => return Self::Timeout,
                _ => {}
            }
        }

        if let Some(code) = vendor_code {
            match code {
                // MySQL access denied / auth
                "1044" | "1045" | "1698" => return Self::Auth,
                // MySQL syntax / bad query
                "1064" | "1146" | "1054" | "1149" => return Self::Query,
                // MySQL query interrupted / cancelled
                "1317" | "1028" => return Self::Cancelled,
                // MySQL lock wait timeout / max_execution_time exceeded
                "1205" | "3024" => return Self::Timeout,
                // MySQL cannot connect / gone away
                "2002" | "2003" | "2006" | "2013" => return Self::Connection,
                _ => {}
            }
        }

        // Constraint violations and anything else from the server are query-level.
        match kind {
            ErrorKind::UniqueViolation
            | ErrorKind::ForeignKeyViolation
            | ErrorKind::NotNullViolation
            | ErrorKind::CheckViolation
            | ErrorKind::Other
            | _ => Self::Query,
        }
    }
}

/// Split a vendor/`sqlx` code into SQLSTATE vs numeric vendor code.
fn split_codes(code: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(code) = code.filter(|c| !c.is_empty()) else {
        return (None, None);
    };
    if is_sqlstate(code) {
        (Some(code.to_string()), None)
    } else {
        (None, Some(code.to_string()))
    }
}

fn is_sqlstate(code: &str) -> bool {
    code.len() == 5
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_timed_out_is_timeout() {
        let err = map_sqlx_error(&SqlxError::PoolTimedOut);
        assert!(matches!(err, DbError::Timeout { .. }));
        assert!(err.message_text().contains("timed out") || err.message_text().contains("pool"));
    }

    #[test]
    fn pool_closed_is_connection() {
        let err = map_sqlx_error(&SqlxError::PoolClosed);
        assert!(matches!(err, DbError::Connection { .. }));
    }

    #[test]
    fn configuration_is_config() {
        let err = map_sqlx_error(&SqlxError::Configuration("bad url".into()));
        assert!(matches!(err, DbError::Config { .. }));
    }

    #[test]
    fn decode_is_decode() {
        let err = map_sqlx_error(&SqlxError::Decode("oops".into()));
        assert!(matches!(err, DbError::Decode { .. }));
    }

    #[test]
    fn io_is_connection() {
        let err = map_sqlx_error(&SqlxError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        )));
        assert!(matches!(err, DbError::Connection { .. }));
    }

    #[test]
    fn tls_handshake_is_tls_not_auth() {
        let err = map_sqlx_error(&SqlxError::Tls(
            "certificate verify failed: hostname mismatch".into(),
        ));
        match &err {
            DbError::Tls { message, .. } => {
                assert!(message.contains("hostname") || message.contains("certificate"));
            }
            other => panic!("expected Tls, got {other:?}"),
        }
        assert!(!matches!(err, DbError::Auth { .. }));
    }

    #[test]
    fn worker_crashed_is_internal() {
        let err = map_sqlx_error(&SqlxError::WorkerCrashed);
        assert!(matches!(err, DbError::Internal { .. }));
    }

    #[test]
    fn classifies_sqlstate_auth() {
        assert_eq!(
            ErrorClass::from_codes(Some("28000"), None, &ErrorKind::Other),
            ErrorClass::Auth
        );
    }

    #[test]
    fn classifies_sqlstate_syntax() {
        assert_eq!(
            ErrorClass::from_codes(Some("42601"), None, &ErrorKind::Other),
            ErrorClass::Query
        );
    }

    #[test]
    fn classifies_sqlstate_cancelled() {
        assert_eq!(
            ErrorClass::from_codes(Some("57014"), None, &ErrorKind::Other),
            ErrorClass::Cancelled
        );
    }

    #[test]
    fn classifies_sqlstate_connection() {
        assert_eq!(
            ErrorClass::from_codes(Some("08006"), None, &ErrorKind::Other),
            ErrorClass::Connection
        );
    }

    #[test]
    fn classifies_sqlstate_too_many_connections() {
        assert_eq!(
            ErrorClass::from_codes(Some("53300"), None, &ErrorKind::Other),
            ErrorClass::Connection
        );
    }

    #[test]
    fn classifies_sqlstate_disk_full_as_internal() {
        assert_eq!(
            ErrorClass::from_codes(Some("53100"), None, &ErrorKind::Other),
            ErrorClass::Internal
        );
    }

    #[test]
    fn classifies_sqlstate_admin_shutdown_as_connection() {
        assert_eq!(
            ErrorClass::from_codes(Some("57P01"), None, &ErrorKind::Other),
            ErrorClass::Connection
        );
    }

    #[test]
    fn classifies_sqlstate_lock_not_available_as_timeout() {
        assert_eq!(
            ErrorClass::from_codes(Some("55P03"), None, &ErrorKind::Other),
            ErrorClass::Timeout
        );
    }

    #[test]
    fn classifies_sqlstate_system_error_as_internal() {
        assert_eq!(
            ErrorClass::from_codes(Some("58000"), None, &ErrorKind::Other),
            ErrorClass::Internal
        );
        assert_eq!(
            ErrorClass::from_codes(Some("58030"), None, &ErrorKind::Other),
            ErrorClass::Internal
        );
    }

    #[test]
    fn classifies_mysql_vendor_auth() {
        assert_eq!(
            ErrorClass::from_codes(None, Some("1045"), &ErrorKind::Other),
            ErrorClass::Auth
        );
    }

    #[test]
    fn classifies_mysql_vendor_syntax() {
        assert_eq!(
            ErrorClass::from_codes(None, Some("1064"), &ErrorKind::Other),
            ErrorClass::Query
        );
    }

    #[test]
    fn classifies_mysql_vendor_cancelled() {
        assert_eq!(
            ErrorClass::from_codes(None, Some("1317"), &ErrorKind::Other),
            ErrorClass::Cancelled
        );
    }

    #[test]
    fn classifies_mysql_lock_wait_timeout() {
        assert_eq!(
            ErrorClass::from_codes(None, Some("1205"), &ErrorKind::Other),
            ErrorClass::Timeout
        );
    }

    #[test]
    fn classifies_mysql_max_execution_time() {
        assert_eq!(
            ErrorClass::from_codes(None, Some("3024"), &ErrorKind::Other),
            ErrorClass::Timeout
        );
    }

    #[test]
    fn split_codes_sqlstate_vs_vendor() {
        assert_eq!(split_codes(Some("42601")), (Some("42601".into()), None));
        assert_eq!(split_codes(Some("1045")), (None, Some("1045".into())));
        assert_eq!(split_codes(None), (None, None));
    }

    /// Synthetic database error so we can unit-test SQLSTATE classification
    /// without a live server.
    #[derive(Debug)]
    struct FakeDbError {
        message: &'static str,
        code: Option<&'static str>,
    }

    impl std::fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for FakeDbError {}

    impl sqlx::error::DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            self.message
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            self.code.map(std::borrow::Cow::Borrowed)
        }
    }

    #[test]
    fn database_auth_sqlstate_maps_to_auth() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "password authentication failed",
            code: Some("28P01"),
        }));
        let mapped = map_sqlx_error(&err);
        match mapped {
            DbError::Auth {
                message,
                sqlstate,
                vendor_code,
            } => {
                assert!(message.contains("password"));
                assert_eq!(sqlstate.as_deref(), Some("28P01"));
                assert_eq!(vendor_code, None);
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn database_mysql_syntax_maps_to_query() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "You have an error in your SQL syntax",
            code: Some("1064"),
        }));
        let mapped = map_sqlx_error(&err);
        match mapped {
            DbError::Query {
                sqlstate,
                vendor_code,
                ..
            } => {
                assert_eq!(sqlstate, None);
                assert_eq!(vendor_code.as_deref(), Some("1064"));
            }
            other => panic!("expected Query, got {other:?}"),
        }
    }

    #[test]
    fn database_cancelled_sqlstate_maps_to_cancelled() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "canceling statement due to user request",
            code: Some("57014"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Cancelled { .. }));
    }

    #[test]
    fn database_too_many_connections_maps_to_connection() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "sorry, too many clients already",
            code: Some("53300"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Connection { .. }));
    }

    #[test]
    fn database_disk_full_maps_to_internal() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "could not write to file: No space left on device",
            code: Some("53100"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Internal { .. }));
    }

    #[test]
    fn database_admin_shutdown_maps_to_connection() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "terminating connection due to administrator command",
            code: Some("57P01"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Connection { .. }));
    }

    #[test]
    fn database_lock_not_available_maps_to_timeout() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "could not obtain lock on relation",
            code: Some("55P03"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Timeout { .. }));
    }

    #[test]
    fn database_system_io_error_maps_to_internal() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "io error",
            code: Some("58030"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Internal { .. }));
    }

    #[test]
    fn database_mysql_lock_wait_timeout_maps_to_timeout() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "Lock wait timeout exceeded",
            code: Some("1205"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Timeout { .. }));
    }

    #[test]
    fn database_mysql_max_execution_time_maps_to_timeout() {
        let err = SqlxError::Database(Box::new(FakeDbError {
            message: "Query execution was interrupted, maximum statement execution time exceeded",
            code: Some("3024"),
        }));
        assert!(matches!(map_sqlx_error(&err), DbError::Timeout { .. }));
    }
}
