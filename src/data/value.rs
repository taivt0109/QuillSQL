//! Column values read from SQL Server, and how two of them are ordered and compared.
//!
//! Keys are ordered the way both servers are asked to order them (see `reader`): strings by
//! code point after the server has trimmed trailing spaces (and upper-cased them for
//! case-insensitive keys), uniqueidentifier in SQL Server's byte order, binary with trailing
//! zero bytes ignored, datetimeoffset by its UTC instant. The merge relies on that agreement
//! and stops with an error if a stream is ever out of order.

use std::cmp::Ordering;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use tiberius::{ColumnData, Row};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// Exact decimal: digits × 10^-scale.
    Decimal(i128, u8),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    /// In SQL Server's storage order.
    Guid([u8; 16]),
    /// Days since 0001-01-01.
    Date(i64),
    /// Nanoseconds since midnight.
    Time(i64),
    /// Nanoseconds since 0001-01-01.
    DateTime(i128),
    /// UTC nanoseconds since 0001-01-01, and the offset in minutes.
    DateTimeOffset(i128, i32),
}

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1, 1, 1).unwrap()
}

fn time_nanos(t: NaiveTime) -> i64 {
    t.num_seconds_from_midnight() as i64 * 1_000_000_000 + t.nanosecond() as i64
}

fn datetime_nanos(dt: NaiveDateTime) -> i128 {
    let days = dt.date().signed_duration_since(epoch()).num_days() as i128;
    days * 86_400_000_000_000 + time_nanos(dt.time()) as i128
}

impl Value {
    /// Reads column `i` of a row.
    pub fn read(row: &Row, i: usize) -> Value {
        let Ok(data) = row.get_column_data(i) else { return Value::Null };
        match data {
            ColumnData::U8(v) => v.map_or(Value::Null, |v| Value::Int(v as i64)),
            ColumnData::I16(v) => v.map_or(Value::Null, |v| Value::Int(v as i64)),
            ColumnData::I32(v) => v.map_or(Value::Null, |v| Value::Int(v as i64)),
            ColumnData::I64(v) => v.map_or(Value::Null, Value::Int),
            ColumnData::F32(v) => v.map_or(Value::Null, |v| Value::Float(v as f64)),
            ColumnData::F64(v) => v.map_or(Value::Null, Value::Float),
            ColumnData::Bit(v) => v.map_or(Value::Null, Value::Bool),
            ColumnData::String(v) => v.as_ref().map_or(Value::Null, |s| Value::Str(s.to_string())),
            ColumnData::Guid(v) => v.map_or(Value::Null, |g| Value::Guid(g.to_bytes_le())),
            ColumnData::Binary(v) => v.as_ref().map_or(Value::Null, |b| Value::Bytes(b.to_vec())),
            ColumnData::Numeric(v) => v.map_or(Value::Null, |n| Value::Decimal(n.value(), n.scale())),
            ColumnData::Xml(v) => v.as_ref().map_or(Value::Null, |x| Value::Str(x.to_string())),
            ColumnData::Date(_) => row.try_get::<NaiveDate, _>(i).ok().flatten().map_or(Value::Null, |d| {
                Value::Date(d.signed_duration_since(epoch()).num_days())
            }),
            ColumnData::Time(_) => row.try_get::<NaiveTime, _>(i).ok().flatten().map_or(Value::Null, |t| Value::Time(time_nanos(t))),
            ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
                row.try_get::<NaiveDateTime, _>(i).ok().flatten().map_or(Value::Null, |dt| Value::DateTime(datetime_nanos(dt)))
            }
            ColumnData::DateTimeOffset(_) => row.try_get::<DateTime<FixedOffset>, _>(i).ok().flatten().map_or(Value::Null, |dt| {
                Value::DateTimeOffset(datetime_nanos(dt.naive_utc()), dt.offset().local_minus_utc() / 60)
            }),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The value as a T-SQL literal for the deployment script; `type_name` is the target
    /// column's type (it decides N'' and the date literal form).
    pub fn literal(&self, type_name: &str) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Bool(b) => if *b { "1".into() } else { "0".into() },
            Value::Int(v) => v.to_string(),
            Value::Decimal(v, s) => decimal_text(*v, *s),
            Value::Float(f) => {
                let s = format!("{f:e}");
                if f.is_finite() { s } else { "NULL".into() }
            }
            Value::Str(s) => {
                let quoted = s.replace('\'', "''");
                if matches!(type_name, "char" | "varchar" | "text") { format!("'{quoted}'") } else { format!("N'{quoted}'") }
            }
            Value::Bytes(b) => {
                if b.is_empty() {
                    "0x".into()
                } else {
                    let mut s = String::with_capacity(2 + b.len() * 2);
                    s.push_str("0x");
                    for byte in b {
                        s.push_str(&format!("{byte:02X}"));
                    }
                    s
                }
            }
            Value::Guid(g) => format!("'{}'", tiberius::Uuid::from_bytes_le(*g).to_string().to_uppercase()),
            Value::Date(d) => format!("'{}'", ymd(*d)),
            Value::Time(n) => format!("'{}'", format_time(*n)),
            Value::DateTime(n) => {
                let days = n.div_euclid(86_400_000_000_000) as i64;
                let rest = n.rem_euclid(86_400_000_000_000) as i64;
                format!("'{}T{}'", ymd(days), format_time(rest))
            }
            Value::DateTimeOffset(n, offset) => {
                let local = n + *offset as i128 * 60_000_000_000;
                let days = local.div_euclid(86_400_000_000_000) as i64;
                let rest = local.rem_euclid(86_400_000_000_000) as i64;
                let sign = if *offset < 0 { '-' } else { '+' };
                format!(
                    "'{}T{}{sign}{:02}:{:02}'",
                    ymd(days),
                    format_time(rest),
                    offset.abs() / 60,
                    offset.abs() % 60
                )
            }
        }
    }

    /// Text for reports and the console.
    pub fn display(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Str(s) => s.clone(),
            other => {
                let l = other.literal("nvarchar");
                l.trim_start_matches(['N']).trim_matches('\'').to_string()
            }
        }
    }
}

