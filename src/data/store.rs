//! Comparison results on disk: one append-only file of differing rows per table, plus the
//! offsets of each kind of row, so a page of any kind can be read without loading the table.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::value::Value;

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
        let dir = root.join(format!("{stamp}-{}", std::process::id()));
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
}

impl TableWriter {
    pub fn create(path: &Path) -> std::io::Result<TableWriter> {
        Ok(TableWriter { file: BufWriter::with_capacity(1 << 20, File::create(path)?), offset: 0, offsets: Default::default() })
    }

    pub fn write(&mut self, row: &StoredRow) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(64);
        buf.push(row.kind.tag());
        encode_values(&row.source, &mut buf);
        encode_values(&row.target, &mut buf);
        buf.extend_from_slice(&(row.differs.len() as u32).to_le_bytes());
        buf.extend(row.differs.iter().map(|&d| d as u8));
        self.file.write_all(&(buf.len() as u32).to_le_bytes())?;
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
        let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
        f.read_exact(&mut buf)?;
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
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_pages() {
        let session = Session::create().unwrap();
        let path = session.table_path(0);
        let mut w = TableWriter::create(&path).unwrap();
        let rows: Vec<StoredRow> = (0..10)
            .map(|i| StoredRow {
                kind: if i % 2 == 0 { RowKind::Different } else { RowKind::OnlyIn1 },
                source: vec![Value::Int(i), Value::Str(format!("ü{i}")), Value::Decimal(-12345, 2), Value::Guid([i as u8; 16])],
                target: if i % 2 == 0 { vec![Value::Int(i), Value::Null, Value::DateTimeOffset(5, -420), Value::Bytes(vec![1, 2])] } else { vec![] },
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
