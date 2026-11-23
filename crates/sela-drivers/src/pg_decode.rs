//! Postgres-specific cell decode strategies (M1-9).
//!
//! Types without a native [`Value`] mapping are rendered as their psql-like text
//! form via sqlx's typed decoders (arrays, inet/cidr, macaddr, money, bit, point,
//! ranges) or by reading text-compatible raw bytes (enums, tsvector, domains).
//!
//! Decode failures never become SQL NULL — they stay [`Value::Other`] / decode_error.

use std::fmt::Write as _;
use std::ops::Bound;
use std::sync::Arc;

use bit_vec::BitVec;
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use rust_decimal::Decimal;
use sqlx::postgres::types::{PgMoney, PgPoint, PgRange};
use sqlx::postgres::{PgColumn, PgRow, PgTypeInfo, PgValueFormat, Postgres};
use sqlx::{Column, Decode, Row, Type, TypeInfo, ValueRef};

use sela_core::Value;

use crate::decode::{early_value, other, probe_null, type_name};

/// Per-column decode plan for one result set. Computed once from column type info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PgDecodeStrategy {
    /// Use the existing typed `read_postgres_cell` match arms / helpers.
    Native,
    ArrayInt2,
    ArrayInt4,
    ArrayInt8,
    ArrayFloat4,
    ArrayFloat8,
    ArrayBool,
    ArrayText,
    ArrayUuid,
    Inet,
    MacAddr,
    Money,
    Bit,
    Point,
    Int4Range,
    Int8Range,
    NumRange,
    DateRange,
    TsVector,
    /// Enums and other text-identical binary payloads.
    TextBytes,
    /// Last resort: try String / bytes / `<TYPE>` via [`other`].
    Fallback,
}

pub(crate) fn strategy_for_type_info(type_info: &PgTypeInfo) -> PgDecodeStrategy {
    match type_info.kind() {
        sqlx::postgres::PgTypeKind::Domain(base) => return strategy_for_type_info(base),
        // Enum binary wire format is the label as text — the only unknown Simple-like
        // case where binary == text is guaranteed.
        sqlx::postgres::PgTypeKind::Enum(_) => return PgDecodeStrategy::TextBytes,
        sqlx::postgres::PgTypeKind::Array(elem) => {
            return array_strategy_for_elem(elem.name());
        }
        sqlx::postgres::PgTypeKind::Range(elem) => {
            return range_strategy_for_elem(elem.name());
        }
        // Composites / records and pseudo-types have structured binary layouts.
        // Never attempt UTF-8 text interpretation.
        sqlx::postgres::PgTypeKind::Composite(_) | sqlx::postgres::PgTypeKind::Pseudo => {
            return PgDecodeStrategy::Fallback;
        }
        _ => {}
    }

    strategy_for_type_name(type_info.name())
}

fn array_strategy_for_elem(elem_name: &str) -> PgDecodeStrategy {
    match elem_name.to_ascii_uppercase().as_str() {
        "INT2" | "SMALLINT" => PgDecodeStrategy::ArrayInt2,
        "INT4" | "INT" | "INTEGER" => PgDecodeStrategy::ArrayInt4,
        "INT8" | "BIGINT" => PgDecodeStrategy::ArrayInt8,
        "FLOAT4" | "REAL" => PgDecodeStrategy::ArrayFloat4,
        "FLOAT8" | "DOUBLE PRECISION" => PgDecodeStrategy::ArrayFloat8,
        "BOOL" | "BOOLEAN" => PgDecodeStrategy::ArrayBool,
        "TEXT" | "VARCHAR" | "NAME" | "BPCHAR" | "CHAR" | "CITEXT" => PgDecodeStrategy::ArrayText,
        "UUID" => PgDecodeStrategy::ArrayUuid,
        _ => PgDecodeStrategy::Fallback,
    }
}

fn range_strategy_for_elem(elem_name: &str) -> PgDecodeStrategy {
    match elem_name.to_ascii_uppercase().as_str() {
        "INT4" | "INT" | "INTEGER" => PgDecodeStrategy::Int4Range,
        "INT8" | "BIGINT" => PgDecodeStrategy::Int8Range,
        "NUMERIC" | "DECIMAL" => PgDecodeStrategy::NumRange,
        "DATE" => PgDecodeStrategy::DateRange,
        _ => PgDecodeStrategy::Fallback,
    }
}