fn ymd(days: i64) -> String {
    let d = epoch() + chrono::Duration::days(days);
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

fn format_time(nanos: i64) -> String {
    let (h, m, s, frac) = (nanos / 3_600_000_000_000, nanos / 60_000_000_000 % 60, nanos / 1_000_000_000 % 60, nanos % 1_000_000_000);
    if frac == 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        let f = format!("{frac:09}");
        format!("{h:02}:{m:02}:{s:02}.{}", f.trim_end_matches('0'))
    }
}

pub fn decimal_text(v: i128, scale: u8) -> String {
    if scale == 0 {
        return v.to_string();
    }
    let neg = v < 0;
    let digits = v.unsigned_abs().to_string();
    let scale = scale as usize;
    let padded = if digits.len() <= scale { format!("{}{digits}", "0".repeat(scale - digits.len() + 1)) } else { digits };
    let (int, frac) = padded.split_at(padded.len() - scale);
    format!("{}{int}.{frac}", if neg { "-" } else { "" })
}

/// Compares numbers of any kind exactly (decimals by value, so 1.50 = 1.5).
fn cmp_number(a: &Value, b: &Value) -> Option<Ordering> {
    fn as_decimal(v: &Value) -> Option<(i128, u8)> {
        match v {
            Value::Int(i) => Some((*i as i128, 0)),
            Value::Bool(b) => Some((*b as i128, 0)),
            Value::Decimal(d, s) => Some((*d, *s)),
            _ => None,
        }
    }
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Float(x), other) | (other, Value::Float(x)) => {
            let (d, s) = as_decimal(other)?;
            let y = d as f64 / 10f64.powi(s as i32);
            let ord = x.partial_cmp(&y)?;
            Some(if matches!(a, Value::Float(_)) { ord } else { ord.reverse() })
        }
        _ => {
            let (x, xs) = as_decimal(a)?;
            let (y, ys) = as_decimal(b)?;
            let scale = xs.max(ys);
            let up = |v: i128, s: u8| v.checked_mul(10i128.checked_pow((scale - s) as u32)?);
            Some(up(x, xs)?.cmp(&up(y, ys)?))
        }
    }
}

/// SQL Server orders uniqueidentifier by bytes 10–15, 8–9, 6–7, 4–5, then 0–3 (storage order).
fn cmp_guid(a: &[u8; 16], b: &[u8; 16]) -> Ordering {
    const ORDER: [usize; 16] = [10, 11, 12, 13, 14, 15, 8, 9, 6, 7, 4, 5, 0, 1, 2, 3];
    ORDER.iter().map(|&i| a[i].cmp(&b[i])).find(|o| o.is_ne()).unwrap_or(Ordering::Equal)
}

fn trim_zeros(b: &[u8]) -> &[u8] {
    let end = b.iter().rposition(|&x| x != 0).map_or(0, |p| p + 1);
    &b[..end]
}

