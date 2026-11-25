//! SQL autocomplete: catalog + buffer + caret → ranked suggestions (M3-2).
//!
//! Pure functions only — no GPUI, no I/O. The app wires a [`SchemaSnapshot`] from
//! the connection catalog cache and asks [`complete`] / [`should_auto_trigger`].

use std::collections::{HashMap, HashSet};

use crate::sql::{
    first_keyword, quote_ident, split_statements, statement_index_at, strip_comments,
};
use crate::types::{ColumnMeta, Dialect};

/// How the catalog cache looks to the completion engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogStatus {
    /// Schema load in flight. UI shows a single non-insertable row.
    Loading,
    /// Finished load with at least one schema or table.
    Ready(SchemaSnapshot),
    /// Finished load with nothing usable.
    Empty,
}

/// Schemas, tables, and columns from the connection catalog (same source as the sidebar).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaSnapshot {
    pub schemas: Vec<SchemaEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaEntry {
    pub name: String,
    pub tables: Vec<TableEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableEntry {
    pub name: String,
    pub columns: Vec<ColumnMeta>,
}

/// Kind shown as the muted right-hand label (and used for ranking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Schema,
    Table,
    Column,
    Alias,
    /// Status row (`Loading schema…` / `No schema loaded.`) — must not insert.
    Status,
}

/// One ranked suggestion for the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionSuggestion {
    /// Monospace name in the row.
    pub label: String,
    /// Muted label on the right: type / `table` / `schema` / `keyword`.
    pub detail: String,
    pub kind: CompletionKind,
    /// Text inserted on accept (dialect-quoted when needed). Empty for status rows.
    pub insert: String,
    /// Byte length of the unquoted identifier fragment (or empty after `.`) to replace.
    pub replace_len: usize,
}

/// Context at the caret used by ranking and triggers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionContext {
    /// True when the caret sits inside a string literal or SQL comment.
    pub in_string_or_comment: bool,
    /// Unquoted identifier fragment immediately before the caret (may be empty).
    pub fragment: String,
    /// Byte start of [`Self::fragment`] in the buffer.
    pub fragment_start: usize,
    /// Qualifier before a trailing `.` (`alias.`, `table.`, `schema.`), if any.
    pub qualifier: Option<String>,
    /// True when the character immediately before the caret (ignoring the fragment) is `.`.
    pub after_dot: bool,
}

const MAX_SUGGESTIONS: usize = 256;
const LOADING_LABEL: &str = "Loading schema…";
const EMPTY_LABEL: &str = "No schema loaded.";

/// DDL first keywords that refresh the catalog (case-insensitive; after comments).
const DDL_REFRESH_KEYWORDS: &[&str] = &[
    "create", "alter", "drop", "truncate", "comment", "rename", "attach", "detach",
];

