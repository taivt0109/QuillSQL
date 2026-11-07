//! Stored column bytes as values, in the form the data engine reads them from a server.

use super::collation;
use super::mtf::{u16_at, u32_at};
use super::pages::PageStore;
use super::record::{Col, large_value, time_bytes};
use crate::data::value::{Value, Variant, hex};

pub const DAY: i128 = 86_400_000_000_000;
/// Days from 0001-01-01 to 1900-01-01.
const DAYS_TO_1900: i64 = 693_595;

/// System type ids.
pub mod xtype {
    pub const IMAGE: u8 = 34;
    pub const TEXT: u8 = 35;
    pub const GUID: u8 = 36;
    pub const DATE: u8 = 40;
    pub const TIME: u8 = 41;
    pub const DATETIME2: u8 = 42;
    pub const DATETIMEOFFSET: u8 = 43;
    pub const TINYINT: u8 = 48;
    pub const SMALLINT: u8 = 52;
    pub const INT: u8 = 56;
    pub const SMALLDATETIME: u8 = 58;
    pub const REAL: u8 = 59;
    pub const MONEY: u8 = 60;
    pub const DATETIME: u8 = 61;
    pub const FLOAT: u8 = 62;
    pub const VARIANT: u8 = 98;
    pub const NTEXT: u8 = 99;
    pub const BIT: u8 = 104;
    pub const DECIMAL: u8 = 106;
    pub const NUMERIC: u8 = 108;
    pub const SMALLMONEY: u8 = 122;
    pub const BIGINT: u8 = 127;
    pub const VARBINARY: u8 = 165;
    pub const VARCHAR: u8 = 167;
    pub const BINARY: u8 = 173;
    pub const CHAR: u8 = 175;
    pub const TIMESTAMP: u8 = 189;
    pub const NVARCHAR: u8 = 231;
    pub const NCHAR: u8 = 239;
    pub const CLR: u8 = 240;
    pub const XML: u8 = 241;
}

pub fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

fn le(b: &[u8]) -> u64 {
    b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64)
}

fn int(b: &[u8]) -> i64 {
    match b.len() {
        1 => b[0] as i64,
        2 => i16::from_le_bytes([b[0], b[1]]) as i64,
        4 => i32::from_le_bytes(b[..4].try_into().unwrap()) as i64,
        _ => i64::from_le_bytes(b[..8].try_into().unwrap()),
    }
}

fn time_ns(b: &[u8], scale: u8) -> i64 {
    le(b) as i64 * 10i64.pow(9 - scale.min(7) as u32)
}

fn days3(b: &[u8]) -> i64 {
    le(&b[..3]) as i64
}

/// A decimal: sign byte (1 = positive), then the magnitude, little-endian.
fn decimal(b: &[u8], scale: u8) -> Value {
    let mag = b[1..].iter().rev().fold(0i128, |v, &x| (v << 8) | x as i128);
    Value::Decimal(if b[0] == 1 { mag } else { -mag }, scale)
}

fn money(b: &[u8]) -> i64 {
    if b.len() == 4 { i32::from_le_bytes(b[..4].try_into().unwrap()) as i64 } else { i64::from_le_bytes(b[..8].try_into().unwrap()) }
}

/// Decodes one stored value of column `c`. `complex` marks a pointer to the value elsewhere.
pub fn decode(store: &PageStore, c: &Col, b: &[u8], complex: bool, db_collation: u32) -> Result<Value, String> {
    use xtype::*;
    let owned;
    let b = if matches!(c.xtype, TEXT | NTEXT | IMAGE) {
        owned = large_value(store, b, true)?;
        &owned[..]
    } else if complex {
        owned = large_value(store, b, false)?;
        &owned[..]
    } else {
        b
    };
    let collation = if c.collation != 0 { c.collation } else { db_collation };
    Ok(match c.xtype {
        TINYINT | SMALLINT | INT | BIGINT => Value::Int(int(b)),
        BIT => Value::Bool(b[0] & (1 << c.bit_pos) != 0),
        REAL => Value::Float(f32::from_le_bytes(b[..4].try_into().unwrap()) as f64),
        FLOAT => Value::Float(f64::from_le_bytes(b[..8].try_into().unwrap())),
        MONEY | SMALLMONEY => Value::Decimal(money(b) as i128, 4),
        DECIMAL | NUMERIC => decimal(b, c.scale),
        GUID => Value::Guid(b[..16].try_into().unwrap()),
        DATE => Value::Date(days3(b)),
        TIME => Value::Time(time_ns(b, c.scale)),
        DATETIME2 => {
            let t = time_bytes(c.scale);
            Value::DateTime(days3(&b[t..]) as i128 * DAY + time_ns(&b[..t], c.scale) as i128)
        }
        DATETIMEOFFSET => {
            let t = time_bytes(c.scale);
            let utc = days3(&b[t..]) as i128 * DAY + time_ns(&b[..t], c.scale) as i128;
            Value::DateTimeOffset(utc, i16::from_le_bytes([b[t + 3], b[t + 4]]) as i32)
        }
        DATETIME => {
            let days = i32::from_le_bytes(b[4..8].try_into().unwrap()) as i64 + DAYS_TO_1900;
            let ticks = u32_at(b, 0) as i128;
            Value::DateTime(days as i128 * DAY + ticks * 1_000_000_000 / 300)
        }
        SMALLDATETIME => {
            let days = u16_at(b, 2) as i64 + DAYS_TO_1900;
            Value::DateTime(days as i128 * DAY + u16_at(b, 0) as i128 * 60_000_000_000)
        }
        CHAR | VARCHAR | TEXT => Value::Str(collation::decode(b, collation::code_page(collation))),
        NCHAR | NVARCHAR | NTEXT => Value::Str(utf16(b)),
        VARIANT => decode_variant(b, db_collation)?,
        CLR => Value::Clr(b.to_vec(), None),
        _ => Value::Bytes(b.to_vec()),
    })
}