/// Orders two key values; NULL sorts first, as in SQL Server.
pub fn cmp_key(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bytes(x), Value::Bytes(y)) => trim_zeros(x).cmp(trim_zeros(y)),
        (Value::Guid(x), Value::Guid(y)) => cmp_guid(x, y),
        (Value::Date(x), Value::Date(y)) => x.cmp(y),
        (Value::Time(x), Value::Time(y)) => x.cmp(y),
        (Value::DateTime(x), Value::DateTime(y)) => x.cmp(y),
        (Value::DateTimeOffset(x, _), Value::DateTimeOffset(y, _)) => x.cmp(y),
        _ => cmp_number(a, b).unwrap_or_else(|| format!("{a:?}").cmp(&format!("{b:?}"))),
    }
}

/// How non-key values are compared.
#[derive(Clone, Copy, Debug, Default)]
pub struct Equality {
    /// Compare strings exactly; otherwise case is ignored for case-insensitive columns.
    pub binary: bool,
    pub case_insensitive_column: bool,
    pub trim_trailing_spaces: bool,
    pub empty_is_null: bool,
    /// Compare floats exactly; otherwise to 15 significant digits.
    pub full_precision: bool,
}

/// Whether two non-key values are the same.
pub fn same(a: &Value, b: &Value, eq: Equality) -> bool {
    let norm = |v: &Value| -> Value {
        match v {
            Value::Str(s) => {
                let s = if eq.trim_trailing_spaces { s.trim_end_matches(' ') } else { s.as_str() };
                if eq.empty_is_null && s.is_empty() { Value::Null } else { Value::Str(s.to_string()) }
            }
            other => other.clone(),
        }
    };
    let (a, b) = (norm(a), norm(b));
    match (&a, &b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Str(x), Value::Str(y)) => {
            if eq.binary || !eq.case_insensitive_column {
                x == y
            } else {
                x.to_lowercase() == y.to_lowercase()
            }
        }
        (Value::Float(x), Value::Float(y)) if !eq.full_precision => {
            x == y || (x - y).abs() <= 1e-15 * x.abs().max(y.abs())
        }
        (Value::Bytes(x), Value::Bytes(y)) => x == y,
        (Value::DateTimeOffset(x, ox), Value::DateTimeOffset(y, oy)) => x == y && ox == oy,
        _ => cmp_key(&a, &b) == Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(cmp_key(&Value::Decimal(150, 2), &Value::Decimal(15, 1)), Ordering::Equal);
        assert_eq!(cmp_key(&Value::Int(2), &Value::Decimal(15, 1)), Ordering::Greater);
        assert!(!same(&Value::Decimal(150, 2), &Value::Int(1), Equality::default()));
        assert!(same(&Value::Float(0.1 + 0.2), &Value::Float(0.3), Equality::default()));
        assert!(!same(&Value::Float(0.1 + 0.2), &Value::Float(0.3), Equality { full_precision: true, ..Default::default() }));
    }

    #[test]
    fn strings_and_bytes() {
        let eq = Equality { case_insensitive_column: true, ..Default::default() };
        assert!(same(&Value::Str("Abc".into()), &Value::Str("aBC".into()), eq));
        assert!(!same(&Value::Str("Abc".into()), &Value::Str("aBC".into()), Equality { binary: true, ..eq }));
        assert!(same(&Value::Str("a  ".into()), &Value::Str("a".into()), Equality { trim_trailing_spaces: true, ..eq }));
        assert!(same(&Value::Str("".into()), &Value::Null, Equality { empty_is_null: true, ..eq }));
        assert_eq!(cmp_key(&Value::Bytes(vec![1, 0]), &Value::Bytes(vec![1])), Ordering::Equal);
        assert_eq!(cmp_key(&Value::Bytes(vec![1, 0, 0]), &Value::Bytes(vec![2])), Ordering::Less);
        assert_eq!(cmp_key(&Value::Null, &Value::Int(0)), Ordering::Less);
    }

    #[test]
    fn guid_order() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        a[0] = 9; // low-order group
        b[15] = 1; // high-order group
        assert_eq!(cmp_guid(&a, &b), Ordering::Less);
    }

    #[test]
    fn literals() {
        assert_eq!(decimal_text(-1234, 2), "-12.34");
        assert_eq!(decimal_text(5, 3), "0.005");
        assert_eq!(Value::Str("O'Neil".into()).literal("nvarchar"), "N'O''Neil'");
        assert_eq!(Value::Str("x".into()).literal("varchar"), "'x'");
        assert_eq!(Value::Bytes(vec![0xAB, 1]).literal("varbinary"), "0xAB01");
        assert_eq!(Value::Date(0).literal("date"), "'0001-01-01'");
        assert_eq!(Value::DateTime(86_400_000_000_000 + 1_500_000_000).literal("datetime2"), "'0001-01-02T00:00:01.5'");
    }
}
