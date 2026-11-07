//! Column values read from SQL Server, and how two of them are ordered, compared and written.
//!
//! Keys are ordered the way both servers are asked to order them (see `compare`): strings by
//! code point after the server has trimmed trailing spaces (and folded case, accents, kana and
//! width as the collation ignores them), uniqueidentifier in SQL Server's byte order, binary
//! with trailing zero bytes ignored and then by length (the server is asked for `DATALENGTH` as a
//! tie-break, so 0x0102 and 0x010200 are different keys, as SQL Data Compare treats them),
//! datetimeoffset by its UTC instant. The merge relies on that
//! agreement and stops with an error if a stream is ever out of order.

use std::cmp::Ordering;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use tiberius::{ColumnData, Row};

use crate::catalog::DataType;

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
    /// sql_variant: its base type and value.
    Variant(Box<Variant>),
    /// A CLR value (hierarchyid, spatial, user CLR type): its bytes and its text.
    Clr(Vec<u8>, Option<String>),
    /// A value too large to transfer while comparing: its length in bytes and SHA-256.
    Large(i64, Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub base_type: String,
    pub precision: u8,
    pub scale: u8,
    pub max_length: i32,
    pub collation: Option<String>,
    /// The value as text: dates in ODBC canonical form, floats in scientific form, binary as `0x..`.
    pub text: String,
}

const DAY: i128 = 86_400_000_000_000;

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1, 1, 1).unwrap()
}

fn time_nanos(t: NaiveTime) -> i64 {
    t.num_seconds_from_midnight() as i64 * 1_000_000_000 + t.nanosecond() as i64
}

fn datetime_nanos(dt: NaiveDateTime) -> i128 {
    let days = dt.date().signed_duration_since(epoch()).num_days() as i128;
    days * DAY + time_nanos(dt.time()) as i128
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

    /// Text for the results grid.
    pub fn display(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Str(s) => s.clone(),
            Value::Bytes(b) => hex(b, true),
            Value::Clr(b, text) => text.clone().unwrap_or_else(|| hex(b, true)),
            Value::Variant(v) => v.text.clone(),
            Value::Large(n, _) => format!("<{n} bytes>"),
            Value::Float(f) => dotnet_g(*f, 15),
            Value::Bool(b) => if *b { "1".into() } else { "0".into() },
            Value::Guid(g) => guid_text(g),
            Value::DateTime(_) | Value::Date(_) | Value::Time(_) | Value::DateTimeOffset(..) => self.ansi_text(None),
            Value::Int(v) => v.to_string(),
            Value::Decimal(v, s) => decimal_text(*v, *s),
        }
    }

    /// Date and time values as SQL Server's ANSI text, to `scale` fractional digits (all
    /// non-zero ones when `None`).
    fn ansi_text(&self, scale: Option<u8>) -> String {
        match self {
            Value::Date(d) => ymd(*d),
            Value::Time(n) => format_time(*n, scale),
            Value::DateTime(n) => {
                format!("{} {}", ymd(n.div_euclid(DAY) as i64), format_time(n.rem_euclid(DAY) as i64, scale))
            }
            Value::DateTimeOffset(n, offset) => {
                let local = n + *offset as i128 * 60_000_000_000;
                let sign = if *offset < 0 { '-' } else { '+' };
                format!(
                    "{} {} {sign}{:02}:{:02}",
                    ymd(local.div_euclid(DAY) as i64),
                    format_time(local.rem_euclid(DAY) as i64, scale),
                    offset.abs() / 60,
                    offset.abs() % 60
                )
            }
            other => other.display(),
        }
    }

    /// The text SQL Data Compare's CSV export writes for a value of a column of type `ty`.
    pub fn csv_text(&self, ty: &DataType) -> String {
        match self {
            Value::Null => "<NULL>".into(),
            Value::Bool(b) => if *b { "True".into() } else { "False".into() },
            Value::Bytes(_) | Value::Large(..) | Value::Clr(_, None) => "<Binary Data...>".into(),
            Value::Float(f) if ty.name == "real" => dotnet_g_f32(*f as f32, 7),
            Value::DateTime(n) if matches!(ty.name.as_str(), "datetime" | "smalldatetime") => us_datetime(*n),
            Value::Date(_) | Value::Time(_) | Value::DateTime(_) | Value::DateTimeOffset(..) => self.ansi_text(Some(ty.scale.unwrap_or(7))),
            other => other.display(),
        }
    }

    /// The value as a T-SQL literal for a deployment script.
    pub fn sql(&self, ctx: &Literal) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Bool(b) => if *b { "1".into() } else { "0".into() },
            Value::Int(v) => v.to_string(),
            Value::Decimal(v, s) => decimal_text(*v, *s),
            Value::Float(f) => {
                if ctx.source.name == "real" {
                    round_trip_f32(*f as f32)
                } else {
                    round_trip(*f)
                }
            }
            Value::Str(s) => string_literal(s, ctx),
            Value::Bytes(b) => hex(b, false),
            Value::Clr(b, text) => {
                let spatial = matches!(ctx.source.name.as_str(), "geometry" | "geography");
                match text {
                    Some(t) if !spatial && (ctx.clr_as_text || !is_clr(ctx.target)) => sql_string(t, true),
                    _ => hex(b, false),
                }
            }
            Value::Guid(g) => format!("'{}'", guid_text(g)),
            Value::DateTime(n) if matches!(ctx.source.name.as_str(), "datetime" | "smalldatetime") => {
                // To the nearest millisecond, as SqlDateTime converts.
                let ms = (n + 500_000).div_euclid(1_000_000) * 1_000_000;
                let (d, t) = (ms.div_euclid(DAY) as i64, ms.rem_euclid(DAY) as i64);
                let (h, m, s, frac) = (t / 3_600_000_000_000, t / 60_000_000_000 % 60, t / 1_000_000_000 % 60, t % 1_000_000_000 / 1_000_000);
                format!("'{} {h:02}:{m:02}:{s:02}.{frac:03}'", ymd(d))
            }
            Value::Date(_) | Value::Time(_) | Value::DateTime(_) | Value::DateTimeOffset(..) => {
                let scale = ctx.source.scale.or(match ctx.source.name.as_str() {
                    "date" => None,
                    _ => Some(7),
                });
                format!("'{}'", self.ansi_text(scale.or(Some(7))))
            }
            Value::Variant(v) => variant_literal(v),
            Value::Large(..) => "NULL".into(),
        }
    }
}