/// Build suggestions for the caret in `buffer`.
///
/// `active_schema` scopes unqualified table names (sidebar schema / search_path / USE).
pub fn complete(
    dialect: Dialect,
    catalog: &CatalogStatus,
    buffer: &str,
    caret: usize,
    active_schema: Option<&str>,
) -> Vec<CompletionSuggestion> {
    let caret = caret.min(buffer.len());
    let ctx = analyze_context(dialect, buffer, caret);
    if ctx.in_string_or_comment {
        return Vec::new();
    }

    match catalog {
        CatalogStatus::Loading => {
            return vec![status_row(LOADING_LABEL, ctx.fragment.len())];
        }
        CatalogStatus::Empty => {
            // Still offer keywords so typing works before a catalog exists.
            let mut out = keyword_suggestions(dialect, &ctx);
            if out.is_empty() && ctx.fragment.is_empty() && !ctx.after_dot {
                out.push(status_row(EMPTY_LABEL, 0));
            }
            return out;
        }
        CatalogStatus::Ready(_) => {}
    }

    let CatalogStatus::Ready(snapshot) = catalog else {
        unreachable!();
    };

    let statement = current_statement_text(buffer, caret, dialect);
    let aliases = extract_aliases(&statement);
    let tables_in_statement = tables_named_in_statement(&statement, snapshot, active_schema);

    let mut scored: Vec<(i32, CompletionSuggestion)> = Vec::new();

    if let Some(qual) = ctx.qualifier.as_deref() {
        // After `alias.` / `table.` / `schema.` — columns (or tables of a schema) first.
        push_qualified(
            dialect,
            snapshot,
            qual,
            &ctx,
            &aliases,
            active_schema,
            &mut scored,
        );
    } else {
        push_unqualified(
            dialect,
            snapshot,
            &ctx,
            &aliases,
            &tables_in_statement,
            active_schema,
            &mut scored,
        );
    }

    // Keywords: below identifiers unless the fragment itself is a keyword prefix.
    let fragment_is_kw_prefix = !ctx.fragment.is_empty()
        && dialect_keywords(dialect)
            .iter()
            .any(|kw| kw.starts_with(&ctx.fragment.to_ascii_lowercase()));
    for item in keyword_suggestions(dialect, &ctx) {
        let score = rank_score(
            &ctx.fragment,
            &item.label,
            item.kind,
            false,
            fragment_is_kw_prefix,
        );
        scored.push((score, item));
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
    scored.truncate(MAX_SUGGESTIONS);
    scored.into_iter().map(|(_, s)| s).collect()
}

/// Auto-open after `.` or after two identifier characters (AU-01 / AU-02 / AU-03).
pub fn should_auto_trigger(dialect: Dialect, buffer: &str, caret: usize) -> bool {
    let caret = caret.min(buffer.len());
    let ctx = analyze_context(dialect, buffer, caret);
    if ctx.in_string_or_comment {
        return false;
    }
    if ctx.after_dot && ctx.fragment.is_empty() {
        return true;
    }
    identifier_prefix_len(dialect, &ctx.fragment) >= 2
}

/// True when a finished editor statement should refresh the catalog (AU-16).
pub fn is_catalog_refresh_statement(sql: &str) -> bool {
    let Some(kw) = first_keyword(sql) else {
        return false;
    };
    DDL_REFRESH_KEYWORDS.contains(&kw.as_str())
        || kw == "use"
        || (kw == "set"
            && strip_comments(sql)
                .to_ascii_lowercase()
                .contains("search_path"))
}

/// Quote `ident` when the dialect requires it for a valid insert.
pub fn quote_ident_if_needed(dialect: Dialect, ident: &str) -> String {
    if ident.is_empty() {
        return String::new();
    }
    if needs_quoting(dialect, ident) {
        quote_ident(dialect, ident).unwrap_or_else(|_| ident.to_string())
    } else {
        ident.to_string()
    }
}

/// Parse trigger / fragment / qualifier at `caret`.
pub fn analyze_context(dialect: Dialect, buffer: &str, caret: usize) -> CompletionContext {
    let caret = caret.min(buffer.len());
    if in_string_or_comment(buffer, caret) {
        return CompletionContext {
            in_string_or_comment: true,
            fragment: String::new(),
            fragment_start: caret,
            qualifier: None,
            after_dot: false,
        };
    }

    let before = &buffer[..caret];
    let fragment_start = scan_ident_start(dialect, before);
    let fragment = before[fragment_start..].to_string();

    let mut after_dot = false;
    let mut qualifier = None;
    let prefix = &before[..fragment_start];
    let trimmed = prefix.trim_end();
    if let Some(ch) = trimmed.chars().last() {
        if ch == '.' {
            after_dot = true;
            let without_dot = &trimmed[..trimmed.len() - ch.len_utf8()];
            let q_start = scan_ident_start(dialect, without_dot);
            let q = without_dot[q_start..].trim();
            // Strip surrounding quotes from the qualifier token.
            let q = unquote_ident(dialect, q);
            if !q.is_empty() {
                qualifier = Some(q);
            }
        }
    }

    CompletionContext {
        in_string_or_comment: false,
        fragment,
        fragment_start,
        qualifier,
        after_dot,
    }
}

fn status_row(label: &str, replace_len: usize) -> CompletionSuggestion {
    CompletionSuggestion {
        label: label.into(),
        detail: String::new(),
        kind: CompletionKind::Status,
        insert: String::new(),
        replace_len,
    }
}

fn keyword_suggestions(dialect: Dialect, ctx: &CompletionContext) -> Vec<CompletionSuggestion> {
    if ctx.after_dot {
        return Vec::new();
    }
    let frag = ctx.fragment.to_ascii_lowercase();
    dialect_keywords(dialect)
        .iter()
        .filter(|kw| frag.is_empty() || kw.starts_with(&frag) || kw.contains(&frag))
        .map(|kw| CompletionSuggestion {
            label: kw.to_ascii_uppercase(),
            detail: "keyword".into(),
            kind: CompletionKind::Keyword,
            insert: kw.to_ascii_uppercase(),
            replace_len: ctx.fragment.len(),
        })
        .collect()
}

fn push_qualified(
    dialect: Dialect,
    snapshot: &SchemaSnapshot,
    qual: &str,
    ctx: &CompletionContext,
    aliases: &HashMap<String, (Option<String>, String)>,
    active_schema: Option<&str>,
    scored: &mut Vec<(i32, CompletionSuggestion)>,
) {
    // Alias → that relation's columns first.
    if let Some((schema, table)) = aliases.get(&qual.to_ascii_lowercase()) {
        let schema_name = schema.as_deref().or(active_schema).unwrap_or_default();
        if let Some(cols) = find_columns(snapshot, schema_name, table) {
            for col in cols {
                push_column(dialect, col, ctx, true, scored);
            }
            return;
        }
    }

    // Bare table name → its columns.
    if let Some((schema, table)) = find_table(snapshot, qual, active_schema) {
        for col in &table.columns {
            push_column(dialect, col, ctx, true, scored);
        }
        // Also allow schema.table when qual is a schema.
        let _ = schema;
        return;
    }

    // Schema name → its tables.
    if let Some(schema) = snapshot
        .schemas
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(qual))
    {
        for table in &schema.tables {
            if matches_fragment(&ctx.fragment, &table.name) {
                let score = rank_score(
                    &ctx.fragment,
                    &table.name,
                    CompletionKind::Table,
                    true,
                    false,
                );
                scored.push((
                    score,
                    CompletionSuggestion {
                        label: table.name.clone(),
                        detail: "table".into(),
                        kind: CompletionKind::Table,
                        insert: quote_ident_if_needed(dialect, &table.name),
                        replace_len: ctx.fragment.len(),
                    },
                ));
            }
        }
    }
}

