//! Comparison results on disk: one append-only file of differing rows per table, plus the
//! offsets of each kind of row, so a page of any kind can be read without loading the table.
//! With "Compress temporary files" each row is stored compressed when that makes it smaller.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::value::{Value, Variant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum RowKind {
    Identical,
    Different,
    /// Only in database 1.
    OnlyIn1,
    /// Only in database 2.
    OnlyIn2,
}

impl RowKind {
    pub const ALL: [RowKind; 4] = [RowKind::Identical, RowKind::Different, RowKind::OnlyIn1, RowKind::OnlyIn2];

    fn tag(self) -> u8 {
        self as u8
    }

    fn from_tag(t: u8) -> Option<RowKind> {
        RowKind::ALL.get(t as usize).copied()
    }
}

/// One stored row: the values of both sides (empty on the side the row is missing from) and,
/// for Different rows, which compared columns differ.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRow {
    pub kind: RowKind,
    pub source: Vec<Value>,
    pub target: Vec<Value>,
    pub differs: Vec<bool>,
}

/// A directory of result files for one comparison run; removed when dropped.
pub struct Session {
    pub dir: PathBuf,
    keep: bool,
}

impl Session {
    /// `%TEMP%\QuillSQL\dc\<time>-<pid>`; also removes sessions older than a day.
    pub fn create() -> std::io::Result<Session> {
        let root = std::env::temp_dir().join("QuillSQL").join("dc");
        fs::create_dir_all(&root)?;
        if let Ok(entries) = fs::read_dir(&root) {
            for e in entries.flatten() {
                let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 86_400);
                if old {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        // Sessions made in the same millisecond by one process get different folders.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = root.join(format!("{stamp}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(Session { dir, keep: false })
    }

    pub fn at(dir: &Path) -> Session {
        Session { dir: dir.to_path_buf(), keep: true }
    }

    pub fn table_path(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{index}.rows"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

pub struct TableWriter {
    file: BufWriter<File>,
    offset: u64,
    pub offsets: [Vec<u64>; 4],
    compress: bool,
}

/// The high bit of a row's length marks a compressed row.
const COMPRESSED: u32 = 0x8000_0000;

impl TableWriter {
    pub fn create(path: &Path, compress: bool) -> std::io::Result<TableWriter> {
        Ok(TableWriter { file: BufWriter::with_capacity(1 << 20, File::create(path)?), offset: 0, offsets: Default::default(), compress })
    }

    pub fn write(&mut self, row: &StoredRow) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(64);
        buf.push(row.kind.tag());
        encode_values(&row.source, &mut buf);
        encode_values(&row.target, &mut buf);
        buf.extend_from_slice(&(row.differs.len() as u32).to_le_bytes());
        buf.extend(row.differs.iter().map(|&d| d as u8));
        let mut len = buf.len() as u32;
        if self.compress && buf.len() > 64 {
            let packed = lz::compress(&buf);
            if packed.len() < buf.len() {
                buf = packed;
                len = buf.len() as u32 | COMPRESSED;
            }
        }
        self.file.write_all(&len.to_le_bytes())?;
        self.file.write_all(&buf)?;
        self.offsets[row.kind as usize].push(self.offset);
        self.offset += 4 + buf.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<[Vec<u64>; 4]> {
        self.file.flush()?;
        Ok(self.offsets)
    }
}

/// Reads rows `start..start+count` of one kind.
pub fn read_page(path: &Path, offsets: &[u64], start: usize, count: usize) -> std::io::Result<Vec<StoredRow>> {
    let mut f = File::open(path)?;
    let mut out = Vec::new();
    for &off in offsets.iter().skip(start).take(count) {
        f.seek(SeekFrom::Start(off))?;
        let mut len = [0u8; 4];
        f.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len);
        let mut buf = vec![0u8; (len & !COMPRESSED) as usize];
        f.read_exact(&mut buf)?;
        if len & COMPRESSED != 0 {
            buf = lz::decompress(&buf).ok_or_else(|| std::io::Error::other("corrupt result file"))?;
        }
        out.push(decode_row(&buf).ok_or_else(|| std::io::Error::other("corrupt result file"))?);
    }
    Ok(out)
}

/// Reads every row of one kind, in order.
pub fn read_all(path: &Path, offsets: &[u64]) -> std::io::Result<Vec<StoredRow>> {
    read_page(path, offsets, 0, offsets.len())
}

fn encode_values(values: &[Value], out: &mut Vec<u8>) {
    out.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for v in values {
        encode(v, out);
    }
}

fn encode(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Null => out.push(0),
        Value::Bool(b) => out.extend([1, *b as u8]),
        Value::Int(i) => {
            out.push(2);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Decimal(d, s) => {
            out.push(3);
            out.extend_from_slice(&d.to_le_bytes());
            out.push(*s);
        }
        Value::Float(f) => {
            out.push(4);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Str(s) => {
            out.push(5);
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            out.push(6);
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        Value::Guid(g) => {
            out.push(7);
            out.extend_from_slice(g);
        }
        Value::Date(d) => {
            out.push(8);
            out.extend_from_slice(&d.to_le_bytes());
        }
        Value::Time(t) => {
            out.push(9);
            out.extend_from_slice(&t.to_le_bytes());
        }
        Value::DateTime(n) => {
            out.push(10);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Value::DateTimeOffset(n, o) => {
            out.push(11);
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&o.to_le_bytes());
        }
        Value::Variant(v) => {
            out.push(12);
            encode_str(&v.base_type, out);
            out.extend([v.precision, v.scale]);
            out.extend_from_slice(&v.max_length.to_le_bytes());
            encode_str(v.collation.as_deref().unwrap_or_default(), out);
            encode_str(&v.text, out);
        }
        Value::Clr(b, text) => {
            out.push(13);
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
            match text {
                Some(t) => {
                    out.push(1);
                    encode_str(t, out);
                }
                None => out.push(0),
            }
        }
        Value::Large(n, hash) => {
            out.push(14);
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&(hash.len() as u32).to_le_bytes());
            out.extend_from_slice(hash);
        }
    }
}

fn encode_str(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn i128(&mut self) -> Option<i128> {
        Some(i128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }

    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        Some(self.take(n)?.to_vec())
    }

    fn value(&mut self) -> Option<Value> {
        Some(match self.u8()? {
            0 => Value::Null,
            1 => Value::Bool(self.u8()? != 0),
            2 => Value::Int(self.i64()?),
            3 => {
                let d = self.i128()?;
                Value::Decimal(d, self.u8()?)
            }
            4 => Value::Float(f64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            5 => {
                let n = self.u32()? as usize;
                Value::Str(String::from_utf8(self.take(n)?.to_vec()).ok()?)
            }
            6 => {
                let n = self.u32()? as usize;
                Value::Bytes(self.take(n)?.to_vec())
            }
            7 => Value::Guid(self.take(16)?.try_into().ok()?),
            8 => Value::Date(self.i64()?),
            9 => Value::Time(self.i64()?),
            10 => Value::DateTime(self.i128()?),
            11 => {
                let n = self.i128()?;
                Value::DateTimeOffset(n, i32::from_le_bytes(self.take(4)?.try_into().ok()?))
            }
            12 => {
                let base_type = self.str()?;
                let (precision, scale) = (self.u8()?, self.u8()?);
                let max_length = i32::from_le_bytes(self.take(4)?.try_into().ok()?);
                let collation = Some(self.str()?).filter(|c| !c.is_empty());
                Value::Variant(Box::new(Variant { base_type, precision, scale, max_length, collation, text: self.str()? }))
            }
            13 => {
                let b = self.bytes()?;
                let text = if self.u8()? == 1 { Some(self.str()?) } else { None };
                Value::Clr(b, text)
            }
            14 => {
                let n = self.i64()?;
                Value::Large(n, self.bytes()?)
            }
            _ => return None,
        })
    }

    fn values(&mut self) -> Option<Vec<Value>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| self.value()).collect()
    }
}

fn decode_row(b: &[u8]) -> Option<StoredRow> {
    let mut c = Cursor { b, pos: 0 };
    let kind = RowKind::from_tag(c.u8()?)?;
    let source = c.values()?;
    let target = c.values()?;
    let n = c.u32()? as usize;
    let differs = c.take(n)?.iter().map(|&x| x != 0).collect();
    Some(StoredRow { kind, source, target, differs })
}

/// A small LZ77 codec for compressed rows: the unpacked length, then tokens: `0..=127` = that
/// many + 1 literal bytes follow; `128..=255` = a match of (token - 128 + 4) bytes at the
/// 16-bit offset that follows.
mod lz {
    pub fn compress(input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() / 2 + 8);
        out.extend_from_slice(&(input.len() as u32).to_le_bytes());
        let mut table = vec![0usize; 1 << 12];
        let (mut i, mut lit) = (0, 0);
        let flush = |out: &mut Vec<u8>, from: usize, to: usize| {
            for chunk in input[from..to].chunks(128) {
                out.push((chunk.len() - 1) as u8);
                out.extend_from_slice(chunk);
            }
        };
        while i + 4 <= input.len() {
            let word = u32::from_le_bytes(input[i..i + 4].try_into().unwrap());
            let h = (word.wrapping_mul(2_654_435_761) >> 20) as usize;
            let cand = table[h];
            table[h] = i + 1;
            if cand > 0 && i - (cand - 1) <= 0xffff && input[cand - 1..cand + 3] == input[i..i + 4] {
                let c = cand - 1;
                let mut len = 4;
                while len < 131 && i + len < input.len() && input[c + len] == input[i + len] {
                    len += 1;
                }
                flush(&mut out, lit, i);
                out.push((128 + len - 4) as u8);
                out.extend_from_slice(&((i - c) as u16).to_le_bytes());
                i += len;
                lit = i;
                continue;
            }
            i += 1;
        }
        flush(&mut out, lit, input.len());
        out
    }

    pub fn decompress(input: &[u8]) -> Option<Vec<u8>> {
        let n = u32::from_le_bytes(input.get(..4)?.try_into().ok()?) as usize;
        let mut out = Vec::with_capacity(n);
        let mut i = 4;
        while i < input.len() {
            let t = input[i] as usize;
            i += 1;
            if t < 128 {
                out.extend_from_slice(input.get(i..i + t + 1)?);
                i += t + 1;
            } else {
                let off = u16::from_le_bytes(input.get(i..i + 2)?.try_into().ok()?) as usize;
                i += 2;
                let start = out.len().checked_sub(off)?;
                for k in 0..t - 128 + 4 {
                    out.push(out[start + k]);
                }
            }
        }
        (out.len() == n).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_round_trip() {
        let data: Vec<u8> = (0..5000u32).flat_map(|i| (i % 37).to_le_bytes()).chain(b"abcabcabcabcabcabc".iter().copied()).collect();
        let packed = lz::compress(&data);
        assert!(packed.len() < data.len() / 2);
        assert_eq!(lz::decompress(&packed).unwrap(), data);
        assert_eq!(lz::decompress(&lz::compress(b"xy")).unwrap(), b"xy");
    }

    #[test]
    fn round_trip_and_pages() {
        for compress in [false, true] {
            round_trip(compress);
        }
    }

    fn round_trip(compress: bool) {
        let session = Session::create().unwrap();
        let path = session.table_path(0);
        let mut w = TableWriter::create(&path, compress).unwrap();
        let rows: Vec<StoredRow> = (0..10)
            .map(|i| StoredRow {
                kind: if i % 2 == 0 { RowKind::Different } else { RowKind::OnlyIn1 },
                source: vec![Value::Int(i), Value::Str(format!("ü{i}").repeat(30)), Value::Decimal(-12345, 2), Value::Guid([i as u8; 16])],
                target: if i % 2 == 0 {
                    vec![
                        Value::Clr(vec![1], Some("/1/".into())),
                        Value::Large(5, vec![9; 32]),
                        Value::DateTimeOffset(5, -420),
                        Value::Variant(Box::new(Variant { base_type: "int".into(), precision: 10, scale: 0, max_length: 4, collation: None, text: "7".into() })),
                    ]
                } else {
                    vec![]
                },
                differs: vec![false, true, true, true],
            })
            .collect();
        for r in &rows {
            w.write(r).unwrap();
        }
        let offsets = w.finish().unwrap();
        assert_eq!(offsets[RowKind::Different as usize].len(), 5);
        let page = read_page(&path, &offsets[RowKind::OnlyIn1 as usize], 1, 2).unwrap();
        assert_eq!(page, vec![rows[3].clone(), rows[5].clone()]);
        let dir = session.dir.clone();
        drop(session);
        assert!(!dir.exists(), "the session directory is removed");
    }
}
