//! Native SQL Server backup files: Microsoft Tape Format descriptor blocks and their streams,
//! read through the compressed-backup container when the file is compressed. Only what a
//! reader of the data needs is decoded: the backup sets, their configuration (database,
//! server, type, LSNs, binding GUIDs), the stripe family, and where the data extents are.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::xpress;

/// Random-access bytes: a backup file, its decompressed form, or a stream of a SQL Backup file.
pub trait Medium: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize>;
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Reads `buf.len()` bytes or fewer at the end.
pub fn read_full(m: &dyn Medium, offset: u64, buf: &mut [u8]) -> usize {
    let mut done = 0;
    while done < buf.len() {
        match m.read_at(offset + done as u64, &mut buf[done..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => done += n,
        }
    }
    done
}

pub struct RawFile {
    file: Mutex<File>,
    len: u64,
}

impl RawFile {
    pub fn open(path: &Path) -> std::io::Result<RawFile> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(RawFile { file: Mutex::new(file), len })
    }
}

impl Medium for RawFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(offset))?;
        f.read(buf)
    }

    fn len(&self) -> u64 {
        self.len
    }
}

/// A compressed backup ("MSSQLBAK" header): compressed blocks and stored 4 KB blocks that
/// together hold the tape image, in order.
pub struct Compressed {
    inner: Arc<dyn Medium>,
    /// (logical offset, logical length, raw offset, header length; 0 = stored).
    segments: Vec<(u64, u64, u64, u32)>,
    len: u64,
    cache: Mutex<HashMap<usize, Arc<Vec<u8>>>>,
}

const STORED_BLOCK: u64 = 4096;

impl Compressed {
    pub fn is_compressed(m: &dyn Medium) -> bool {
        let mut sig = [0u8; 8];
        read_full(m, 0, &mut sig) == 8 && &sig == b"MSSQLBAK"
    }

    pub fn open(inner: Arc<dyn Medium>) -> Result<Compressed, String> {
        let total = inner.len();
        let mut segments = Vec::new();
        let (mut raw, mut logical) = (16u64, 0u64);
        // The first block starts after the signature and zero padding.
        let mut buf = vec![0u8; 4096];
        let n = read_full(&*inner, raw, &mut buf);
        match buf[..n].iter().position(|&b| b != 0) {
            Some(p) => raw += p as u64,
            None => return Err("the compressed backup holds no data".into()),
        }
        while raw < total {
            let mut h = [0u8; 28];
            let got = read_full(&*inner, raw, &mut h);
            if got < 8 {
                break;
            }
            match h[0] {
                // 0x21: SQL Server 2022 blocks (28-byte header); 0x81: earlier ones (8 bytes).
                0x21 | 0x81 if h[4..8] == [0xff; 4] || h[0] == 0x81 => {
                    let header = if h[0] == 0x21 { 28 } else { 8 };
                    let ulen = (h[1] as u64) << 9;
                    let clen = u16::from_le_bytes([h[2], h[3]]) as u64;
                    if ulen == 0 {
                        break;
                    }
                    segments.push((logical, ulen, raw, header));
                    logical += ulen;
                    raw += header as u64 + clen;
                }
                // A filler block.
                0x80 => raw += 4 + u16::from_le_bytes([h[2], h[3]]) as u64,
                _ if h[..4].iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) && h[..4] != [0; 4] => {
                    segments.push((logical, STORED_BLOCK, raw, 0));
                    logical += STORED_BLOCK;
                    raw += STORED_BLOCK;
                }
                _ => {
                    // Padding to the next 4 KB boundary.
                    if raw % STORED_BLOCK == 0 {
                        break;
                    }
                    raw = (raw / STORED_BLOCK + 1) * STORED_BLOCK;
                }
            }
        }
        Ok(Compressed { inner, segments, len: logical, cache: Mutex::new(HashMap::new()) })
    }

    fn block(&self, i: usize) -> std::io::Result<Arc<Vec<u8>>> {
        if let Some(b) = self.cache.lock().unwrap().get(&i) {
            return Ok(b.clone());
        }
        let (_, ulen, raw, header) = self.segments[i];
        let data = if header == 0 {
            let mut b = vec![0u8; ulen as usize];
            read_full(&*self.inner, raw, &mut b);
            b
        } else {
            let mut h = [0u8; 4];
            read_full(&*self.inner, raw, &mut h);
            let clen = u16::from_le_bytes([h[2], h[3]]) as usize;
            // The bit stream may read a little past the stated length.
            let mut src = vec![0u8; clen + 8];
            read_full(&*self.inner, raw + header as u64, &mut src);
            xpress::decompress(&src, ulen as usize).map_err(std::io::Error::other)?
        };
        let data = Arc::new(data);
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(i, data.clone());
        Ok(data)
    }
}