fn push_unqualified(
    dialect: Dialect,
    snapshot: &SchemaSnapshot,
    ctx: &CompletionContext,
    aliases: &HashMap<String, (Option<String>, String)>,
    tables_in_statement: &[(String, String)],
    active_schema: Option<&str>,
    scored: &mut Vec<(i32, CompletionSuggestion)>,
) {
    for alias in aliases.keys() {
        if matches_fragment(&ctx.fragment, alias) {
            let score = rank_score(&ctx.fragment, alias, CompletionKind::Alias, false, false);
            scored.push((
                score,
                CompletionSuggestion {
                    label: alias.clone(),
                    detail: "alias".into(),
                    kind: CompletionKind::Alias,
                    insert: quote_ident_if_needed(dialect, alias),
                    replace_len: ctx.fragment.len(),
                },
            ));
        }
    }

    for schema in &snapshot.schemas {
        if matches_fragment(&ctx.fragment, &schema.name) {
            let score = rank_score(
                &ctx.fragment,
                &schema.name,
                CompletionKind::Schema,
                false,
                false,
            );
            scored.push((
                score,
                CompletionSuggestion {
                    label: schema.name.clone(),
                    detail: "schema".into(),
                    kind: CompletionKind::Schema,
                    insert: quote_ident_if_needed(dialect, &schema.name),
                    replace_len: ctx.fragment.len(),
                },
            ));
        }
        let prefer_schema = active_schema
            .map(|a| a.eq_ignore_ascii_case(&schema.name))
            .unwrap_or(true);
        for table in &schema.tables {
            if matches_fragment(&ctx.fragment, &table.name) {
                let in_stmt = tables_in_statement.iter().any(|(s, t)| {
                    s.eq_ignore_ascii_case(&schema.name) && t.eq_ignore_ascii_case(&table.name)
                });
                let score = rank_score(
                    &ctx.fragment,
                    &table.name,
                    CompletionKind::Table,
                    in_stmt || prefer_schema,
                    false,
                );
                scored.push((
                    score,
                    CompletionSuggestion {
                        label: table.name.clone(),
                        detail: "table".into(),
                        kind: CompletionKind::Table,
                        insert: quote_ident_if_needed(dialect, &table.name),
                        replace_len: ctx.fragment.len(),
                    },
                ));
            }
            for col in &table.columns {
                let in_stmt = tables_in_statement.iter().any(|(s, t)| {
                    s.eq_ignore_ascii_case(&schema.name) && t.eq_ignore_ascii_case(&table.name)
                });
                if matches_fragment(&ctx.fragment, &col.name) {
                    push_column_scored(dialect, col, ctx, in_stmt, scored);
                }
            }
        }
    }
}

