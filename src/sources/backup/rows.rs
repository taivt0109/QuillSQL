//! The rows of a table in a backup, as the data engine reads them from a server: every
//! partition of the heap or clustered index, compressed or not, large values followed to their
//! pages.

use std::collections::HashMap;

use super::compressed::{self, CdField, PageInfo};
use super::pages;
use super::record::{Col, Field, RECORD_FORWARDED, RECORD_PRIMARY, Record, large_value};
use super::tables::Catalog;
use super::values::{self, xtype};
use crate::data::value::Value;

/// How a column's values are wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Value,
    /// Large values above this many bytes come as their length and SHA-256.
    Hashed(i64),
}

/// Finds a table (or view with an index) by schema and name.
pub fn object_id(cat: &Catalog, schema: &str, name: &str) -> Option<i32> {
    let schemas: HashMap<i64, String> = cat.system("sysclsobjs").into_iter().filter(|r| r.int("class") == 50).map(|r| (r.int("id"), r.str("name"))).collect();
    cat.table_rows(34)
        .into_iter()
        .find(|r| r.str("name") == name && schemas.get(&r.int("nsid")).is_some_and(|s| s == schema) && matches!(r.str("type").trim(), "U" | "V"))
        .map(|r| r.int("id") as i32)
}

/// The values of `columns` (by name) of every row of the table.
pub fn read(cat: &Catalog, object: i32, columns: &[(String, Want)]) -> Result<Vec<Vec<Value>>, String> {
    let mut out = Vec::new();
    let levels: HashMap<u64, i64> = cat.rowset_rows.iter().map(|r| (r.int("rowsetid") as u64, r.int("cmprlevel"))).collect();
    for (_, rowset) in cat.partitions(object) {
        let cols = cat.columns(object, rowset);
        let picked: Vec<(Option<&Col>, Want)> = columns.iter().map(|(n, w)| (cols.iter().find(|c| &c.name == n), *w)).collect();
        if let Some((n, _)) = columns.iter().zip(&picked).find(|(_, p)| p.0.is_none()).map(|(c, _)| c) {
            return Err(format!("The column '{n}' was not found"));
        }
        let compressed = levels.get(&rowset).copied().unwrap_or(0) > 0;
        for id in cat.data_pages(rowset) {
            let Some(p) = cat.store.page(id) else { continue };
            read_page(cat, &p, compressed, &picked, &mut out)?;
        }
    }
    Ok(out)
}

fn read_page(cat: &Catalog, p: &[u8], compressed: bool, picked: &[(Option<&Col>, Want)], out: &mut Vec<Vec<Value>>) -> Result<(), String> {
    let h = pages::header(p);
    let info = (compressed && compressed::has_info(p)).then(|| PageInfo::read(p));
    for s in 0..h.slots {
        let Some(bytes) = pages::slot(p, s) else { continue };
        if compressed && compressed::is_compressed(bytes) {
            let kind = compressed::kind(bytes);
            if kind != RECORD_PRIMARY && kind != RECORD_FORWARDED {
                continue;
            }
            let (fields, _) = compressed::parse(bytes, info.as_ref()).ok_or_else(|| format!("unreadable compressed row on page {}", h.id))?;
            let mut row = Vec::with_capacity(picked.len());
            for (c, want) in picked {
                let c = c.unwrap();
                let i = (-c.offset - 1).max(0) as usize;
                let f = fields.get(i).cloned().unwrap_or(CdField::Null);
                row.push(compressed_value(cat, c, f, *want)?);
            }
            out.push(row);
            continue;
        }
        let Some(r) = Record::parse(bytes) else { continue };
        if r.kind != RECORD_PRIMARY && r.kind != RECORD_FORWARDED {
            continue;
        }
        let mut row = Vec::with_capacity(picked.len());
        for (c, want) in picked {
            let c = c.unwrap();
            let v = match r.field(c) {
                Field::Null => Value::Null,
                Field::Missing => match &c.default {
                    Some(d) => values::decode_variant(d, cat.collation)?,
                    None => Value::Null,
                },
                Field::Bytes(b) if is_lob(c) => lob(cat, c, b.to_vec(), *want)?,
                Field::Complex(b) if is_lob(c) || c.xtype == xtype::CLR || c.xtype == xtype::XML => lob(cat, c, large_value(cat.store, b, false)?, *want)?,
                Field::Bytes(b) => values::decode(cat.store, c, b, false, cat.collation)?,
                Field::Complex(b) => values::decode(cat.store, c, b, true, cat.collation)?,
            };
            row.push(v);
        }
        out.push(row);
    }
    Ok(())
}