impl Medium for Compressed {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let i = match self.segments.binary_search_by(|s| if s.0 > offset { std::cmp::Ordering::Greater } else if s.0 + s.1 <= offset { std::cmp::Ordering::Less } else { std::cmp::Ordering::Equal }) {
            Ok(i) => i,
            Err(_) => return Ok(0),
        };
        let block = self.block(i)?;
        let at = (offset - self.segments[i].0) as usize;
        let n = buf.len().min(block.len() - at);
        buf[..n].copy_from_slice(&block[at..at + n]);
        Ok(n)
    }

    fn len(&self) -> u64 {
        self.len
    }
}

/// A log sequence number: virtual log file, block, slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsn(pub u32, pub u32, pub u16);

impl Lsn {
    fn at(b: &[u8], o: usize) -> Lsn {
        Lsn(u32_at(b, o), u32_at(b, o + 4), u16_at(b, o + 8))
    }
}

impl std::fmt::Display for Lsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:08}:{:08}:{:04}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupType {
    Full,
    Differential,
    Log,
    File,
    FileDifferential,
    Other(u8),
}

impl BackupType {
    fn of(b: u8) -> BackupType {
        match b {
            b'D' => BackupType::Full,
            b'I' => BackupType::Differential,
            b'L' => BackupType::Log,
            b'F' => BackupType::File,
            b'G' => BackupType::FileDifferential,
            o => BackupType::Other(o),
        }
    }

    /// The number SQL Compare projects store (`BackupType` of a backup set).
    pub fn number(self) -> i32 {
        match self {
            BackupType::Full => 0,
            BackupType::Differential => 1,
            BackupType::Log => 2,
            _ => 3,
        }
    }
}

/// One backup set found in one file (a stripe of a striped set is one of these per file).
#[derive(Clone, Debug)]
pub struct SetPart {
    pub file: PathBuf,
    /// 1-based position of the set in the file.
    pub position: u32,
    pub name: String,
    pub database: String,
    pub server: String,
    pub backup_type: BackupType,
    pub first_lsn: Lsn,
    pub last_lsn: Lsn,
    pub checkpoint_lsn: Lsn,
    pub differential_base_lsn: Lsn,
    /// The database's binding GUID: the same for all backups of one database.
    pub binding_id: [u8; 16],
    /// The backup set's own GUID (from the set descriptor).
    pub set_guid: [u8; 16],
    /// For a differential: `set_guid` of its full backup.
    pub differential_base_guid: [u8; 16],
    pub start: Option<chrono::NaiveDateTime>,
    pub finish: Option<chrono::NaiveDateTime>,
    pub family: [u8; 16],
    pub devices: u16,
    pub sequence: u32,
    /// (offset, length) of the data streams: whole 64 KB extents.
    pub extents: Vec<(u64, u64)>,
    pub size: u64,
}

pub fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

pub fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn align4(n: u64) -> u64 {
    n.div_ceil(4) * 4
}

fn mtf_date(b: &[u8]) -> Option<chrono::NaiveDateTime> {
    let year = ((b[0] as i32) << 6) | ((b[1] as i32 & 0xfc) >> 2);
    let month = ((b[1] as u32 & 3) << 2) | ((b[2] as u32 & 0xc0) >> 6);
    let day = (b[2] as u32 & 0x3e) >> 1;
    let hour = ((b[2] as u32 & 1) << 4) | ((b[3] as u32 & 0xf0) >> 4);
    let minute = ((b[3] as u32 & 0xf) << 2) | ((b[4] as u32 & 0xc0) >> 6);
    let second = b[4] as u32 & 0x3f;
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)
}

fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units).trim_end_matches('\0').to_string()
}

/// An MTF stream header.
struct Stream {
    id: [u8; 4],
    length: u64,
}

fn stream_at(m: &dyn Medium, off: u64) -> Option<Stream> {
    let mut h = [0u8; 22];
    if read_full(m, off, &mut h) < 22 {
        return None;
    }
    Some(Stream { id: h[..4].try_into().unwrap(), length: u64_at(&h, 8) })
}