fn push_column(
    dialect: Dialect,
    col: &ColumnMeta,
    ctx: &CompletionContext,
    relation_local: bool,
    scored: &mut Vec<(i32, CompletionSuggestion)>,
) {
    if !matches_fragment(&ctx.fragment, &col.name) && !ctx.fragment.is_empty() {
        return;
    }
    push_column_scored(dialect, col, ctx, relation_local, scored);
}

fn push_column_scored(
    dialect: Dialect,
    col: &ColumnMeta,
    ctx: &CompletionContext,
    relation_local: bool,
    scored: &mut Vec<(i32, CompletionSuggestion)>,
) {
    let score = rank_score(
        &ctx.fragment,
        &col.name,
        CompletionKind::Column,
        relation_local,
        false,
    );
    scored.push((
        score,
        CompletionSuggestion {
            label: col.name.clone(),
            detail: col.type_name.clone(),
            kind: CompletionKind::Column,
            insert: quote_ident_if_needed(dialect, &col.name),
            replace_len: ctx.fragment.len(),
        },
    ));
}

/// Higher is better.
fn rank_score(
    fragment: &str,
    label: &str,
    kind: CompletionKind,
    relation_boost: bool,
    keyword_prefix_boost: bool,
) -> i32 {
    let frag = fragment.to_ascii_lowercase();
    let name = label.to_ascii_lowercase();
    let mut score = 0i32;
    if frag.is_empty() {
        score += 10;
    } else if name.starts_with(&frag) {
        score += 400; // (1) case-insensitive prefix
    } else if name.contains(&frag) {
        score += 200; // (3) substring
    } else {
        return -1_000;
    }
    if relation_boost {
        score += 80; // (2) columns/tables of current statement
    }
    match kind {
        CompletionKind::Column => score += 30,
        CompletionKind::Alias => score += 28,
        CompletionKind::Table => score += 20,
        CompletionKind::Schema => score += 15,
        CompletionKind::Keyword => {
            // (4) keywords below identifiers unless fragment is a keyword prefix
            if keyword_prefix_boost && name.starts_with(&frag) {
                score += 350;
            } else {
                score -= 40;
            }
        }
        CompletionKind::Status => score = 0,
    }
    score
}

fn matches_fragment(fragment: &str, name: &str) -> bool {
    if fragment.is_empty() {
        return true;
    }
    let frag = fragment.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    n.starts_with(&frag) || n.contains(&frag)
}

fn identifier_prefix_len(dialect: Dialect, fragment: &str) -> usize {
    fragment
        .chars()
        .filter(|ch| is_ident_char(dialect, *ch))
        .count()
}

fn is_ident_char(dialect: Dialect, ch: char) -> bool {
    ch.is_alphabetic()
        || matches!(ch, '0'..='9' | '_')
        || (matches!(dialect, Dialect::Mysql) && ch == '$')
}

fn scan_ident_start(dialect: Dialect, before: &str) -> usize {
    let mut start = before.len();
    for (ix, ch) in before.char_indices().rev() {
        if is_ident_char(dialect, ch) {
            start = ix;
        } else {
            break;
        }
    }
    start
}

fn unquote_ident(dialect: Dialect, raw: &str) -> String {
    let raw = raw.trim();
    match dialect {
        Dialect::Mysql => {
            if raw.len() >= 2 && raw.starts_with('`') && raw.ends_with('`') {
                raw[1..raw.len() - 1].replace("``", "`")
            } else {
                raw.to_string()
            }
        }
        Dialect::Postgres | Dialect::Sqlite => {
            if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
                raw[1..raw.len() - 1].replace("\"\"", "\"")
            } else {
                raw.to_string()
            }
        }
    }
}