fn is_lob(c: &Col) -> bool {
    matches!(c.xtype, xtype::TEXT | xtype::NTEXT | xtype::IMAGE | xtype::XML) || (matches!(c.xtype, xtype::VARCHAR | xtype::NVARCHAR | xtype::VARBINARY) && c.length == -1)
}

/// A large value from its bytes (text pointers followed); hashed when it is above the limit.
fn lob(cat: &Catalog, c: &Col, mut bytes: Vec<u8>, want: Want) -> Result<Value, String> {
    if matches!(c.xtype, xtype::TEXT | xtype::NTEXT | xtype::IMAGE) {
        bytes = large_value(cat.store, &bytes, true)?;
    }
    let plain = |b: &[u8]| -> Result<Value, String> {
        if c.xtype == xtype::XML {
            return super::binxml::decode(b).map(Value::Str);
        }
        let as_inline = Col { xtype: match c.xtype {
            xtype::TEXT => xtype::VARCHAR,
            xtype::NTEXT => xtype::NVARCHAR,
            xtype::IMAGE => xtype::VARBINARY,
            x => x,
        }, ..c.clone() };
        values::decode(cat.store, &as_inline, b, false, cat.collation)
    };
    match want {
        Want::Hashed(limit) if bytes.len() as i64 > limit => {
            let hashed = if c.xtype == xtype::XML {
                match plain(&bytes)? {
                    Value::Str(s) => s.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                    _ => Vec::new(),
                }
            } else {
                bytes.clone()
            };
            Ok(Value::Large(bytes.len() as i64, super::sha256::digest(&hashed).to_vec()))
        }
        _ => plain(&bytes),
    }
}

/// A column of a compressed row as a value.
fn compressed_value(cat: &Catalog, c: &Col, f: CdField, want: Want) -> Result<Value, String> {
    use xtype::*;
    let b = match f {
        CdField::Null => return Ok(Value::Null),
        CdField::True => return Ok(Value::Bool(true)),
        CdField::Complex(p) => {
            let bytes = large_value(cat.store, &p, false)?;
            return if is_lob(c) || c.xtype == CLR || c.xtype == XML { lob(cat, c, bytes, want) } else { values::decode(cat.store, c, &bytes, false, cat.collation) };
        }
        CdField::Empty => Vec::new(),
        CdField::Bytes(b) => b,
    };
    let pad = |b: &[u8], n: usize, with: u8| {
        let mut v = b.to_vec();
        if v.len() < n {
            v.resize(n, with);
        }
        v
    };
    let collation = if c.collation != 0 { c.collation } else { cat.collation };
    Ok(match c.xtype {
        BIT => Value::Bool(false),
        TINYINT => Value::Int(b.first().copied().unwrap_or(0) as i64),
        SMALLINT | INT | BIGINT => Value::Int(compressed::int(&b)),
        MONEY | SMALLMONEY => Value::Decimal(compressed::int(&b) as i128, 4),
        DECIMAL | NUMERIC => Value::Decimal(compressed::decimal(&b, c.scale), c.scale),
        FLOAT => Value::Float(f64::from_le_bytes(compressed::float_bytes(&b, 8).try_into().unwrap())),
        REAL => Value::Float(f32::from_le_bytes(compressed::float_bytes(&b, 4).try_into().unwrap()) as f64),
        DATETIME => {
            let v = compressed::int(&b);
            let raw = [((v as u32).to_le_bytes()), (((v >> 32) as i32).to_le_bytes())].concat();
            values::decode(cat.store, c, &raw, false, collation)?
        }
        CHAR => Value::Str(super::collation::decode(&pad(&b, c.length.max(0) as usize, b' '), super::collation::code_page(collation))),
        NCHAR => {
            let mut s = compressed::unicode(&b);
            let n = (c.length.max(0) / 2) as usize;
            while s.encode_utf16().count() < n {
                s.push(' ');
            }
            Value::Str(s)
        }
        NVARCHAR if c.length != -1 => Value::Str(compressed::unicode(&b)),
        BINARY => Value::Bytes(pad(&b, c.length.max(0) as usize, 0)),
        _ if is_lob(c) => lob(cat, c, b, want)?,
        // Low-order zero bytes are left out.
        SMALLDATETIME => values::decode(cat.store, c, &compressed::float_bytes(&b, 4), false, collation)?,
        DATE | TIME | DATETIME2 | DATETIMEOFFSET | GUID | TIMESTAMP => {
            let n = super::record::fixed_length(c);
            values::decode(cat.store, c, &pad(&b, n, 0), false, collation)?
        }
        _ => values::decode(cat.store, c, &b, false, collation)?,
    })
}