/// Reads the backup sets of one file (native format, compressed or not).
pub fn read_sets(m: &dyn Medium, file: &Path) -> Result<Vec<SetPart>, String> {
    let mut head = [0u8; 4];
    read_full(m, 0, &mut head);
    if &head != b"TAPE" {
        return Err(format!("The file '{}' is not a valid SQL Server backup file.", file.display()));
    }
    let mut sets: Vec<SetPart> = Vec::new();
    let mut current: Option<SetPart> = None;
    let mut filemark = 4096u64;
    let (mut family, mut devices, mut sequence) = ([0u8; 16], 1u16, 1u32);
    let mut position = 0u32;
    let mut off = 0u64;
    let len = m.len();
    let mut block = vec![0u8; 256];
    while off + 52 <= len {
        if read_full(m, off, &mut block) < 52 {
            break;
        }
        let kind: [u8; 4] = block[..4].try_into().unwrap();
        let first_event = u16_at(&block, 8) as u64;
        if &kind == b"SFMB" {
            off += filemark;
            continue;
        }
        if kind == [0; 4] {
            break;
        }
        match &kind {
            b"TAPE" => {
                filemark = u16_at(&block, 64).max(1) as u64 * 512;
            }
            b"SSET" => {
                position += 1;
                let name = {
                    let (size, at) = (u16_at(&block, 64) as usize, u16_at(&block, 66) as u64);
                    let mut b = vec![0u8; size];
                    read_full(m, off + at, &mut b);
                    utf16(&b)
                };
                current = Some(SetPart {
                    file: file.to_path_buf(),
                    position,
                    name,
                    database: String::new(),
                    server: String::new(),
                    backup_type: BackupType::Other(0),
                    first_lsn: Lsn::default(),
                    last_lsn: Lsn::default(),
                    checkpoint_lsn: Lsn::default(),
                    differential_base_lsn: Lsn::default(),
                    binding_id: [0; 16],
                    set_guid: block[100..116].try_into().unwrap(),
                    differential_base_guid: [0; 16],
                    start: None,
                    finish: None,
                    family,
                    devices,
                    sequence,
                    extents: Vec::new(),
                    size: 0,
                });
            }
            _ => {}
        }
        // Walk the block's streams up to the pad that ends it.
        let mut s_off = off + first_event;
        let mut steps = 0;
        while let Some(s) = stream_at(m, s_off) {
            let data = s_off + 22;
            match (&kind, &s.id) {
                (b"TAPE", b"RAID") => {
                    let mut r = [0u8; 32];
                    read_full(m, data, &mut r);
                    if u32_at(&r, 16) == 0xf000_00ad {
                        family = r[..16].try_into().unwrap();
                        devices = u16_at(&r, 20).max(1);
                        sequence = u32_at(&r, 24).max(1);
                    }
                }
                (b"MSCI" | b"MSLS", b"MQCI") => {
                    if let Some(set) = current.as_mut() {
                        let mut c = vec![0u8; s.length.min(1 << 20) as usize];
                        read_full(m, data, &mut c);
                        if c.len() >= 220 {
                            config(set, &c);
                        }
                    }
                }
                (b"MSDA", b"MQDA") => {
                    if let Some(set) = current.as_mut()
                        && s.length > 2
                    {
                        set.extents.push((data + 2, s.length - 2));
                    }
                }
                _ => {}
            }
            s_off = align4(data + s.length);
            steps += 1;
            if &s.id == b"SPAD" || steps > 100_000 || s_off >= len {
                break;
            }
        }
        if &kind == b"MSLS"
            && let Some(set) = current.take()
        {
            sets.push(set);
        }
        if s_off <= off {
            break;
        }
        off = s_off;
    }
    if let Some(set) = current.take()
        && !set.extents.is_empty()
    {
        sets.push(set);
    }
    Ok(sets)
}

/// Fills a set from a SQL configuration structure (the MQCI stream).
fn config(set: &mut SetPart, c: &[u8]) {
    set.first_lsn = Lsn::at(c, 32);
    set.last_lsn = Lsn::at(c, 42);
    set.checkpoint_lsn = Lsn::at(c, 52);
    set.differential_base_lsn = Lsn::at(c, 62);
    set.start = mtf_date(&c[77..82]);
    set.finish = mtf_date(&c[82..87]);
    set.backup_type = BackupType::of(c[87]);
    set.size = u64_at(c, 96);
    let text = |at: usize| {
        let (size, offset) = (u16_at(c, at) as usize, u16_at(c, at + 2) as usize);
        c.get(offset..offset + size).map(utf16).unwrap_or_default()
    };
    set.database = text(104);
    set.server = text(108);
    set.binding_id = c[124..140].try_into().unwrap();
    set.differential_base_guid = c[200..216].try_into().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_lsns() {
        // 2026-11-07 06:04:05
        let y = 2026u32;
        let b = [(y >> 6) as u8, (((y & 0x3f) << 2) | (11 >> 2)) as u8, (((11 & 3) << 6) | (7 << 1)) as u8, (6 << 4 | 4 >> 2) as u8, ((4 & 3) << 6 | 5) as u8];
        assert_eq!(mtf_date(&b).unwrap().to_string(), "2026-11-07 06:04:05");
        assert_eq!(Lsn(37, 160, 1).to_string(), "00000037:00000160:0001");
    }
}