fn needs_quoting(dialect: Dialect, ident: &str) -> bool {
    if ident.is_empty() {
        return true;
    }
    let mut chars = ident.chars();
    let Some(first) = chars.next() else {
        return true;
    };
    let start_ok = first.is_ascii_alphabetic() || first == '_';
    if !start_ok {
        return true;
    }
    if !ident.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || ch == '_' || (matches!(dialect, Dialect::Mysql) && ch == '$')
    }) {
        return true;
    }
    // Mixed case / reserved → quote so the insert round-trips.
    if ident.chars().any(|ch| ch.is_ascii_uppercase())
        && ident.chars().any(|ch| ch.is_ascii_lowercase())
    {
        return true;
    }
    let lower = ident.to_ascii_lowercase();
    dialect_keywords(dialect)
        .binary_search(&lower.as_str())
        .is_ok()
}

fn in_string_or_comment(buffer: &str, caret: usize) -> bool {
    let bytes = buffer.as_bytes();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    while i < caret && i < bytes.len() {
        let b = bytes[i];
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_double = false;
            }
            i += 1;
            continue;
        }
        if in_backtick {
            if b == b'`' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'`' {
                    i += 2;
                    continue;
                }
                in_backtick = false;
            }
            i += 1;
            continue;
        }
        if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            in_block_comment = true;
            i += 2;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'`' => in_backtick = true,
            _ => {}
        }
        i += 1;
    }
    in_single || in_double || in_backtick || in_line_comment || in_block_comment
}

fn current_statement_text(buffer: &str, caret: usize, dialect: Dialect) -> String {
    let Ok(stmts) = split_statements(buffer, dialect) else {
        return buffer.to_string();
    };
    if let Some(ix) = statement_index_at(&stmts, caret) {
        return stmts[ix].text.clone();
    }
    buffer.to_string()
}

/// Map alias (lower) → (optional schema, table name).
fn extract_aliases(statement: &str) -> HashMap<String, (Option<String>, String)> {
    let mut out = HashMap::new();
    let stripped = strip_comments(statement);
    let tokens = tokenize_idents(&stripped);
    let mut i = 0usize;
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        if tok.eq_ignore_ascii_case("from") || tok.eq_ignore_ascii_case("join") {
            i += 1;
            let Some((schema, table, next)) = parse_relation(&tokens, i) else {
                continue;
            };
            i = next;
            let alias = if i < tokens.len() {
                let t = tokens[i].as_str();
                if t.eq_ignore_ascii_case("as") {
                    i += 1;
                    tokens.get(i).cloned()
                } else if !is_sql_clause_keyword(t) {
                    Some(tokens[i].clone())
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(alias) = alias {
                i += 1;
                out.insert(alias.to_ascii_lowercase(), (schema, table));
            }
            continue;
        }
        i += 1;
    }
    out
}

fn parse_relation(tokens: &[String], start: usize) -> Option<(Option<String>, String, usize)> {
    let first = tokens.get(start)?;
    if tokens.get(start + 1).map(|s| s.as_str()) == Some(".") {
        let second = tokens.get(start + 2)?;
        Some((Some(first.clone()), second.clone(), start + 3))
    } else {
        Some((None, first.clone(), start + 1))
    }
}

fn tables_named_in_statement(
    statement: &str,
    snapshot: &SchemaSnapshot,
    active_schema: Option<&str>,
) -> Vec<(String, String)> {
    let aliases = extract_aliases(statement);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (schema, table) in aliases.values() {
        let schema_name = schema
            .clone()
            .or_else(|| active_schema.map(str::to_string))
            .unwrap_or_default();
        let key = (schema_name.to_ascii_lowercase(), table.to_ascii_lowercase());
        if seen.insert(key.clone()) {
            out.push((schema_name, table.clone()));
        }
    }
    // Also pick up bare table tokens that match the catalog.
    for tok in tokenize_idents(statement) {
        if is_sql_clause_keyword(&tok) {
            continue;
        }
        if let Some((schema, table)) = find_table(snapshot, &tok, active_schema) {
            let key = (
                schema.name.to_ascii_lowercase(),
                table.name.to_ascii_lowercase(),
            );
            if seen.insert(key) {
                out.push((schema.name.clone(), table.name.clone()));
            }
        }
    }
    out
}

fn find_table<'a>(
    snapshot: &'a SchemaSnapshot,
    name: &str,
    active_schema: Option<&str>,
) -> Option<(&'a SchemaEntry, &'a TableEntry)> {
    if let Some(active) = active_schema {
        if let Some(schema) = snapshot
            .schemas
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(active))
        {
            if let Some(table) = schema
                .tables
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(name))
            {
                return Some((schema, table));
            }
        }
    }
    for schema in &snapshot.schemas {
        if let Some(table) = schema
            .tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
        {
            return Some((schema, table));
        }
    }
    None
}

