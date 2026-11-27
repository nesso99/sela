//! Read-only table structure: columns, indexes, foreign keys, and DDL.

/// One column from [`TableStructure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnStructure {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
    /// True when the engine reports the column as identity, generated, or
    /// auto-increment (drivers set this from catalog metadata).
    pub generated: bool,
}

/// An index on a table (including the primary key when the engine reports it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    /// Partial index (has a predicate): uniqueness only holds for a subset of rows.
    pub partial: bool,
    /// Index has an expression member (not a plain column), so `columns` is incomplete.
    pub expression: bool,
    /// Engine-provided definition (`CREATE INDEX …` / `pg_get_indexdef`), when available.
    pub definition: Option<String>,
}

/// A foreign-key relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
}

/// Full read-only description of a table for the Structure view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableStructure {
    pub schema: String,
    pub table: String,
    pub columns: Vec<ColumnStructure>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Full DDL for the table (and related indexes when the engine provides them).
    pub ddl: String,
}

impl TableStructure {
    /// Primary-key column names in key order, when the engine reported a PK index.
    ///
    /// Returns `None` when there is no primary key (table is not grid-editable).
    pub fn primary_key_columns(&self) -> Option<Vec<String>> {
        self.indexes
            .iter()
            .find(|idx| idx.primary && !idx.columns.is_empty())
            .map(|idx| idx.columns.clone())
    }

    /// Columns that identify a row for editing: the primary key when present,
    /// otherwise the smallest plain unique index whose columns are all
    /// `NOT NULL` (ties broken by index name). Partial indexes are skipped.
    ///
    /// Returns `None` when neither exists (table is not grid-editable).
    pub fn row_key_columns(&self) -> Option<Vec<String>> {
        if let Some(pk) = self.primary_key_columns() {
            return Some(pk);
        }
        self.indexes
            .iter()
            .filter(|idx| idx.unique && !idx.primary && !idx.columns.is_empty())
            .filter(|idx| !idx.partial && !idx.expression)
            .filter(|idx| {
                idx.columns.iter().all(|c| {
                    self.columns
                        .iter()
                        .any(|col| col.name == *c && !col.nullable)
                })
            })
            .min_by(|a, b| {
                a.columns
                    .len()
                    .cmp(&b.columns.len())
                    .then_with(|| a.name.cmp(&b.name))
            })
            .map(|idx| idx.columns.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, nullable: bool) -> ColumnStructure {
        ColumnStructure {
            name: name.into(),
            type_name: "text".into(),
            nullable,
            default: None,
            comment: None,
            generated: false,
        }
    }

    fn uidx(name: &str, cols: &[&str], def: Option<&str>) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            columns: cols.iter().map(|c| c.to_string()).collect(),
            unique: true,
            primary: false,
            partial: false,
            expression: false,
            definition: def.map(str::to_string),
        }
    }

    fn table(columns: Vec<ColumnStructure>, indexes: Vec<IndexInfo>) -> TableStructure {
        TableStructure {
            schema: "main".into(),
            table: "t".into(),
            columns,
            indexes,
            foreign_keys: vec![],
            ddl: String::new(),
        }
    }

    #[test]
    fn row_key_prefers_primary_key() {
        let mut pk = uidx("pk", &["a"], None);
        pk.primary = true;
        let t = table(
            vec![col("a", false), col("b", false)],
            vec![uidx("u", &["b"], None), pk],
        );
        assert_eq!(t.row_key_columns(), Some(vec!["a".to_string()]));
    }

    #[test]
    fn row_key_falls_back_to_unique_not_null() {
        let t = table(
            vec![col("a", false), col("b", false), col("c", false)],
            vec![uidx("u_ab", &["a", "b"], None), uidx("u_c", &["c"], None)],
        );
        assert_eq!(t.primary_key_columns(), None);
        assert_eq!(t.row_key_columns(), Some(vec!["c".to_string()]));
    }

    #[test]
    fn row_key_rejects_flagged_partial_and_expression_indexes() {
        let mut partial = uidx("u_p", &["a"], None);
        partial.partial = true;
        let mut expr = uidx("u_e", &["a"], None); // e.g. UNIQUE (a, lower(b)) -> columns [a]
        expr.expression = true;
        let t = table(vec![col("a", false)], vec![partial, expr]);
        assert_eq!(t.row_key_columns(), None);
    }

    #[test]
    fn row_key_rejects_nullable_partial_and_non_unique() {
        let mut non_unique = uidx("n", &["a"], None);
        non_unique.unique = false;
        let t = table(
            vec![col("a", false), col("b", true), col("c", false)],
            vec![non_unique, uidx("u_b", &["b"], None), {
                let mut i = uidx(
                    "u_c",
                    &["c"],
                    Some("CREATE UNIQUE INDEX u_c ON t (c) WHERE c > 0"),
                );
                i.partial = true;
                i
            }],
        );
        assert_eq!(t.row_key_columns(), None);
    }
}