fn strategy_for_type_name(name: &str) -> PgDecodeStrategy {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "INT2[]" | "_INT2" => PgDecodeStrategy::ArrayInt2,
        "INT4[]" | "_INT4" => PgDecodeStrategy::ArrayInt4,
        "INT8[]" | "_INT8" => PgDecodeStrategy::ArrayInt8,
        "FLOAT4[]" | "_FLOAT4" => PgDecodeStrategy::ArrayFloat4,
        "FLOAT8[]" | "_FLOAT8" => PgDecodeStrategy::ArrayFloat8,
        "BOOL[]" | "_BOOL" => PgDecodeStrategy::ArrayBool,
        "TEXT[]" | "_TEXT" | "VARCHAR[]" | "_VARCHAR" | "NAME[]" | "_NAME" | "BPCHAR[]"
        | "_BPCHAR" => PgDecodeStrategy::ArrayText,
        "UUID[]" | "_UUID" => PgDecodeStrategy::ArrayUuid,
        "INET" | "CIDR" => PgDecodeStrategy::Inet,
        "MACADDR" => PgDecodeStrategy::MacAddr,
        "MONEY" => PgDecodeStrategy::Money,
        "BIT" | "VARBIT" => PgDecodeStrategy::Bit,
        "POINT" => PgDecodeStrategy::Point,
        "INT4RANGE" => PgDecodeStrategy::Int4Range,
        "INT8RANGE" => PgDecodeStrategy::Int8Range,
        "NUMRANGE" => PgDecodeStrategy::NumRange,
        "DATERANGE" => PgDecodeStrategy::DateRange,
        "TSVECTOR" => PgDecodeStrategy::TsVector,
        // tsquery binary layout differs from tsvector; fail safe until a dedicated decoder exists.
        "TSQUERY" => PgDecodeStrategy::Fallback,
        // Built-ins already handled as Native by the caller when matched first.
        "BOOL" | "INT2" | "INT4" | "INT8" | "OID" | "FLOAT4" | "FLOAT8" | "NUMERIC" | "TEXT"
        | "VARCHAR" | "NAME" | "XML" | "CITEXT" | "CHAR" | "BPCHAR" | "\"CHAR\"" | "BYTEA"
        | "UUID" | "JSON" | "JSONB" | "DATE" | "TIME" | "TIMESTAMP" | "TIMESTAMPTZ"
        | "INTERVAL" => PgDecodeStrategy::Native,
        // Unknown non-array types (polygon, box, PostGIS, extension types, …) must
        // NOT use TextBytes: sqlx fetches BINARY, and many binary payloads are still
        // valid UTF-8 with control junk. Enums are handled via PgTypeKind::Enum above.
        _ => PgDecodeStrategy::Fallback,
    }
}

pub(crate) fn strategies_from_columns<'a>(
    columns: impl IntoIterator<Item = &'a PgColumn>,
) -> Vec<PgDecodeStrategy> {
    columns
        .into_iter()
        .map(|column| strategy_for_type_info(column.type_info()))
        .collect()
}

