//! Rows of row- and page-compressed tables (the CD record format): a 4-bit description per
//! column, short values up to 8 bytes kept together, longer ones in a long data region. Pages of
//! page-compressed tables add an anchor record (a prefix per column) and a dictionary of values.

use super::mtf::u16_at;
use super::pages::PAGE;

/// One column of a compressed record, with any page prefix and dictionary resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CdField {
    Null,
    /// A zero-length value (a zero number, an empty string, a false bit).
    Empty,
    /// A true bit.
    True,
    Bytes(Vec<u8>),
    /// A pointer to the value elsewhere (a large value root or row-overflow pointer).
    Complex(Vec<u8>),
}

const CD_NULL: u8 = 0;
const CD_EMPTY: u8 = 1;
const CD_LONG: u8 = 10;
const CD_BIT: u8 = 11;
const CD_SYMBOL: u8 = 12;

/// The compression information of a page-compressed page.
#[derive(Clone, Debug, Default)]
pub struct PageInfo {
    /// Per column (0-based): the anchor value, when the column has one.
    anchor: Vec<Option<Vec<u8>>>,
    dictionary: Vec<Vec<u8>>,
}

/// Whether the page carries compression information (page compression).
pub fn has_info(page: &[u8]) -> bool {
    page[2] & 0x80 != 0
}

impl PageInfo {
    /// Reads the compression information record at the start of the page's data.
    pub fn read(page: &[u8]) -> PageInfo {
        let mut info = PageInfo::default();
        let ci = &page[96..PAGE];
        let flags = ci[0];
        let dictionary_at = u16_at(ci, 3) as usize;
        if flags & 0x02 != 0 {
            let anchor = &ci[7..];
            info.anchor = parse(anchor, None).map(|(fields, _)| fields.into_iter().map(|f| match f {
                CdField::Bytes(b) => Some(b),
                CdField::Empty => Some(Vec::new()),
                _ => None,
            }).collect()).unwrap_or_default();
        }
        if flags & 0x04 != 0 && dictionary_at < ci.len() {
            let d = &ci[dictionary_at..];
            let n = u16_at(d, 0) as usize;
            let data = 2 + 2 * n;
            let mut start = data;
            for i in 0..n {
                let end = u16_at(d, 2 + 2 * i) as usize;
                info.dictionary.push(d.get(start..end).unwrap_or_default().to_vec());
                start = end;
            }
        }
        info
    }

    /// A stored value of column `i`: with an anchor it is a prefix length and a suffix, and
    /// nothing stored means the anchor value itself.
    fn expand(&self, i: usize, stored: &[u8]) -> Vec<u8> {
        let Some(Some(anchor)) = self.anchor.get(i) else { return stored.to_vec() };
        if stored.is_empty() {
            return anchor.clone();
        }
        let (prefix, rest) = if stored[0] & 0x80 != 0 && stored.len() >= 2 {
            ((((stored[0] & 0x7f) as usize) << 8) | stored[1] as usize, &stored[2..])
        } else {
            (stored[0] as usize, &stored[1..])
        };
        let mut v = anchor[..prefix.min(anchor.len())].to_vec();
        v.extend_from_slice(rest);
        v
    }
}

/// The record type of a compressed record (same bits as a FixedVar record).
pub fn kind(record: &[u8]) -> u8 {
    (record[0] >> 1) & 7
}

pub fn is_compressed(record: &[u8]) -> bool {
    record[0] & 1 != 0
}

/// Parses a compressed record into its columns, in CD order; also returns the record's length.
pub fn parse(r: &[u8], page: Option<&PageInfo>) -> Option<(Vec<CdField>, usize)> {
    let header = *r.first()?;
    let mut at = 1;
    let mut columns = *r.get(at)? as usize;
    at += 1;
    if columns & 0x80 != 0 {
        columns = ((columns & 0x7f) << 8) | *r.get(at)? as usize;
        at += 1;
    }
    let cd: Vec<u8> = (0..columns).map(|i| r.get(at + i / 2).map_or(CD_NULL, |b| if i % 2 == 0 { b & 15 } else { b >> 4 })).collect();
    at += columns.div_ceil(2);
    let clusters = columns.div_ceil(30);
    if clusters > 1 {
        // Short data sizes of all clusters but the last.
        at += clusters - 1;
    }
    // Short data.
    let mut fields = Vec::with_capacity(columns);
    let mut long_columns = Vec::new();
    for (i, &d) in cd.iter().enumerate() {
        let f = match d {
            CD_NULL => CdField::Null,
            CD_EMPTY => match page {
                Some(p) if p.anchor.get(i).is_some_and(Option::is_some) => CdField::Bytes(p.expand(i, &[])),
                _ => CdField::Empty,
            },
            CD_BIT => CdField::True,
            CD_LONG => {
                long_columns.push(i);
                CdField::Null
            }
            CD_SYMBOL => {
                let s = *r.get(at)? as usize;
                at += 1;
                let value = page.and_then(|p| p.dictionary.get(s)).cloned().unwrap_or_default();
                CdField::Bytes(page.map_or(value.clone(), |p| p.expand(i, &value)))
            }
            n => {
                let len = (n - 1) as usize;
                let b = r.get(at..at + len)?;
                at += len;
                CdField::Bytes(page.map_or(b.to_vec(), |p| p.expand(i, b)))
            }
        };
        fields.push(f);
    }
    if header & 0x20 != 0 {
        // Long data region: a flags byte, the count, the end offsets (high bit: complex).
        let flags = *r.get(at)?;
        at += 1;
        let n = u16_at(r, at) as usize;
        at += 2;
        let ends: Vec<u16> = (0..n).map(|k| u16_at(r, at + 2 * k)).collect();
        at += 2 * n;
        if flags & 0x02 != 0 {
            // Long data cluster array: one count per cluster of 30 columns.
            at += clusters;
        }
        let start = at;
        let mut prev = 0usize;
        for (k, &end) in ends.iter().enumerate() {
            let stop = (end & 0x7fff) as usize;
            let b = r.get(start + prev..start + stop)?;
            if let Some(&i) = long_columns.get(k) {
                fields[i] = if end & 0x8000 != 0 { CdField::Complex(b.to_vec()) } else { CdField::Bytes(page.map_or(b.to_vec(), |p| p.expand(i, b))) };
            }
            prev = stop;
        }
        at = start + prev;
    }
    Some((fields, at.max(9)))
}

