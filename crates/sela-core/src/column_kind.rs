//! Classify engine `type_name` strings into a small operator-facing kind.

use crate::filter::FilterOp;

/// Coarse column kind used to pick legal filter operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Bool,
    Int,
    Float,
    Decimal,
    Date,
    Time,
    Timestamp,
    Interval,
    Text,
    Uuid,
    Json,
    Bytes,
    Other,
}

impl ColumnKind {
    /// Classify a wire `type_name` (Postgres / MySQL / SQLite). Case-insensitive;
    /// longer names are checked first (`TIMESTAMPTZ` before `TIME`, etc.).
    pub fn from_type_name(type_name: &str) -> Self {
        let t = type_name.trim().to_ascii_uppercase();
        if t.is_empty() {
            return Self::Other;
        }
        if t.contains("BOOL") || t == "BIT" || t == "TINYINT(1)" {
            return Self::Bool;
        }
        if t.contains("BYTEA")
            || t.contains("BLOB")
            || t.contains("BINARY")
            || t.contains("VARBINARY")
            || t == "BYTE"
        {
            return Self::Bytes;
        }
        if t.contains("JSON") {
            return Self::Json;
        }
        if t.contains("UUID") {
            return Self::Uuid;
        }
        if t.contains("INTERVAL") {
            return Self::Interval;
        }
        if t.contains("TIMESTAMPTZ") || t.contains("TIMESTAMP") || t.contains("DATETIME") {
            return Self::Timestamp;
        }
        if t.contains("TIME") {
            return Self::Time;
        }
        if t.contains("DATE") {
            return Self::Date;
        }
        if t.contains("NUMERIC")
            || t.contains("DECIMAL")
            || t.contains("MONEY")
            || t.contains("NUMBER")
        {
            return Self::Decimal;
        }
        if t.contains("FLOAT") || t.contains("DOUBLE") || t.contains("REAL") {
            return Self::Float;
        }
        if t.contains("INT")
            || t.contains("SERIAL")
            || t.contains("OID")
            || t == "YEAR"
            || t == "INTEGER"
        {
            return Self::Int;
        }
        if t.contains("TEXT")
            || t.contains("CHAR")
            || t.contains("CLOB")
            || t.contains("CITEXT")
            || t.contains("VARCHAR")
            || t.contains("NVARCHAR")
            || t == "NAME"
            || t.contains("XML")
            || t.contains("STRING")
        {
            return Self::Text;
        }
        Self::Other
    }

    /// Operators offered for this kind (M1-3). Pattern ops only on [`Self::Text`].
    pub fn operators(self) -> &'static [FilterOp] {
        use FilterOp::*;
        match self {
            Self::Bool => &[Eq, Ne, IsNull, IsNotNull],
            Self::Int | Self::Float | Self::Decimal => {
                &[Eq, Ne, Gt, Ge, Lt, Le, In, IsNull, IsNotNull]
            }
            Self::Date | Self::Time | Self::Timestamp => {
                &[Eq, Ne, Gt, Ge, Lt, Le, IsNull, IsNotNull]
            }
            Self::Interval => &[Eq, Ne, Gt, Lt, IsNull, IsNotNull],
            Self::Text => &[Eq, Ne, Like, NotLike, In, IsNull, IsNotNull],
            Self::Uuid => &[Eq, Ne, In, IsNull, IsNotNull],
            Self::Json => &[Eq, IsNull, IsNotNull],
            Self::Bytes => &[IsNull, IsNotNull],
            Self::Other => &[Eq, Ne, IsNull, IsNotNull],
        }
    }

    pub fn allows(self, op: FilterOp) -> bool {
        if self.operators().contains(&op) {
            return true;
        }
        // Legacy convenience pattern ops (tabs.json) remain valid on text even
        // though the filter menu only offers LIKE / NOT LIKE.
        self == Self::Text
            && matches!(
                op,
                FilterOp::Contains
                    | FilterOp::NotContains
                    | FilterOp::StartsWith
                    | FilterOp::EndsWith
            )
    }

    /// Next operator in this kind's list (cycles).
    pub fn next_op(self, current: FilterOp) -> FilterOp {
        let ops = self.operators();
        let ix = ops.iter().position(|o| *o == current).unwrap_or(0);
        ops[(ix + 1) % ops.len()]
    }
}

/// Maximum `ORDER BY` keys for table header multi-sort (M1-3).
pub const MAX_SORT_KEYS: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_engine_type_strings() {
        assert_eq!(ColumnKind::from_type_name("INT4"), ColumnKind::Int);
        assert_eq!(ColumnKind::from_type_name("INTEGER"), ColumnKind::Int);
        assert_eq!(
            ColumnKind::from_type_name("BIGINT UNSIGNED"),
            ColumnKind::Int
        );
        assert_eq!(ColumnKind::from_type_name("VARCHAR(255)"), ColumnKind::Text);
        assert_eq!(ColumnKind::from_type_name("TEXT"), ColumnKind::Text);
        assert_eq!(
            ColumnKind::from_type_name("TIMESTAMPTZ"),
            ColumnKind::Timestamp
        );
        assert_eq!(
            ColumnKind::from_type_name("TIMESTAMP"),
            ColumnKind::Timestamp
        );
        assert_eq!(
            ColumnKind::from_type_name("DATETIME"),
            ColumnKind::Timestamp
        );
        assert_eq!(ColumnKind::from_type_name("TIME"), ColumnKind::Time);
        assert_eq!(ColumnKind::from_type_name("DATE"), ColumnKind::Date);
        assert_eq!(ColumnKind::from_type_name("JSONB"), ColumnKind::Json);
        assert_eq!(ColumnKind::from_type_name("BYTEA"), ColumnKind::Bytes);
        assert_eq!(ColumnKind::from_type_name("BLOB"), ColumnKind::Bytes);
        assert_eq!(ColumnKind::from_type_name("BOOL"), ColumnKind::Bool);
        assert_eq!(ColumnKind::from_type_name("UUID"), ColumnKind::Uuid);
        assert_eq!(
            ColumnKind::from_type_name("NUMERIC(10,2)"),
            ColumnKind::Decimal
        );
    }

    #[test]
    fn like_ops_only_on_text() {
        assert!(ColumnKind::Text.allows(FilterOp::Like));
        assert!(ColumnKind::Text.allows(FilterOp::NotLike));
        assert!(!ColumnKind::Int.allows(FilterOp::Like));
        assert!(!ColumnKind::Bool.allows(FilterOp::Like));
        assert!(!ColumnKind::Json.allows(FilterOp::Like));
        // Convenience wraps stay off the menu but remain allowed for restore.
        assert!(ColumnKind::Text.allows(FilterOp::Contains));
        assert!(!ColumnKind::Int.allows(FilterOp::Contains));
        assert!(ColumnKind::Bytes.allows(FilterOp::IsNull));
        assert!(!ColumnKind::Bytes.allows(FilterOp::Eq));
    }
}