fn find_columns<'a>(
    snapshot: &'a SchemaSnapshot,
    schema: &str,
    table: &str,
) -> Option<&'a [ColumnMeta]> {
    let schema_entry = if schema.is_empty() {
        snapshot.schemas.first()
    } else {
        snapshot
            .schemas
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(schema))
    }?;
    schema_entry
        .tables
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(table))
        .map(|t| t.columns.as_slice())
}

fn tokenize_idents(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in sql.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            buf.push(ch);
            continue;
        }
        if !buf.is_empty() {
            out.push(std::mem::take(&mut buf));
        }
        if ch == '.' {
            out.push(".".into());
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

fn is_sql_clause_keyword(tok: &str) -> bool {
    matches!(
        tok.to_ascii_lowercase().as_str(),
        "select"
            | "from"
            | "where"
            | "join"
            | "inner"
            | "left"
            | "right"
            | "full"
            | "outer"
            | "cross"
            | "on"
            | "and"
            | "or"
            | "group"
            | "order"
            | "by"
            | "limit"
            | "offset"
            | "having"
            | "union"
            | "except"
            | "intersect"
            | "into"
            | "values"
            | "set"
            | "as"
            | "with"
            | "using"
            | "natural"
    )
}

/// Dialect keywords for autocomplete (sorted for binary_search in [`needs_quoting`]).
pub fn dialect_keywords(dialect: Dialect) -> &'static [&'static str] {
    match dialect {
        Dialect::Postgres => PG_KEYWORDS,
        Dialect::Mysql => MYSQL_KEYWORDS,
        Dialect::Sqlite => SQLITE_KEYWORDS,
    }
}

macro_rules! keywords {
    ($($kw:literal),* $(,)?) => {
        &[$($kw),*]
    };
}

// Keep sorted ASC for binary_search.
const PG_KEYWORDS: &[&str] = keywords![
    "all",
    "alter",
    "analyze",
    "and",
    "any",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "case",
    "cast",
    "check",
    "coalesce",
    "column",
    "commit",
    "constraint",
    "create",
    "cross",
    "current",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "except",
    "exists",
    "false",
    "fetch",
    "for",
    "foreign",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "ilike",
    "in",
    "index",
    "inner",
    "insert",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "nulls",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "over",
    "primary",
    "references",
    "returning",
    "right",
    "rollback",
    "select",
    "set",
    "table",
    "then",
    "true",
    "truncate",
    "union",
    "unique",
    "update",
    "using",
    "values",
    "when",
    "where",
    "with",
];

const MYSQL_KEYWORDS: &[&str] = keywords![
    "all",
    "alter",
    "and",
    "as",
    "asc",
    "between",
    "by",
    "case",
    "cast",
    "change",
    "character",
    "check",
    "collate",
    "column",
    "create",
    "cross",
    "database",
    "default",
    "delete",
    "desc",
    "describe",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "false",
    "for",
    "foreign",
    "from",
    "full",
    "group",
    "having",
    "if",
    "ignore",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "references",
    "replace",
    "right",
    "select",
    "set",
    "show",
    "table",
    "then",
    "true",
    "truncate",
    "union",
    "unique",
    "update",
    "use",
    "using",
    "values",
    "when",
    "where",
    "with",
];