fn is_clr(t: &DataType) -> bool {
    t.schema.is_some() || matches!(t.name.as_str(), "hierarchyid" | "geometry" | "geography")
}

/// What a literal depends on besides the value.
pub struct Literal<'a> {
    pub source: &'a DataType,
    pub target: &'a DataType,
    pub source_collation: Option<&'a str>,
    pub target_collation: Option<&'a str>,
    /// Write CLR values as text (unless TransportCLRBinary).
    pub clr_as_text: bool,
    /// The target is a graph id column of this table: the id is rewritten for it.
    pub graph_table: Option<&'a str>,
}

impl Literal<'_> {
    fn unicode(t: &DataType) -> bool {
        matches!(t.name.as_str(), "nchar" | "nvarchar" | "ntext" | "xml" | "sysname" | "sql_variant") || is_clr(t)
    }

    /// The source column's text needs N'': it is Unicode, or its collation is not a Latin1 or
    /// code page 1252 one.
    pub fn source_needs_n(&self) -> bool {
        Self::unicode(self.source) || self.source_collation.is_some_and(|c| !c.is_empty() && !c.starts_with("Latin1") && !c.contains("_CP1_"))
    }
}

fn string_literal(s: &str, ctx: &Literal) -> String {
    if let Some(table) = ctx.graph_table {
        return format!("JSON_MODIFY('{}','$.table','{}')", s.replace('\'', "''"), table.replace('\'', "''"));
    }
    if ctx.target.name == "xml" {
        let (body, dtd) = xml_start(s);
        return format!("CONVERT(xml,{},{})", sql_string(body, true), if dtd { 3 } else { 1 });
    }
    let n = ctx.source_needs_n();
    let target_unicode = Literal::unicode(ctx.target);
    let len = s.encode_utf16().count();
    let coll = ctx.target_collation.unwrap_or("DATABASE_DEFAULT");
    if !target_unicode && n && len > 4000 && len <= 8000 {
        let units: Vec<u16> = s.encode_utf16().collect();
        let (a, b) = (String::from_utf16_lossy(&units[..4000]), String::from_utf16_lossy(&units[4000..]));
        return format!(
            "cast({} COLLATE {coll} as varchar(8000))\r\n+cast({} COLLATE {coll} as varchar(8000))\r\n",
            sql_string(&a, true),
            sql_string(&b, true)
        );
    }
    if !target_unicode && n && len > 8000 {
        return format!("{} COLLATE {coll}", sql_string(s, true));
    }
    if target_unicode && len > 4000 {
        return sql_string(s, true);
    }
    sql_string(s, n)
}

