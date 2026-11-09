//! Rows on data pages (the FixedVar record format) and the large values they point to.

use super::mtf::{u16_at, u32_at};
use super::pages::{PageId, PageStore, slot};

/// A column's physical place in a record (from sysrscols) and its declared type (from
/// syscolpars).
#[derive(Clone, Debug, Default)]
pub struct Col {
    pub id: i32,
    pub name: String,
    pub xtype: u8,
    /// Declared length in bytes; -1 for (max) and the LOB types.
    pub length: i16,
    pub prec: u8,
    pub scale: u8,
    pub collation: u32,
    /// > 0: offset in the fixed part; < 0: -(index + 1) among the variable columns; 0: sparse.
    pub offset: i16,
    /// 1-based bit in the null bitmap.
    pub null_bit: u16,
    pub bit_pos: u8,
    /// The value rows written before the column was added take (an online added column).
    pub default: Option<Vec<u8>>,
}

pub const RECORD_PRIMARY: u8 = 0;
pub const RECORD_FORWARDED: u8 = 1;

/// A data record's layout.
pub struct Record<'a> {
    pub bytes: &'a [u8],
    pub kind: u8,
    columns: usize,
    nulls: Option<&'a [u8]>,
    fixed_end: usize,
    /// End offsets of the variable columns (high bit: complex column).
    var_ends: Vec<u16>,
    var_start: usize,
}

/// One column's stored bytes.
pub enum Field<'a> {
    Null,
    /// The record predates the column.
    Missing,
    Bytes(&'a [u8]),
    /// A pointer to the value elsewhere: row overflow or the root of a large value.
    Complex(&'a [u8]),
}

impl<'a> Record<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Record<'a>> {
        if bytes.len() < 4 {
            return None;
        }
        let status = bytes[0];
        let kind = (status >> 1) & 7;
        let fixed_end = u16_at(bytes, 2) as usize;
        if fixed_end < 4 || fixed_end + 2 > bytes.len() {
            return Some(Record { bytes, kind, columns: 0, nulls: None, fixed_end: fixed_end.min(bytes.len()), var_ends: Vec::new(), var_start: 0 });
        }
        let columns = u16_at(bytes, fixed_end) as usize;
        let mut at = fixed_end + 2;
        let nulls = if status & 0x10 != 0 {
            let n = columns.div_ceil(8);
            let b = bytes.get(at..at + n)?;
            at += n;
            Some(b)
        } else {
            None
        };
        let mut var_ends = Vec::new();
        if status & 0x20 != 0 && at + 2 <= bytes.len() {
            let n = u16_at(bytes, at) as usize;
            at += 2;
            for i in 0..n {
                var_ends.push(u16_at(bytes, at + 2 * i));
            }
            at += 2 * n;
        }
        Some(Record { bytes, kind, columns, nulls, fixed_end, var_ends, var_start: at })
    }

    /// The bytes of column `c`.
    pub fn field(&self, c: &Col) -> Field<'a> {
        if c.null_bit as usize > self.columns {
            return Field::Missing;
        }
        if let (Some(n), Some(bit)) = (self.nulls, (c.null_bit as usize).checked_sub(1))
            && n.get(bit / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0)
        {
            return Field::Null;
        }
        if c.offset > 0 {
            let at = c.offset as usize;
            let len = fixed_length(c);
            if at + len > self.fixed_end {
                return Field::Missing;
            }
            return Field::Bytes(&self.bytes[at..at + len]);
        }
        if c.offset == 0 {
            return self.sparse(c.id);
        }
        let i = (-c.offset - 1) as usize;
        // Trailing empty variable columns are not stored; the null bitmap tells them from NULLs.
        let Some(&end) = self.var_ends.get(i) else { return if self.nulls.is_some() { Field::Bytes(&[]) } else { Field::Null } };
        let start = if i == 0 { self.var_start } else { (self.var_ends[i - 1] & 0x7fff) as usize };
        let stop = (end & 0x7fff) as usize;
        if stop < start || stop > self.bytes.len() {
            return Field::Null;
        }
        let b = &self.bytes[start..stop];
        if end & 0x8000 != 0 { Field::Complex(b) } else { Field::Bytes(b) }
    }

    /// A sparse column: its value in the row's sparse vector (a complex column of type 5:
    /// count, column ids, end offsets, then the values), NULL when it is not there.
    fn sparse(&self, column: i32) -> Field<'a> {
        for (i, &end) in self.var_ends.iter().enumerate() {
            if end & 0x8000 == 0 {
                continue;
            }
            let start = if i == 0 { self.var_start } else { (self.var_ends[i - 1] & 0x7fff) as usize };
            let Some(v) = self.bytes.get(start..(end & 0x7fff) as usize) else { continue };
            if v.len() < 4 || u16_at(v, 0) != 5 {
                continue;
            }
            let n = u16_at(v, 2) as usize;
            let ends = 4 + 2 * n;
            let data = ends + 2 * n;
            for k in 0..n {
                if u16_at(v, 4 + 2 * k) as i32 == column {
                    let from = if k == 0 { data } else { u16_at(v, ends + 2 * (k - 1)) as usize };
                    let to = u16_at(v, ends + 2 * k) as usize;
                    return v.get(from..to).map_or(Field::Null, Field::Bytes);
                }
            }
        }
        Field::Null
    }

    /// For a forwarding stub: where the row lives.
    pub fn forwarded_to(&self) -> Option<(PageId, u16)> {
        (self.kind == 2 && self.bytes.len() >= 9).then(|| (PageId::at(self.bytes, 1), u16_at(self.bytes, 7)))
    }
}