pub(crate) fn read_postgres_cell_with_strategy(
    row: &PgRow,
    index: usize,
    strategy: PgDecodeStrategy,
) -> Value {
    let type_name = type_name(row, index);
    if let Some(early) = early_value(probe_null(row, index), &type_name) {
        return early;
    }

    match strategy {
        PgDecodeStrategy::Native => crate::decode::read_postgres_native(row, index, &type_name),
        PgDecodeStrategy::ArrayInt2 => array_other(
            &type_name,
            try_decode::<Vec<Option<i16>>>(row, index)
                .map(|v| format_array(v.iter().map(|x| x.map(|n| n.to_string())))),
            row,
            index,
        ),
        PgDecodeStrategy::ArrayInt4 => array_other(
            &type_name,
            try_decode::<Vec<Option<i32>>>(row, index)
                .map(|v| format_array(v.iter().map(|x| x.map(|n| n.to_string())))),
            row,
            index,
        ),
        PgDecodeStrategy::ArrayInt8 => array_other(
            &type_name,
            try_decode::<Vec<Option<i64>>>(row, index)
                .map(|v| format_array(v.iter().map(|x| x.map(|n| n.to_string())))),
            row,
            index,
        ),
        PgDecodeStrategy::ArrayFloat4 => array_other(
            &type_name,
            try_decode::<Vec<Option<f32>>>(row, index)
                .map(|v| format_array(v.iter().map(|x| x.map(|n| format_pg_float(f64::from(n)))))),
            row,
            index,
        ),
        PgDecodeStrategy::ArrayFloat8 => array_other(
            &type_name,
            try_decode::<Vec<Option<f64>>>(row, index)
                .map(|v| format_array(v.iter().map(|x| x.map(format_pg_float)))),
            row,
            index,
        ),
        PgDecodeStrategy::ArrayBool => array_other(
            &type_name,
            try_decode::<Vec<Option<bool>>>(row, index).map(|v| {
                format_array(
                    v.iter()
                        .map(|x| x.map(|b| if b { "t".to_string() } else { "f".to_string() })),
                )
            }),
            row,
            index,
        ),
        PgDecodeStrategy::ArrayText => array_other(
            &type_name,
            try_decode::<Vec<Option<String>>>(row, index).map(|v| format_text_array(v.as_slice())),
            row,
            index,
        ),
        PgDecodeStrategy::ArrayUuid => array_other(
            &type_name,
            try_decode::<Vec<Option<uuid::Uuid>>>(row, index)
                .map(|v| format_array(v.iter().map(|x| x.map(|u| u.to_string())))),
            row,
            index,
        ),
        PgDecodeStrategy::Inet => match try_decode::<IpNetwork>(row, index) {
            Some(net) => other_text(&type_name, net.to_string()),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::MacAddr => match try_decode::<MacAddress>(row, index) {
            Some(mac) => other_text(&type_name, format_mac(mac)),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::Money => match try_decode::<PgMoney>(row, index) {
            Some(money) => other_text(&type_name, format_money(money)),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::Bit => match try_decode::<BitVec>(row, index) {
            Some(bits) => other_text(&type_name, format_bitvec(&bits)),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::Point => match try_decode::<PgPoint>(row, index) {
            Some(point) => other_text(
                &type_name,
                format!(
                    "({},{})",
                    format_pg_float(point.x),
                    format_pg_float(point.y)
                ),
            ),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::Int4Range => match try_decode::<PgRange<i32>>(row, index) {
            Some(range) => other_text(&type_name, range.to_string()),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::Int8Range => match try_decode::<PgRange<i64>>(row, index) {
            Some(range) => other_text(&type_name, range.to_string()),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::NumRange => match try_decode::<PgRange<Decimal>>(row, index) {
            Some(range) => other_text(&type_name, format_num_range(&range)),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::DateRange => match try_decode::<PgRange<chrono::NaiveDate>>(row, index) {
            Some(range) => other_text(&type_name, range.to_string()),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::TsVector => match decode_tsvector(row, index) {
            Some(text) => other_text(&type_name, text),
            None => text_bytes_or_other(&type_name, row, index),
        },
        PgDecodeStrategy::TextBytes => text_bytes_or_other(&type_name, row, index),
        PgDecodeStrategy::Fallback => other(&type_name, row, index),
    }
}

/// Decode `T` from a cell, including domains whose OID fails `Type::compatible`.
fn try_decode<'r, T>(row: &'r PgRow, index: usize) -> Option<T>
where
    T: Decode<'r, Postgres> + Type<Postgres>,
{
    if let Ok(value) = row.try_get::<T, _>(index) {
        return Some(value);
    }
    let raw = row.try_get_raw(index).ok()?;
    T::decode(raw).ok()
}

fn array_other(type_name: &str, rendered: Option<String>, row: &PgRow, index: usize) -> Value {
    match rendered {
        Some(text) => other_text(type_name, text),
        None => text_bytes_or_other(type_name, row, index),
    }
}

fn other_text(type_name: &str, text: impl Into<Arc<str>>) -> Value {
    Value::Other {
        type_name: type_name.to_string(),
        text: text.into(),
    }
}

fn text_bytes_or_other(type_name: &str, row: &PgRow, index: usize) -> Value {
    if let Some(text) = raw_text_bytes(row, index) {
        return other_text(type_name, text);
    }
    other(type_name, row, index)
}

/// Read Postgres raw value bytes as UTF-8 when the payload is text-identical
/// (enums; text-format cells). Rejects binary junk that happens to be valid UTF-8.
fn raw_text_bytes(row: &PgRow, index: usize) -> Option<String> {
    let raw = row.try_get_raw(index).ok()?;
    if ValueRef::is_null(&raw) {
        return None;
    }
    let bytes = raw.as_bytes().ok()?;
    match raw.format() {
        PgValueFormat::Text => decode_safe_utf8_text(bytes),
        PgValueFormat::Binary => decode_safe_utf8_text(bytes),
    }
}

/// True when every byte is printable UTF-8 text (allows tab/newline/carriage return).
/// Rejects NUL and other control bytes so binary layouts are not shown as garbage.
pub(crate) fn bytes_are_safe_text(bytes: &[u8]) -> bool {
    !bytes
        .iter()
        .any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r'))
}

fn decode_safe_utf8_text(bytes: &[u8]) -> Option<String> {
    if !bytes_are_safe_text(bytes) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn format_array<I, S>(items: I) -> String
where
    I: IntoIterator<Item = Option<S>>,
    S: AsRef<str>,
{
    let mut out = String::from("{");
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match item {
            None => out.push_str("NULL"),
            Some(value) => out.push_str(value.as_ref()),
        }
    }
    out.push('}');
    out
}

fn format_text_array(items: &[Option<String>]) -> String {
    let mut out = String::from("{");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match item {
            None => out.push_str("NULL"),
            Some(value) => push_array_text_element(&mut out, value),
        }
    }
    out.push('}');
    out
}

fn push_array_text_element(out: &mut String, value: &str) {
    let needs_quote = value.is_empty()
        || value.eq_ignore_ascii_case("null")
        || value.bytes().any(|b| {
            matches!(
                b,
                b'"' | b'\\' | b'{' | b'}' | b',' | b' ' | b'\t' | b'\n' | b'\r'
            )
        });
    if !needs_quote {
        out.push_str(value);
        return;
    }
    out.push('"');
    for ch in value.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
}

fn format_pg_float(value: f64) -> String {
    // Match Postgres text output for ordinary floats (no forced trailing .0).
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        let mut s = value.to_string();
        if let Some(stripped) = s.strip_suffix(".0") {
            s = stripped.to_string();
        }
        s
    }
}

fn format_money(money: PgMoney) -> String {
    // Assume locale_frac_digits = 2 (C / en_US). Format like psql: $1,234.56 / -$5.50
    let decimal = money.to_decimal(2);
    let negative = decimal.is_sign_negative();
    let abs = decimal.abs();
    let formatted = format!("{abs:.2}");
    let (int_part, frac) = formatted
        .split_once('.')
        .map_or((formatted.as_str(), "00"), |(a, b)| (a, b));
    let with_commas = group_thousands(int_part);
    if negative {
        format!("-${with_commas}.{frac}")
    } else {
        format!("${with_commas}.{frac}")
    }
}

fn group_thousands(int_part: &str) -> String {
    let mut digits: Vec<char> = int_part.chars().collect();
    if digits.is_empty() {
        return "0".into();
    }
    let mut out = String::new();
    while !digits.is_empty() {
        let take = digits.len().min(3);
        let start = digits.len() - take;
        if !out.is_empty() {
            out.insert(0, ',');
        }
        for ch in digits[start..].iter().rev() {
            out.insert(0, *ch);
        }
        digits.truncate(start);
    }
    out
}

fn format_mac(mac: MacAddress) -> String {
    let bytes = mac.bytes();
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}

fn format_bitvec(bits: &BitVec) -> String {
    let mut out = String::with_capacity(bits.len());
    for bit in bits.iter() {
        out.push(if bit { '1' } else { '0' });
    }
    out
}

fn format_num_range(range: &PgRange<Decimal>) -> String {
    let mut out = String::new();
    match &range.start {
        Bound::Unbounded => out.push_str("(,"),
        Bound::Excluded(v) => {
            let _ = write!(out, "({},", v.normalize());
        }
        Bound::Included(v) => {
            let _ = write!(out, "[{},", v.normalize());
        }
    }
    match &range.end {
        Bound::Unbounded => out.push(')'),
        Bound::Excluded(v) => {
            let _ = write!(out, "{})", v.normalize());
        }
        Bound::Included(v) => {
            let _ = write!(out, "{}]", v.normalize());
        }
    }
    out
}

/// Decode tsvector / tsquery from text format, or from the binary lexeme layout.
fn decode_tsvector(row: &PgRow, index: usize) -> Option<String> {
    if let Some(text) = raw_text_bytes(row, index) {
        // Binary tsvector contains NULs and fails raw_text_bytes; text format succeeds.
        // If we somehow got binary mis-read as UTF-8 without NULs, still accept it.
        return Some(text);
    }
    let raw = row.try_get_raw(index).ok()?;
    let bytes = raw.as_bytes().ok()?;
    if raw.format() != PgValueFormat::Binary {
        return None;
    }
    format_tsvector_binary(bytes)
}

fn format_tsvector_binary(mut bytes: &[u8]) -> Option<String> {
    // tsvector_send: int32 nlexemes; then for each: cstring lexeme, int16 npos, npos×int16.
    if bytes.len() < 4 {
        return None;
    }
    let n = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    bytes = &bytes[4..];
    if n < 0 {
        return None;
    }
    let n = n as usize;
    let mut parts = Vec::with_capacity(n);
    for _ in 0..n {
        let nul = bytes.iter().position(|&b| b == 0)?;
        let lexeme = std::str::from_utf8(&bytes[..nul]).ok()?;
        bytes = &bytes[nul + 1..];
        if bytes.len() < 2 {
            return None;
        }
        let npos = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        bytes = &bytes[2..];
        let need = npos.checked_mul(2)?;
        if bytes.len() < need {
            return None;
        }
        if npos == 0 {
            parts.push(format!("'{lexeme}'"));
        } else {
            let mut positions = Vec::with_capacity(npos);
            for i in 0..npos {
                let off = i * 2;
                let pos = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                // High 2 bits are weight; low 14 are position (psql shows position).
                let index = pos & 0x3FFF;
                positions.push(index.to_string());
            }
            parts.push(format!("'{lexeme}':{}", positions.join(",")));
        }
        bytes = &bytes[need..];
    }
    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::types::PgMoney;

    #[test]
    fn formats_text_array_like_psql() {
        let items = vec![
            Some("a".into()),
            None,
            Some("b,c".into()),
            Some("\"x\"".into()),
        ];
        assert_eq!(format_text_array(&items), r#"{a,NULL,"b,c","\"x\""}"#);
    }

    #[test]
    fn formats_int_array_with_null() {
        assert_eq!(
            format_array(
                [Some("1"), Some("2"), None, Some("3")]
                    .into_iter()
                    .map(|x| x.map(str::to_string))
            ),
            "{1,2,NULL,3}"
        );
    }

    #[test]
    fn formats_money_like_c_locale_psql() {
        assert_eq!(format_money(PgMoney(123456)), "$1,234.56");
        assert_eq!(format_money(PgMoney(1234)), "$12.34");
        assert_eq!(format_money(PgMoney(-550)), "-$5.50");
    }

    #[test]
    fn formats_bitvec() {
        let mut bits = BitVec::from_elem(4, false);
        bits.set(0, true);
        bits.set(2, true);
        assert_eq!(format_bitvec(&bits), "1010");
    }

    #[test]
    fn strategy_names_cover_m1_9_types() {
        assert_eq!(
            strategy_for_type_name("INT4[]"),
            PgDecodeStrategy::ArrayInt4
        );
        assert_eq!(
            strategy_for_type_name("TEXT[]"),
            PgDecodeStrategy::ArrayText
        );
        assert_eq!(strategy_for_type_name("INET"), PgDecodeStrategy::Inet);
        assert_eq!(strategy_for_type_name("CIDR"), PgDecodeStrategy::Inet);
        assert_eq!(strategy_for_type_name("MACADDR"), PgDecodeStrategy::MacAddr);
        assert_eq!(strategy_for_type_name("MONEY"), PgDecodeStrategy::Money);
        assert_eq!(strategy_for_type_name("BIT"), PgDecodeStrategy::Bit);
        assert_eq!(strategy_for_type_name("POINT"), PgDecodeStrategy::Point);
        assert_eq!(
            strategy_for_type_name("INT4RANGE"),
            PgDecodeStrategy::Int4Range
        );
        assert_eq!(
            strategy_for_type_name("TSVECTOR"),
            PgDecodeStrategy::TsVector
        );
        assert_eq!(
            strategy_for_type_name("TSQUERY"),
            PgDecodeStrategy::Fallback
        );
        // Name alone is not enough for TextBytes — Enum kind is required.
        assert_eq!(
            strategy_for_type_name("sela_mood"),
            PgDecodeStrategy::Fallback
        );
        assert_eq!(
            strategy_for_type_name("POLYGON"),
            PgDecodeStrategy::Fallback
        );
        assert_eq!(
            strategy_for_type_name("TSTZRANGE"),
            PgDecodeStrategy::Fallback
        );
    }

    #[test]
    fn safe_text_bytes_rejects_nul_and_other_controls() {
        assert!(bytes_are_safe_text(b"happy"));
        assert!(bytes_are_safe_text(b"a\tb\nc\r"));
        assert!(bytes_are_safe_text(b""));
        assert!(!bytes_are_safe_text(b"a\x00b"));
        assert!(!bytes_are_safe_text(b"a\x01b"));
        assert!(!bytes_are_safe_text(&[0x1f]));
        assert!(decode_safe_utf8_text(b"happy").as_deref() == Some("happy"));
        assert!(decode_safe_utf8_text(b"a\x00b").is_none());
        assert!(decode_safe_utf8_text(b"a\x01b").is_none());
    }
}