const SQLITE_KEYWORDS: &[&str] = keywords![
    "abort",
    "add",
    "all",
    "alter",
    "and",
    "as",
    "asc",
    "attach",
    "begin",
    "between",
    "by",
    "case",
    "cast",
    "check",
    "collate",
    "column",
    "commit",
    "conflict",
    "constraint",
    "create",
    "cross",
    "default",
    "deferred",
    "delete",
    "desc",
    "detach",
    "distinct",
    "drop",
    "else",
    "end",
    "escape",
    "except",
    "exists",
    "fail",
    "for",
    "foreign",
    "from",
    "full",
    "glob",
    "group",
    "having",
    "if",
    "ignore",
    "in",
    "index",
    "indexed",
    "inner",
    "insert",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "notnull",
    "null",
    "on",
    "or",
    "order",
    "outer",
    "plan",
    "pragma",
    "primary",
    "query",
    "raise",
    "references",
    "regexp",
    "right",
    "rollback",
    "row",
    "select",
    "set",
    "table",
    "temp",
    "temporary",
    "then",
    "transaction",
    "trigger",
    "union",
    "unique",
    "update",
    "using",
    "vacuum",
    "values",
    "view",
    "virtual",
    "when",
    "where",
    "with",
    "without",
];

/// One schema's tables and columns for [`snapshot_from_catalog`].
type CatalogSchemaInput = (String, Vec<(String, Vec<ColumnMeta>)>);