/// A sql_variant: base type, its properties, then the value.
pub fn decode_variant(b: &[u8], db_collation: u32) -> Result<Value, String> {
    use xtype::*;
    if b.len() < 2 {
        return Ok(Value::Null);
    }
    let base = b[0];
    let mut c = Col { xtype: base, ..Default::default() };
    let (value, max_length, collation) = match base {
        DECIMAL | NUMERIC => {
            c.prec = b[2];
            c.scale = b[3];
            (&b[4..], 17, None)
        }
        TIME | DATETIME2 | DATETIMEOFFSET => {
            c.scale = b[2];
            (&b[3..], 0, None)
        }
        CHAR | VARCHAR | NCHAR | NVARCHAR => {
            c.length = u16_at(b, 2) as i16;
            c.collation = u32_at(b, 4);
            (&b[8..], c.length as i32, collation::name(c.collation))
        }
        BINARY | VARBINARY => {
            c.length = u16_at(b, 2) as i16;
            (&b[4..], c.length as i32, None)
        }
        _ => (&b[2..], 0, None),
    };
    let store = PageStore::empty();
    let v = decode(&store, &c, value, false, db_collation)?;
    let base_type = type_name(base).to_string();
    let max_length = match base {
        TINYINT | BIT => 1,
        SMALLINT => 2,
        INT | REAL | SMALLMONEY | SMALLDATETIME => 4,
        BIGINT | FLOAT | MONEY | DATETIME => 8,
        GUID => 16,
        DATE => 3,
        TIME => 5,
        DATETIME2 => 8,
        DATETIMEOFFSET => 10,
        _ => max_length,
    };
    let text = variant_text(&v, base, c.scale);
    let precision = match base {
        DECIMAL | NUMERIC => c.prec,
        TINYINT => 3,
        SMALLINT => 5,
        INT => 10,
        BIGINT => 19,
        REAL => 24,
        FLOAT => 53,
        MONEY => 19,
        SMALLMONEY => 10,
        DATETIME => 23,
        SMALLDATETIME => 16,
        DATE => 10,
        BIT => 1,
        TIME => 8 + if c.scale > 0 { c.scale + 1 } else { 0 },
        DATETIME2 => 19 + if c.scale > 0 { c.scale + 1 } else { 0 },
        DATETIMEOFFSET => 26 + if c.scale > 0 { c.scale + 1 } else { 0 },
        _ => 0,
    };
    let scale = match base {
        DECIMAL | NUMERIC | TIME | DATETIME2 | DATETIMEOFFSET => c.scale,
        MONEY | SMALLMONEY => 4,
        DATETIME => 3,
        _ => 0,
    };
    Ok(Value::Variant(Box::new(Variant { base_type, precision, scale, max_length, collation, text })))
}

/// The text the data engine reads for a sql_variant: dates in ODBC canonical form (style 121),
/// floats in scientific form (style 3), money with two decimals (style 2), binary as 0x...
fn variant_text(v: &Value, base: u8, scale: u8) -> String {
    use xtype::*;
    match (v, base) {
        (Value::Float(f), REAL) => super::text::float_style3(*f, true),
        (Value::Float(f), _) => super::text::float_style3(*f, false),
        (Value::Decimal(d, _), MONEY | SMALLMONEY) => super::text::money_style2(*d),
        (Value::Bytes(b), _) => hex(b, true),
        (Value::Date(_) | Value::Time(_) | Value::DateTime(..) | Value::DateTimeOffset(..), _) => super::text::odbc(v, base, scale),
        (Value::Bool(b), _) => (*b as u8).to_string(),
        (Value::Guid(_), _) => v.display(),
        _ => v.display(),
    }
}

pub fn type_name(x: u8) -> &'static str {
    use xtype::*;
    match x {
        IMAGE => "image",
        TEXT => "text",
        GUID => "uniqueidentifier",
        DATE => "date",
        TIME => "time",
        DATETIME2 => "datetime2",
        DATETIMEOFFSET => "datetimeoffset",
        TINYINT => "tinyint",
        SMALLINT => "smallint",
        INT => "int",
        SMALLDATETIME => "smalldatetime",
        REAL => "real",
        MONEY => "money",
        DATETIME => "datetime",
        FLOAT => "float",
        VARIANT => "sql_variant",
        NTEXT => "ntext",
        BIT => "bit",
        DECIMAL => "decimal",
        NUMERIC => "numeric",
        SMALLMONEY => "smallmoney",
        BIGINT => "bigint",
        VARBINARY => "varbinary",
        VARCHAR => "varchar",
        BINARY => "binary",
        CHAR => "char",
        TIMESTAMP => "timestamp",
        NVARCHAR => "nvarchar",
        NCHAR => "nchar",
        XML => "xml",
        _ => "",
    }
}