/// Bytes a fixed-length column takes in the record.
pub fn fixed_length(c: &Col) -> usize {
    match c.xtype {
        48 | 104 => 1,                           // tinyint, bit
        52 => 2,                                 // smallint
        56 | 58 | 59 | 122 => 4,                 // int, smalldatetime, real, smallmoney
        60 | 61 | 62 | 127 | 189 => 8,           // money, datetime, float, bigint, timestamp
        36 => 16,                                // uniqueidentifier
        40 => 3,                                 // date
        41 => time_bytes(c.scale),               // time
        42 => 3 + time_bytes(c.scale),           // datetime2
        43 => 5 + time_bytes(c.scale),           // datetimeoffset
        106 | 108 => decimal_bytes(c.prec),      // decimal, numeric
        _ => c.length.max(0) as usize,           // char, nchar, binary
    }
}

pub fn time_bytes(scale: u8) -> usize {
    match scale {
        0..=2 => 3,
        3 | 4 => 4,
        _ => 5,
    }
}

fn decimal_bytes(prec: u8) -> usize {
    match prec {
        0..=9 => 5,
        10..=19 => 9,
        20..=28 => 13,
        _ => 17,
    }
}

/// Reads a record by its row id (page, slot).
pub fn record_at(store: &PageStore, page: PageId, slot_no: u16) -> Option<Vec<u8>> {
    let p = store.page(page)?;
    let r = slot(&p, slot_no)?;
    let len = record_length(r);
    Some(r[..len.min(r.len())].to_vec())
}

/// The length of a record from its own layout, for records read straight from a page.
fn record_length(r: &[u8]) -> usize {
    let kind = (r[0] >> 1) & 7;
    if kind == 4 {
        // A blob fragment carries its length.
        return u16_at(r, 2) as usize;
    }
    match Record::parse(r) {
        Some(rec) => match rec.var_ends.last() {
            Some(&e) => (e & 0x7fff) as usize,
            None => rec.var_start.max(rec.fixed_end),
        },
        None => r.len(),
    }
}

/// The whole of a large value: from an in-row root or row-overflow pointer (`complex`), or
/// from a text pointer (`text_pointer`).
pub fn large_value(store: &PageStore, bytes: &[u8], text_pointer: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    if text_pointer {
        if bytes.len() < 16 {
            return Err("short text pointer".into());
        }
        fragment(store, PageId::at(bytes, 8), u16_at(bytes, 14), &mut out, 0)?;
        return Ok(out);
    }
    if bytes.len() < 12 {
        return Err("short large value pointer".into());
    }
    match bytes[0] {
        // In-row root (4) or row overflow (2): 12-byte header, then (size, row id) links.
        2 | 4 => {
            let mut total = 0usize;
            for link in bytes[12..].as_chunks::<12>().0 {
                total = u32_at(link, 0) as usize;
                fragment(store, PageId::at(link, 4), u16_at(link, 10), &mut out, 0)?;
            }
            out.truncate(total);
            Ok(out)
        }
        t => Err(format!("unsupported large value pointer type {t}")),
    }
}

fn fragment(store: &PageStore, page: PageId, slot_no: u16, out: &mut Vec<u8>, depth: u32) -> Result<(), String> {
    if depth > 16 {
        return Err("large value tree too deep".into());
    }
    let r = record_at(store, page, slot_no).ok_or_else(|| format!("large value fragment {page}:{slot_no} is not in the backup"))?;
    if r.len() < 14 {
        return Err(format!("short large value fragment at {page}:{slot_no}"));
    }
    match u16_at(&r, 12) {
        // Data.
        3 => out.extend_from_slice(&r[14..]),
        // Small root: the data follows a short header.
        0 => {
            let n = u16_at(&r, 14) as usize;
            out.extend_from_slice(r.get(20..20 + n).unwrap_or(&r[20.min(r.len())..]));
        }
        // Internal node: 16-byte links (offset, row id).
        2 => {
            let n = u16_at(&r, 16) as usize;
            let start = out.len();
            let mut end = 0;
            for link in r.get(20..20 + 16 * n).unwrap_or_default().as_chunks::<16>().0 {
                end = u32_at(link, 0) as usize;
                fragment(store, PageId::at(link, 8), u16_at(link, 14), out, depth + 1)?;
            }
            out.truncate(start + end);
        }
        // Large root: 12-byte links (size, row id).
        1 | 5 => {
            let n = u16_at(&r, 16) as usize;
            let start = out.len();
            let mut end = 0;
            for link in r.get(24..24 + 12 * n).unwrap_or_default().as_chunks::<12>().0 {
                end = u32_at(link, 0) as usize;
                fragment(store, PageId::at(link, 4), u16_at(link, 10), out, depth + 1)?;
            }
            out.truncate(start + end);
        }
        t => return Err(format!("unsupported large value fragment type {t} at {page}:{slot_no}")),
    }
    Ok(())
}
