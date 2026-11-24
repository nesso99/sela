//! Classified database errors shared by drivers and the UI.
//!
//! Drivers map vendor/`sqlx` failures into these variants. The UI shows
//! [`DbError`]'s `Display` text (the original message).

/// A database or driver failure, classified for the UI and call sites.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DbError {
    /// Could not open or keep a connection (network, pool closed, …).
    #[error("{message}")]
    Connection {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// TLS handshake or certificate verification failed.
    ///
    /// Distinct from [`Auth`] so the UI can show a TLS-specific hint without
    /// confusing it with a wrong password.
    #[error("{message}")]
    Tls {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// Authentication or authorization failed.
    #[error("{message}")]
    Auth {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// SQL syntax error or other query/statement failure.
    #[error("{message}")]
    Query {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// An operation timed out (connect, acquire, or query).
    #[error("{message}")]
    Timeout {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// The statement was cancelled (client cancel or server interrupt).
    #[error("{message}")]
    Cancelled {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// A value could not be encoded or decoded.
    #[error("{message}")]
    Decode {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// Bad connection settings, paths, or other local configuration.
    #[error("{message}")]
    Config {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// Unexpected internal failure (panicked task, driver bug, …).
    #[error("{message}")]
    Internal {
        message: String,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
    /// An UPDATE/DELETE in a [`crate::ChangePlan`] did not affect exactly one row.
    ///
    /// The applying transaction is rolled back. Typical causes: the row was
    /// deleted or its primary key changed concurrently, or a NULL PK component
    /// (`IS NULL`) matched more than one row.
    #[error("{message}")]
    ChangeConflict {
        message: String,
        /// 0-based index into [`crate::ChangePlan::statements`].
        statement_index: usize,
        /// Rows reported affected by the engine for that statement.
        rows_affected: u64,
        /// Display form of each primary-key value (table PK order).
        pk: Vec<String>,
        sqlstate: Option<String>,
        vendor_code: Option<String>,
    },
}

impl DbError {
    pub fn connection(message: impl Into<String>) -> Self {
        Self::Connection {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn tls(message: impl Into<String>) -> Self {
        Self::Tls {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn query(message: impl Into<String>) -> Self {
        Self::Query {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::Cancelled {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn decode(message: impl Into<String>) -> Self {
        Self::Decode {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            sqlstate: None,
            vendor_code: None,
        }
    }

    /// Build a [`Self::ChangeConflict`] for a plan statement that did not hit
    /// exactly one row. `pk` is the original primary-key identity from the plan.
    pub fn change_conflict(
        statement_index: usize,
        rows_affected: u64,
        pk: &[crate::types::Value],
    ) -> Self {
        let pk_display: Vec<String> = pk
            .iter()
            .map(|value| value.display().into_owned())
            .collect();
        let pk_joined = pk_display.join(", ");
        let message = format!(
            "change plan statement {statement_index} affected {rows_affected} rows (expected exactly 1); primary key: [{pk_joined}]"
        );
        Self::ChangeConflict {
            message,
            statement_index,
            rows_affected,
            pk: pk_display,
            sqlstate: None,
            vendor_code: None,
        }
    }

    /// Original human-readable message (without classification).
    pub fn message_text(&self) -> &str {
        match self {
            Self::Connection { message, .. }
            | Self::Tls { message, .. }
            | Self::Auth { message, .. }
            | Self::Query { message, .. }
            | Self::Timeout { message, .. }
            | Self::Cancelled { message, .. }
            | Self::Decode { message, .. }
            | Self::Config { message, .. }
            | Self::Internal { message, .. }
            | Self::ChangeConflict { message, .. } => message,
        }
    }

    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Connection { sqlstate, .. }
            | Self::Tls { sqlstate, .. }
            | Self::Auth { sqlstate, .. }
            | Self::Query { sqlstate, .. }
            | Self::Timeout { sqlstate, .. }
            | Self::Cancelled { sqlstate, .. }
            | Self::Decode { sqlstate, .. }
            | Self::Config { sqlstate, .. }
            | Self::Internal { sqlstate, .. }
            | Self::ChangeConflict { sqlstate, .. } => sqlstate.as_deref(),
        }
    }

    pub fn vendor_code(&self) -> Option<&str> {
        match self {
            Self::Connection { vendor_code, .. }
            | Self::Tls { vendor_code, .. }
            | Self::Auth { vendor_code, .. }
            | Self::Query { vendor_code, .. }
            | Self::Timeout { vendor_code, .. }
            | Self::Cancelled { vendor_code, .. }
            | Self::Decode { vendor_code, .. }
            | Self::Config { vendor_code, .. }
            | Self::Internal { vendor_code, .. }
            | Self::ChangeConflict { vendor_code, .. } => vendor_code.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Value;

    #[test]
    fn change_conflict_names_index_and_pk() {
        let err = DbError::change_conflict(2, 0, &[Value::Int(9), Value::text("k")]);
        match &err {
            DbError::ChangeConflict {
                statement_index,
                rows_affected,
                pk,
                ..
            } => {
                assert_eq!(*statement_index, 2);
                assert_eq!(*rows_affected, 0);
                assert_eq!(pk, &["9".to_string(), "k".to_string()]);
            }
            other => panic!("expected ChangeConflict, got {other:?}"),
        }
        let text = err.to_string();
        assert!(text.contains("statement 2"));
        assert!(text.contains("0 rows"));
        assert!(text.contains("expected exactly 1"));
        assert!(text.contains("[9, k]"));
    }
}