/// A quoted string; quotes are doubled and a backslash before a line break is escaped so the
/// server does not read it as a line continuation.
pub fn sql_string(s: &str, unicode: bool) -> String {
    let mut out = String::with_capacity(s.len() + 3);
    if unicode {
        out.push('N');
    }
    out.push('\'');
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            out.push_str("''");
        } else if c == '\\' && i + 1 < chars.len() {
            if chars[i + 1] == '\r' && chars.get(i + 2) == Some(&'\n') {
                out.push_str("\\\\\r\n\r\n");
                i += 2;
            } else if chars[i + 1] == '\n' {
                out.push_str("\\\\\n\n");
                i += 1;
            } else if chars[i + 1] == '\r' {
                out.push_str("\\\\\r");
                i += 1;
            } else {
                out.push('\\');
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out.push('\'');
    out
}

/// The XML without a leading declaration (an `encoding=` would fail inside N''), and whether it
/// has a DTD.
fn xml_start(s: &str) -> (&str, bool) {
    let mut body = s.trim_start_matches('\u{feff}');
    if body.trim_start().starts_with("<?xml")
        && let Some(end) = body.find("?>")
    {
        body = &body[end + 2..];
    }
    (body, body.contains("<!DOCTYPE"))
}

fn variant_literal(v: &Variant) -> String {
    let base = v.base_type.to_ascii_lowercase();
    let coll = v.collation.as_deref().map(|c| format!(" COLLATE {c}")).unwrap_or_default();
    match base.as_str() {
        "char" | "varchar" | "nchar" | "nvarchar" if v.max_length == 0 || v.text.is_empty() => {
            let n = if base.starts_with('n') { "N" } else { "" };
            format!("replicate({n}'',0){coll}")
        }
        "nchar" | "nvarchar" => format!("CAST({} AS {base}({})){coll}", sql_string(&v.text, true), v.max_length / 2),
        "char" | "varchar" => format!("CAST({} AS {base}({})){coll}", sql_string(&v.text, true), v.max_length),
        "binary" | "varbinary" => format!("CAST({} AS {base}({}))", v.text.to_ascii_lowercase(), v.max_length),
        "decimal" | "numeric" => format!("CAST({} AS decimal({},{}))", v.text, v.precision, v.scale),
        "float" | "real" => {
            let f: f64 = v.text.parse().unwrap_or_default();
            format!("CAST({} AS {base})", if base == "real" { round_trip_f32(f as f32) } else { round_trip(f) })
        }
        "uniqueidentifier" => format!("CAST('{}' AS {base})", v.text.to_ascii_lowercase()),
        "tinyint" | "smallint" | "int" | "bigint" | "bit" | "money" | "smallmoney" => format!("CAST({} AS {base})", v.text),
        _ => format!("CAST({} AS {base})", sql_string(&v.text, false)),
    }
}

pub fn hex(b: &[u8], upper: bool) -> String {
    let mut s = String::with_capacity(2 + b.len() * 2);
    s.push_str("0x");
    for byte in b {
        if upper {
            s.push_str(&format!("{byte:02X}"));
        } else {
            s.push_str(&format!("{byte:02x}"));
        }
    }
    s
}

fn guid_text(g: &[u8; 16]) -> String {
    tiberius::Uuid::from_bytes_le(*g).to_string()
}

fn ymd(days: i64) -> String {
    let d = epoch() + chrono::Duration::days(days);
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

fn format_time(nanos: i64, scale: Option<u8>) -> String {
    let (h, m, s, frac) = (nanos / 3_600_000_000_000, nanos / 60_000_000_000 % 60, nanos / 1_000_000_000 % 60, nanos % 1_000_000_000);
    let digits = format!("{frac:09}");
    match scale {
        Some(0) => format!("{h:02}:{m:02}:{s:02}"),
        Some(n) => format!("{h:02}:{m:02}:{s:02}.{}", &format!("{digits}00")[..n.min(9) as usize]),
        None if frac == 0 => format!("{h:02}:{m:02}:{s:02}"),
        None => format!("{h:02}:{m:02}:{s:02}.{}", digits.trim_end_matches('0')),
    }
}

/// datetime as en-US culture text: `M/d/yyyy h:mm:ss tt`.
fn us_datetime(n: i128) -> String {
    let ms = (n + 500_000).div_euclid(1_000_000) * 1_000_000;
    let d = epoch() + chrono::Duration::days(ms.div_euclid(DAY) as i64);
    let t = ms.rem_euclid(DAY) as i64 / 1_000_000_000;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    format!("{}/{}/{} {h12}:{m:02}:{s:02} {}", d.month(), d.day(), d.year(), if h < 12 { "AM" } else { "PM" })
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

/// .NET's general ("G<n>") number format: fixed notation unless the exponent is below -5 or
/// at least the precision, then `d.dddE+XX`.
fn g_format(sci: &str, precision: usize) -> String {
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if neg { "-" } else { "" };
    if exp > -5 && exp < precision as i32 {
        let point = exp + 1;
        if point <= 0 {
            format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
        } else if point as usize >= digits.len() {
            format!("{sign}{digits}{}", "0".repeat(point as usize - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..point as usize], &digits[point as usize..])
        }
    } else {
        let rest = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        format!("{sign}{}{rest}E{}{:02}", &digits[..1], if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

pub fn dotnet_g(f: f64, precision: usize) -> String {
    if f == 0.0 {
        return "0".into();
    }
    g_format(&format!("{:.*e}", precision - 1, f), precision)
}

fn dotnet_g_f32(f: f32, precision: usize) -> String {
    if f == 0.0 {
        return "0".into();
    }
    g_format(&format!("{:.*e}", precision - 1, f), precision)
}

/// .NET Framework's round-trip ("R") text of a double: 15 digits when they read back the same,
/// else 17.
pub fn round_trip(f: f64) -> String {
    let short = dotnet_g(f, 15);
    if short.parse::<f64>().is_ok_and(|x| x == f) { short } else { dotnet_g(f, 17) }
}

fn round_trip_f32(f: f32) -> String {
    let short = dotnet_g_f32(f, 7);
    if short.parse::<f32>().is_ok_and(|x| x == f) { short } else { dotnet_g_f32(f, 9) }
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
        (Value::Bytes(x), Value::Bytes(y)) => trim_zeros(x).cmp(trim_zeros(y)).then_with(|| x.len().cmp(&y.len())),
        (Value::Clr(x, _), Value::Clr(y, _)) => trim_zeros(x).cmp(trim_zeros(y)),
        (Value::Guid(x), Value::Guid(y)) => cmp_guid(x, y),
        (Value::Date(x), Value::Date(y)) => x.cmp(y),
        (Value::Time(x), Value::Time(y)) => x.cmp(y),
        (Value::DateTime(x), Value::DateTime(y)) => x.cmp(y),
        (Value::DateTimeOffset(x, _), Value::DateTimeOffset(y, _)) => x.cmp(y),
        (Value::Variant(x), Value::Variant(y)) => x.base_type.cmp(&y.base_type).then_with(|| x.text.cmp(&y.text)),
        (Value::Large(n, h), Value::Large(m, g)) => n.cmp(m).then_with(|| h.cmp(g)),
        _ => cmp_number(a, b).unwrap_or_else(|| format!("{a:?}").cmp(&format!("{b:?}"))),
    }
}

/// What a collation ignores when it compares strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fold {
    pub case: bool,
    pub accent: bool,
    pub kana: bool,
    pub width: bool,
}

impl Fold {
    /// From a collation name: `_CI`, `_AI`, no `_KS`, no `_WS`; binary collations ignore nothing.
    pub fn of(collation: Option<&str>) -> Fold {
        let Some(c) = collation.filter(|c| !c.is_empty()) else { return Fold::default() };
        let c = c.to_ascii_uppercase();
        if c.contains("_BIN") {
            return Fold::default();
        }
        Fold { case: c.contains("_CI"), accent: c.contains("_AI"), kana: !c.contains("_KS"), width: !c.contains("_WS") }
    }

    pub fn any(&self) -> bool {
        self.case || self.accent || self.kana || self.width
    }

    /// The characters folded besides case, as two strings of equal length (from → to), for
    /// TRANSLATE on the server.
    pub fn table(&self) -> (String, String) {
        let (mut from, mut to) = (String::new(), String::new());
        if self.accent {
            for (a, b) in ACCENT_FROM.chars().zip(ACCENT_TO.chars()) {
                from.push(a);
                to.push(b);
            }
        }
        if self.width {
            for c in 0xff01..=0xff5eu32 {
                from.push(char::from_u32(c).unwrap());
                to.push(char::from_u32(c - 0xfee0).unwrap());
            }
        }
        if self.kana {
            for c in (0x3041..=0x3096u32).chain([0x309d, 0x309e]) {
                from.push(char::from_u32(c).unwrap());
                to.push(char::from_u32(c + 0x60).unwrap());
            }
        }
        (from, to)
    }

    fn char(&self, c: char) -> char {
        let u = c as u32;
        if self.width && (0xff01..=0xff5e).contains(&u) {
            return char::from_u32(u - 0xfee0).unwrap();
        }
        if self.kana && ((0x3041..=0x3096).contains(&u) || u == 0x309d || u == 0x309e) {
            return char::from_u32(u + 0x60).unwrap();
        }
        if self.accent
            && let Some(i) = ACCENT_FROM.chars().position(|x| x == c)
        {
            return ACCENT_TO.chars().nth(i).unwrap();
        }
        c
    }

    /// The string as the collation sees it.
    pub fn apply(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if self.accent && ('\u{300}'..='\u{36f}').contains(&c) {
                continue;
            }
            let c = self.char(c);
            if self.case {
                out.extend(c.to_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }
}

const ACCENT_FROM: &str = "ÀÁÂÃÄÅÇÈÉÊËÌÍÎÏÑÒÓÔÕÖÙÚÛÜÝàáâãäåçèéêëìíîïñòóôõöùúûüýÿĀāĂăĄąĆćĈĉĊċČčĎďĒēĔĕĖėĘęĚěĜĝĞğĠġĢģĤĥĨĩĪīĬĭĮįİĴĵĶķĹĺĻļĽľŃńŅņŇňŌōŎŏŐőŔŕŖŗŘřŚśŜŝŞşŠšŢţŤťŨũŪūŬŭŮůŰűŲųŴŵŶŷŸŹźŻżŽž";
const ACCENT_TO: &str = "AAAAAACEEEEIIIINOOOOOUUUUYaaaaaaceeeeiiiinooooouuuuyyAaAaAaCcCcCcCcDdEeEeEeEeEeGgGgGgGgHhIiIiIiIiIJjKkLlLlLlNnNnNnOoOoOoRrRrRrSsSsSsSsTtTtUuUuUuUuUuUuWwYyYZzZzZz";

/// How non-key values are compared.
#[derive(Clone, Copy, Debug, Default)]
pub struct Equality {
    /// Compare strings exactly (ForceBinaryCollation).
    pub binary: bool,
    /// What database 1's collation ignores.
    pub fold: Fold,
    pub trim_trailing_spaces: bool,
    /// Empty strings are NULL (only when database 2's column is a character type).
    pub empty_is_null: bool,
    /// Compare floats exactly; otherwise nearly-equal ones are the same.
    pub full_precision: bool,
    /// real: nearly equal within 1e-7 rather than 1e-15.
    pub real: bool,
    /// Which side is an xml column.
    pub xml: (bool, bool),
    /// The Windows locale of database 1's collation (0 = none), for text that is not ASCII.
    pub locale: u32,
}

impl Equality {
    fn strings(&self, a: &str, b: &str) -> bool {
        if self.binary || !self.fold.any() {
            return a == b;
        }
        if !(a.is_ascii() && b.is_ascii())
            && let Some(equal) = super::collation::equal(self.locale, self.fold, a, b)
        {
            return equal;
        }
        self.fold.apply(a) == self.fold.apply(b)
    }
}

fn nearly_equal(a: f64, b: f64, eps: f64) -> bool {
    if a == b {
        return true;
    }
    if a * b == 0.0 { (a - b).abs() < eps * eps } else { (a - b).abs() / (a.abs() + b.abs()) < eps }
}

/// A date or time value as nanoseconds of local time since 0001-01-01 (time of day on
/// 1900-01-01, as SQL Server converts it).
fn instant(v: &Value) -> Option<i128> {
    match v {
        Value::Date(d) => Some(*d as i128 * DAY),
        Value::DateTime(n) => Some(*n),
        Value::Time(t) => Some(693_595 * DAY + *t as i128),
        Value::DateTimeOffset(n, o) => Some(n + *o as i128 * 60_000_000_000),
        _ => None,
    }
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
        (Value::Large(n, h), Value::Large(m, g)) => n == m && h == g,
        (Value::Large(..), _) | (_, Value::Large(..)) => false,
        (Value::Str(x), Value::Str(y)) => match eq.xml {
            (true, false) => xml_equal(y, x),
            (false, true) => xml_equal(x, y),
            _ => eq.strings(x, y),
        },
        (Value::Float(x), Value::Float(y)) if !eq.full_precision => nearly_equal(*x, *y, if eq.real { 1e-7 } else { 1e-15 }),
        (Value::Bytes(x), Value::Bytes(y)) | (Value::Clr(x, _), Value::Clr(y, _)) => x == y,
        (Value::Guid(g), Value::Str(s)) | (Value::Str(s), Value::Guid(g)) => eq.strings(&guid_text(g), s),
        (Value::Clr(_, Some(t)), Value::Str(s)) | (Value::Str(s), Value::Clr(_, Some(t))) => eq.strings(t, s),
        (Value::Int(_) | Value::Decimal(..) | Value::Float(_), Value::Str(s)) => eq.strings(&a.display(), s),
        (Value::Str(s), Value::Int(_) | Value::Decimal(..) | Value::Float(_)) => eq.strings(s, &b.display()),
        (Value::Variant(x), Value::Variant(y)) => x.base_type.eq_ignore_ascii_case(&y.base_type) && x.text == y.text,
        (Value::DateTimeOffset(x, ox), Value::DateTimeOffset(y, oy)) => x == y && ox == oy,
        _ => match (instant(&a), instant(&b)) {
            (Some(x), Some(y)) => x == y,
            _ => cmp_key(&a, &b) == Ordering::Equal,
        },
    }
}

/// An xml value compared with a string column: both read as XML fragments and compared node by
/// node (element names, attributes, text with CRLF as LF, comments, processing instructions).
pub fn xml_equal(text: &str, xml: &str) -> bool {
    fn parse(s: &str) -> Option<String> {
        let (body, _) = xml_start(s);
        // A fragment may have several roots and text around them.
        let wrapped = format!("<r>{body}</r>");
        let doc = roxmltree::Document::parse_with_options(&wrapped, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).ok()?;
        let mut out = String::new();
        walk(doc.root_element(), &mut out);
        Some(out)
    }
    fn walk(n: roxmltree::Node, out: &mut String) {
        for c in n.children() {
            match c.node_type() {
                roxmltree::NodeType::Element => {
                    out.push('<');
                    out.push_str(c.tag_name().name());
                    let mut attrs: Vec<(String, String)> = c.attributes().map(|a| (a.name().to_string(), a.value().to_string())).collect();
                    attrs.sort();
                    for (k, v) in attrs {
                        out.push_str(&format!(" {k}={v:?}"));
                    }
                    out.push('>');
                    walk(c, out);
                    out.push_str("</>");
                }
                roxmltree::NodeType::Text => {
                    let t = c.text().unwrap_or_default().replace("\r\n", "\n");
                    if !t.trim().is_empty() {
                        out.push_str(&t);
                    }
                }
                roxmltree::NodeType::Comment => out.push_str(&format!("<!--{}-->", c.text().unwrap_or_default().replace("\r\n", "\n"))),
                roxmltree::NodeType::PI => {
                    if let Some(pi) = c.pi() {
                        out.push_str(&format!("<?{} {}?>", pi.target, pi.value.unwrap_or_default()));
                    }
                }
                roxmltree::NodeType::Root => {}
            }
        }
    }
    match (parse(text), parse(xml)) {
        (Some(a), Some(b)) => !a.is_empty() && a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(n: &str) -> DataType {
        DataType { name: n.into(), ..Default::default() }
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(cmp_key(&Value::Decimal(150, 2), &Value::Decimal(15, 1)), Ordering::Equal);
        assert_eq!(cmp_key(&Value::Int(2), &Value::Decimal(15, 1)), Ordering::Greater);
        assert!(!same(&Value::Decimal(150, 2), &Value::Int(1), Equality::default()));
        assert!(same(&Value::Float(0.1 + 0.2), &Value::Float(0.3), Equality::default()));
        assert!(!same(&Value::Float(0.1 + 0.2), &Value::Float(0.3), Equality { full_precision: true, ..Default::default() }));
        assert!(same(&Value::Float(1.0), &Value::Float(1.00000001), Equality { real: true, ..Default::default() }));
        assert!(same(&Value::Int(5), &Value::Str("5".into()), Equality::default()));
        assert!(!same(&Value::Int(5), &Value::Str("05".into()), Equality::default()));
    }

    #[test]
    fn strings_and_bytes() {
        let ci = Fold::of(Some("Latin1_General_CI_AS"));
        let eq = Equality { fold: ci, ..Default::default() };
        assert!(same(&Value::Str("Abc".into()), &Value::Str("aBC".into()), eq));
        assert!(!same(&Value::Str("Abc".into()), &Value::Str("aBC".into()), Equality { binary: true, ..eq }));
        assert!(same(&Value::Str("a  ".into()), &Value::Str("a".into()), Equality { trim_trailing_spaces: true, ..eq }));
        assert!(same(&Value::Str("".into()), &Value::Null, Equality { empty_is_null: true, ..eq }));
        assert!(!same(&Value::Str("café".into()), &Value::Str("cafe".into()), eq), "accent-sensitive");
        let ai = Equality { fold: Fold::of(Some("Latin1_General_CI_AI")), ..Default::default() };
        assert!(same(&Value::Str("Café".into()), &Value::Str("cafe".into()), ai));
        assert!(same(&Value::Str("ＡＢＣ".into()), &Value::Str("abc".into()), eq), "width-insensitive without _WS");
        assert!(!same(&Value::Str("ＡＢＣ".into()), &Value::Str("abc".into()), Equality { fold: Fold::of(Some("Japanese_CI_AS_KS_WS")), ..eq }));
        assert!(same(&Value::Str("ひらがな".into()), &Value::Str("ヒラガナ".into()), eq), "kana-insensitive without _KS");
        assert_eq!(cmp_key(&Value::Bytes(vec![1, 0]), &Value::Bytes(vec![1])), Ordering::Greater, "trailing zeros break the tie by length");
        assert_eq!(cmp_key(&Value::Bytes(vec![1, 0]), &Value::Bytes(vec![1, 1])), Ordering::Less);
        assert_eq!(cmp_key(&Value::Bytes(vec![1, 0, 0]), &Value::Bytes(vec![2])), Ordering::Less);
        assert_eq!(cmp_key(&Value::Null, &Value::Int(0)), Ordering::Less);
        let (from, to) = Fold::of(Some("Latin1_General_CI_AI")).table();
        assert_eq!(from.chars().count(), to.chars().count());
    }

    #[test]
    fn cross_type_values() {
        let g = [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        assert_eq!(guid_text(&g), "00112233-4455-6677-8899-aabbccddeeff");
        let ci = Equality { fold: Fold::of(Some("Latin1_General_CI_AS")), ..Default::default() };
        assert!(same(&Value::Guid(g), &Value::Str("00112233-4455-6677-8899-AABBCCDDEEFF".into()), ci));
        assert!(same(&Value::Date(10), &Value::DateTime(10 * DAY), Equality::default()));
        assert!(!same(&Value::Date(10), &Value::DateTime(10 * DAY + 1), Equality::default()));
        let x = Equality { xml: (false, true), ..Default::default() };
        assert!(same(&Value::Str("<a  x='1'>t</a>".into()), &Value::Str("<a x=\"1\">t</a>".into()), x));
        assert!(!same(&Value::Str("<a x='2'>t</a>".into()), &Value::Str("<a x=\"1\">t</a>".into()), x));
        assert!(!same(&Value::Str("not xml <".into()), &Value::Str("<a/>".into()), x));
        let v = |t: &str, s: &str| Value::Variant(Box::new(Variant { base_type: t.into(), precision: 0, scale: 0, max_length: 4, collation: None, text: s.into() }));
        assert!(same(&v("int", "1"), &v("int", "1"), Equality::default()));
        assert!(!same(&v("int", "1"), &v("bigint", "1"), Equality::default()));
    }

    #[test]
    fn number_text() {
        assert_eq!(round_trip(0.1), "0.1");
        assert_eq!(round_trip(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(round_trip(1e20), "1E+20");
        assert_eq!(round_trip(-1.5e-7), "-1.5E-07");
        assert_eq!(round_trip(123456.0), "123456");
        assert_eq!(round_trip_f32(0.1f32), "0.1");
        assert_eq!(dotnet_g(1234.5, 15), "1234.5");
        assert_eq!(decimal_text(-1234, 2), "-12.34");
        assert_eq!(decimal_text(5, 3), "0.005");
    }

    #[test]
    fn literals() {
        let (nv, v) = (ty("nvarchar"), ty("varchar"));
        fn lit<'a>(s: &'a DataType, t: &'a DataType, c: Option<&'a str>) -> Literal<'a> {
            Literal { source: s, target: t, source_collation: c, target_collation: c, clr_as_text: true, graph_table: None }
        }
        assert_eq!(Value::Str("O'Neil".into()).sql(&lit(&nv, &nv, None)), "N'O''Neil'");
        assert_eq!(Value::Str("x".into()).sql(&lit(&v, &nv, Some("Latin1_General_CI_AS"))), "'x'");
        assert_eq!(Value::Str("x".into()).sql(&lit(&v, &v, Some("Cyrillic_General_CI_AS"))), "N'x'");
        assert_eq!(Value::Str("a\\\r\nb".into()).sql(&lit(&nv, &nv, None)), "N'a\\\\\r\n\r\nb'");
        assert_eq!(Value::Str("<?xml version=\"1.0\" encoding=\"utf-8\"?><a/>".into()).sql(&lit(&nv, &ty("xml"), None)), "CONVERT(xml,N'<a/>',1)");
        assert_eq!(Value::Bytes(vec![0xAB, 1]).sql(&lit(&ty("varbinary"), &ty("varbinary"), None)), "0xab01");
        let dt = ty("datetime");
        assert_eq!(Value::DateTime(86_400_000_000_000 + 1_996_666_666).sql(&lit(&dt, &dt, None)), "'0001-01-02 00:00:01.997'");
        let d2 = DataType { name: "datetime2".into(), scale: Some(7), ..Default::default() };
        assert_eq!(Value::DateTime(86_400_000_000_000 + 1_500_000_000).sql(&lit(&d2, &d2, None)), "'0001-01-02 00:00:01.5000000'");
        assert_eq!(Value::Date(0).sql(&lit(&ty("date"), &ty("date"), None)), "'0001-01-01'");
        let dto = DataType { name: "datetimeoffset".into(), scale: Some(0), ..Default::default() };
        assert_eq!(Value::DateTimeOffset(0, 90).sql(&lit(&dto, &dto, None)), "'0001-01-01 01:30:00 +01:30'");
        let g = Literal { graph_table: Some("N2"), ..lit(&nv, &nv, None) };
        assert_eq!(Value::Str("{\"id\":0}".into()).sql(&g), "JSON_MODIFY('{\"id\":0}','$.table','N2')");
        let var = Variant { base_type: "nvarchar".into(), precision: 0, scale: 0, max_length: 10, collation: Some("Latin1_General_CI_AS".into()), text: "hi".into() };
        assert_eq!(Value::Variant(Box::new(var)).sql(&lit(&ty("sql_variant"), &ty("sql_variant"), None)), "CAST(N'hi' AS nvarchar(5)) COLLATE Latin1_General_CI_AS");
        let h = ty("hierarchyid");
        assert_eq!(Value::Clr(vec![0x58], Some("/1/".into())).sql(&lit(&h, &h, None)), "N'/1/'");
        assert_eq!(Value::Clr(vec![0x58], Some("/1/".into())).sql(&Literal { clr_as_text: false, ..lit(&h, &h, None) }), "0x58");
    }

    #[test]
    fn csv_text() {
        assert_eq!(Value::Null.csv_text(&ty("int")), "<NULL>");
        assert_eq!(Value::Bytes(vec![1]).csv_text(&ty("varbinary")), "<Binary Data...>");
        assert_eq!(Value::Bool(true).csv_text(&ty("bit")), "True");
        assert_eq!(Value::DateTime(739_343 * DAY + 13 * 3_600_000_000_000).csv_text(&ty("datetime")), "4/3/2025 1:00:00 PM");
        let t = DataType { name: "time".into(), scale: Some(3), ..Default::default() };
        assert_eq!(Value::Time(1_500_000_000).csv_text(&t), "00:00:01.500");
    }
}