/// A compressed integer: big-endian, as few bytes as the value needs, with the sign bit flipped.
pub fn int(b: &[u8]) -> i64 {
    if b.is_empty() {
        return 0;
    }
    let n = b.len().min(8);
    let mut v = b[..n].iter().fold(0u64, |v, &x| (v << 8) | x as u64);
    v ^= 1 << (8 * n - 1);
    // Sign-extend from n bytes.
    let shift = 64 - 8 * n as u32;
    ((v << shift) as i64) >> shift
}

/// A compressed float or real: its little-endian bytes without the low-order zero bytes.
pub fn float_bytes(b: &[u8], width: usize) -> Vec<u8> {
    let mut v = vec![0u8; width.saturating_sub(b.len())];
    v.extend_from_slice(&b[b.len().saturating_sub(width)..]);
    v
}

/// A vardecimal: sign and exponent in the first byte, then the digits in 10-bit groups of three.
/// Returns the value scaled to `scale` decimal places.
pub fn decimal(b: &[u8], scale: u8) -> i128 {
    if b.is_empty() {
        return 0;
    }
    let positive = b[0] & 0x80 != 0;
    let exponent = (b[0] & 0x7f) as i32 - 64;
    let bits = (b.len() - 1) * 8;
    // Trailing zero bits may be cut off the last group.
    let groups = bits.div_ceil(10);
    let mut digits: i128 = 0;
    let mut count = 0i32;
    for g in 0..groups {
        let mut v = 0u32;
        for k in 0..10 {
            let bit = g * 10 + k;
            let byte = b.get(1 + bit / 8).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - bit % 8)) & 1) as u32;
        }
        digits = digits * 1000 + v as i128;
        count += 3;
    }
    // value = 0.digits × 10^(exponent + 1)
    let power = exponent + 1 + scale as i32 - count;
    let v = if power >= 0 { digits * 10i128.pow(power as u32) } else { digits / 10i128.pow((-power) as u32) };
    if positive { v } else { -v }
}