/// Build a [`SchemaSnapshot`] from loaded sidebar schemas plus optional column maps.
pub fn snapshot_from_catalog(schemas: &[CatalogSchemaInput]) -> SchemaSnapshot {
    SchemaSnapshot {
        schemas: schemas
            .iter()
            .map(|(name, tables)| SchemaEntry {
                name: name.clone(),
                tables: tables
                    .iter()
                    .map(|(tname, cols)| TableEntry {
                        name: tname.clone(),
                        columns: cols.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn books_catalog() -> CatalogStatus {
        CatalogStatus::Ready(SchemaSnapshot {
            schemas: vec![SchemaEntry {
                name: "public".into(),
                tables: vec![
                    TableEntry {
                        name: "books".into(),
                        columns: vec![
                            ColumnMeta {
                                name: "id".into(),
                                type_name: "integer".into(),
                            },
                            ColumnMeta {
                                name: "title".into(),
                                type_name: "text".into(),
                            },
                            ColumnMeta {
                                name: "author_id".into(),
                                type_name: "integer".into(),
                            },
                        ],
                    },
                    TableEntry {
                        name: "authors".into(),
                        columns: vec![
                            ColumnMeta {
                                name: "id".into(),
                                type_name: "integer".into(),
                            },
                            ColumnMeta {
                                name: "name".into(),
                                type_name: "text".into(),
                            },
                        ],
                    },
                    TableEntry {
                        name: "Odd Name".into(),
                        columns: vec![ColumnMeta {
                            name: "Weird Col".into(),
                            type_name: "text".into(),
                        }],
                    },
                ],
            }],
        })
    }

    #[test]
    fn au01_auto_trigger_needs_two_ident_chars() {
        assert!(!should_auto_trigger(Dialect::Postgres, "s", 1));
        assert!(should_auto_trigger(Dialect::Postgres, "se", 2));
        assert!(!should_auto_trigger(Dialect::Postgres, "s!", 2));
    }

    #[test]
    fn au01_mysql_dollar_counts() {
        assert!(!should_auto_trigger(Dialect::Mysql, "$", 1));
        assert!(should_auto_trigger(Dialect::Mysql, "$a", 2));
        assert!(!should_auto_trigger(Dialect::Postgres, "$a", 2));
    }

    #[test]
    fn au02_dot_triggers_immediately() {
        assert!(should_auto_trigger(Dialect::Postgres, "b.", 2));
        assert!(should_auto_trigger(Dialect::Postgres, "books.", 6));
    }

    #[test]
    fn au03_no_trigger_in_comment_or_string() {
        assert!(!should_auto_trigger(Dialect::Postgres, "-- se", 5));
        assert!(!should_auto_trigger(Dialect::Postgres, "/* se", 5));
        assert!(!should_auto_trigger(Dialect::Postgres, "'se", 3));
        assert!(complete(
            Dialect::Postgres,
            &books_catalog(),
            "-- se",
            5,
            Some("public")
        )
        .is_empty());
    }

    #[test]
    fn au07_alias_columns_first() {
        let sql = "SELECT a. FROM books AS a";
        let caret = sql.find('.').unwrap() + 1;
        let items = complete(
            Dialect::Postgres,
            &books_catalog(),
            sql,
            caret,
            Some("public"),
        );
        assert!(!items.is_empty());
        assert_eq!(items[0].kind, CompletionKind::Column);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"id"));
        assert!(labels.contains(&"title"));
        // Unrelated authors.name should not lead.
        assert_ne!(items[0].label, "name");
    }

    #[test]
    fn au08_prefix_and_keyword_ranking() {
        let items = complete(
            Dialect::Postgres,
            &books_catalog(),
            "SEL",
            3,
            Some("public"),
        );
        assert_eq!(items[0].label, "SELECT");
        let items = complete(Dialect::Postgres, &books_catalog(), "bo", 2, Some("public"));
        assert_eq!(items[0].label, "books");
        assert_eq!(items[0].kind, CompletionKind::Table);
    }

    #[test]
    fn au08_statement_columns_rank_above_others() {
        let sql = "SELECT ti FROM books";
        let caret = "SELECT ti".len();
        let items = complete(
            Dialect::Postgres,
            &books_catalog(),
            sql,
            caret,
            Some("public"),
        );
        assert_eq!(items[0].label, "title");
    }

    #[test]
    fn au09_quoting_mixed_case_and_reserved() {
        assert_eq!(
            quote_ident_if_needed(Dialect::Postgres, "Odd Name"),
            "\"Odd Name\""
        );
        assert_eq!(
            quote_ident_if_needed(Dialect::Mysql, "Odd Name"),
            "`Odd Name`"
        );
        assert_eq!(quote_ident_if_needed(Dialect::Postgres, "books"), "books");
        assert_eq!(
            quote_ident_if_needed(Dialect::Postgres, "select"),
            "\"select\""
        );
        let sql = "SELECT * FROM public.";
        let caret = sql.len();
        let items = complete(
            Dialect::Postgres,
            &books_catalog(),
            sql,
            caret,
            Some("public"),
        );
        let odd = items
            .iter()
            .find(|i| i.label == "Odd Name")
            .expect("Odd Name");
        assert_eq!(odd.insert, "\"Odd Name\"");
    }

    #[test]
    fn au10_large_catalog_stays_bounded() {
        let mut tables = Vec::new();
        for i in 0..400 {
            tables.push(TableEntry {
                name: format!("table_{i:03}"),
                columns: (0..20)
                    .map(|c| ColumnMeta {
                        name: format!("col_{c}"),
                        type_name: "text".into(),
                    })
                    .collect(),
            });
        }
        let catalog = CatalogStatus::Ready(SchemaSnapshot {
            schemas: vec![SchemaEntry {
                name: "public".into(),
                tables,
            }],
        });
        let start = std::time::Instant::now();
        let items = complete(Dialect::Postgres, &catalog, "table_1", 7, Some("public"));
        assert!(start.elapsed().as_millis() < 200, "ranking too slow");
        assert!(items.len() <= MAX_SUGGESTIONS);
        assert!(!items.is_empty());
    }

    #[test]
    fn au16_loading_and_empty_and_ddl_detect() {
        let loading = complete(Dialect::Postgres, &CatalogStatus::Loading, "bo", 2, None);
        assert_eq!(loading.len(), 1);
        assert_eq!(loading[0].kind, CompletionKind::Status);
        assert_eq!(loading[0].insert, "");
        assert_eq!(loading[0].label, LOADING_LABEL);

        assert!(is_catalog_refresh_statement("CREATE TABLE t (id int)"));
        assert!(is_catalog_refresh_statement("-- c\nDROP TABLE t"));
        assert!(is_catalog_refresh_statement("USE sample"));
        assert!(is_catalog_refresh_statement("SET search_path TO public"));
        assert!(!is_catalog_refresh_statement("GRANT SELECT ON t TO u"));
        assert!(!is_catalog_refresh_statement("SELECT 1"));
    }

    #[test]
    fn au06_keywords_and_tables_present() {
        let items = complete(Dialect::Sqlite, &books_catalog(), "bo", 2, Some("public"));
        assert!(items
            .iter()
            .any(|i| i.label == "books" && i.detail == "table"));
        let items = complete(Dialect::Sqlite, &books_catalog(), "SEL", 3, None);
        assert!(items.iter().any(|i| i.label == "SELECT"));
    }
}