/// Standard Compression Scheme for Unicode, as SQL Server stores compressed nchar/nvarchar
/// (an odd length marks compressed data; a trailing SC0 pads it).
pub fn scsu(b: &[u8]) -> String {
    const STATIC: [u32; 8] = [0x0000, 0x0080, 0x0100, 0x0300, 0x2000, 0x2080, 0x2100, 0x3000];
    let mut dynamic: [u32; 8] = [0x0080, 0x00C0, 0x0400, 0x0600, 0x0900, 0x3040, 0x30A0, 0xFF00];
    let mut window = 0usize;
    let mut out = String::new();
    let push = |c: u32, out: &mut String| out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
    let offset = |x: u8| -> u32 {
        match x {
            0x01..=0x67 => x as u32 * 0x80,
            0x68..=0xA7 => x as u32 * 0x80 + 0xAC00,
            0xF9 => 0x00C0,
            0xFA => 0x0250,
            0xFB => 0x0370,
            0xFC => 0x0530,
            0xFD => 0x3040,
            0xFE => 0x30A0,
            0xFF => 0xFF60,
            _ => 0,
        }
    };
    let mut i = 0;
    let mut unicode = false;
    let mut pending_high: Option<u32> = None;
    let mut emit = |c: u32, out: &mut String| {
        if (0xD800..0xDC00).contains(&c) {
            pending_high = Some(c);
        } else if (0xDC00..0xE000).contains(&c) {
            if let Some(h) = pending_high.take() {
                push(0x10000 + ((h - 0xD800) << 10) + (c - 0xDC00), out);
            }
        } else {
            push(c, out);
        }
    };
    while i < b.len() {
        let x = b[i];
        i += 1;
        if unicode {
            match x {
                0xE0..=0xE7 => {
                    window = (x - 0xE0) as usize;
                    unicode = false;
                }
                0xE8..=0xEF => {
                    if let Some(&y) = b.get(i) {
                        window = (x - 0xE8) as usize;
                        dynamic[window] = offset(y);
                        i += 1;
                    }
                    unicode = false;
                }
                0xF0 => {
                    if i + 1 < b.len() {
                        emit(((b[i] as u32) << 8) | b[i + 1] as u32, &mut out);
                    }
                    i += 2;
                }
                0xF1 => {
                    if i + 2 < b.len() {
                        let n = ((b[i] as usize) << 8) | b[i + 1] as usize;
                        window = n >> 13;
                        dynamic[window] = 0x10000 + ((n as u32 & 0x1FFF) << 7);
                        i += 3;
                    }
                    unicode = false;
                }
                _ => {
                    if i < b.len() {
                        emit(((x as u32) << 8) | b[i] as u32, &mut out);
                    }
                    i += 1;
                }
            }
            continue;
        }
        match x {
            0x00 | 0x09 | 0x0A | 0x0D | 0x20..=0x7F => emit(x as u32, &mut out),
            0x80..=0xFF => emit(dynamic[window] + (x - 0x80) as u32, &mut out),
            // SQn: quote one character from window n.
            0x01..=0x08 => {
                if let Some(&y) = b.get(i) {
                    let n = (x - 1) as usize;
                    emit(if y < 0x80 { STATIC[n] + y as u32 } else { dynamic[n] + (y - 0x80) as u32 }, &mut out);
                }
                i += 1;
            }
            // SDX: define an extended window.
            0x0B => {
                if i + 1 < b.len() {
                    let n = ((b[i] as usize) << 8) | b[i + 1] as usize;
                    window = n >> 13;
                    dynamic[window] = 0x10000 + ((n as u32 & 0x1FFF) << 7);
                }
                i += 2;
            }
            // SQU: one UTF-16 code unit.
            0x0E => {
                if i + 1 < b.len() {
                    emit(((b[i] as u32) << 8) | b[i + 1] as u32, &mut out);
                }
                i += 2;
            }
            // SCU: switch to Unicode mode.
            0x0F => unicode = true,
            // SCn: change to window n.
            0x10..=0x17 => window = (x - 0x10) as usize,
            // SDn: define window n.
            0x18..=0x1F => {
                if let Some(&y) = b.get(i) {
                    window = (x - 0x18) as usize;
                    dynamic[window] = offset(y);
                }
                i += 1;
            }
            _ => {}
        }
    }
    out
}

/// nchar/nvarchar in a compressed record: compressed when the length is odd, else UTF-16.
pub fn unicode(b: &[u8]) -> String {
    if b.len() % 2 == 1 { scsu(b) } else { super::values::utf16(b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(int(&[0x81]), 1);
        assert_eq!(int(&[0x7e]), -2);
        assert_eq!(int(&[0x7e, 0xd4]), -300);
        assert_eq!(int(&[0x81, 0x1f, 0x71, 0xfb, 0x04, 0xcb]), 1_234_567_890_123);
        assert_eq!(int(&[0; 8]), i64::MIN);
        assert_eq!(decimal(&[0xc1, 0x1e, 0xdc, 0x80], 4), 123_456);
        assert_eq!(decimal(&[0x3c, 0x19], 4), -1);
        assert_eq!(decimal(&[0xcd, 0xf9, 0xfe, 0x7f, 0x9f, 0xe7, 0xf9, 0xfe, 0x70], 4), 999_999_999_999_999_999);
        assert_eq!(f64::from_le_bytes(float_bytes(&[0xf8, 0x3f], 8).try_into().unwrap()), 1.5);
    }

    #[test]
    fn unicode_compression() {
        assert_eq!(scsu(&[0x78, 0x79, 0x10]), "xy");
        let s = [0x68, 0xe9, 0x6c, 0x6c, 0x6f, 0x20, 0x77, 0xf6, 0x72, 0x6c, 0x64, 0x20, 0x0e, 0x65, 0xe5, 0x0e, 0x67, 0x2c, 0x10];
        assert_eq!(scsu(&s), "héllo wörld 日本");
        assert_eq!(scsu(&[0x1f, 0xfb, 0xb9, 0xb9, 0x10]), "ΩΩ");
    }

    #[test]
    fn records() {
        // id = 2, ti NULL, si 0, bi -1 (from a row-compressed page).
        let r = [0x0d, 0x04, 0x02, 0x21, 0x82, 0x7f, 0, 0, 0];
        let (f, _) = parse(&r, None).unwrap();
        assert_eq!(f, vec![CdField::Bytes(vec![0x82]), CdField::Null, CdField::Empty, CdField::Bytes(vec![0x7f])]);
    }
}
