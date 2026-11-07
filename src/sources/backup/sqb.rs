//! SQL Backup (.sqb) files: a header (possibly encrypted with a password-derived AES key),
//! then the backup's tape image cut in extents, each compressed and possibly encrypted. A file
//! written by several threads holds one image per thread ("device"), which are the stripes of
//! the backup. See specs/gaps/backup.md, section 6.
//!
//! Written from the format description alone: no real .sqb file has been read with it.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::BackupError;
use super::aes::Aes;
use super::mtf::{Medium, RawFile, read_full, u32_at};

/// A SQL Backup file has `SSBE` or `SSBF` at offset 508 of its first kilobyte.
pub fn is_sqb(path: &Path) -> bool {
    let mut head = [0u8; 1024];
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    matches!(&head[508..512], b"SSBE" | b"SSBF")
}

/// The streams of the file, one per device; `passwords` are tried in turn, then no password.
pub fn open(path: &Path, passwords: &[String]) -> Result<Vec<Arc<dyn Medium>>, BackupError> {
    let file = Arc::new(RawFile::open(path).map_err(|e| BackupError::Io(format!("{}: {e}", path.display())))?);
    let mut raw = [0u8; 1024];
    read_full(&*file, 0, &mut raw);
    let mut header = None;
    for p in passwords.iter().map(String::as_str).chain([""]) {
        if let Some(h) = Header::read(&raw, p)? {
            header = Some(h);
            break;
        }
    }
    let header = Arc::new(header.ok_or_else(|| BackupError::Password("Cannot decrypt SQB file.".into()))?);
    let extents = map_extents(&*file, &header).map_err(BackupError::Reader)?;
    // The stated size covers all devices; each device's image is no longer (the reference
    // reader divides it evenly, which need not hold).
    let len = header.size;
    Ok((0..header.devices)
        .map(|d| {
            let mine: Vec<Extent> = extents.iter().filter(|e| e.device == d).copied().collect();
            Arc::new(Stream { file: file.clone(), header: header.clone(), extents: mine, len, state: Mutex::new(State::default()) }) as Arc<dyn Medium>
        })
        .collect())
}

/// How the header and the extents are encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    None,
    /// SQL Backup 3: a 256-bit key from a hash of the password.
    V3,
    /// SQL Backup 4: 128- or 256-bit keys from hashes of the password.
    V4(usize),
    /// Later: 128- or 256-bit keys by PBKDF2 over the password and the header's salt.
    V5(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compression {
    None,
    /// Level 1: a BriefLZ-style LZ77 with interleaved control bits.
    Lz,
    /// Levels 2 and 3.
    Zlib,
    /// Level 4.
    Lzma,
}

struct Header {
    aes: Option<Aes>,
    /// The counter of every extent (SQL Backup 3 and 4); later versions derive one per extent.
    counter: Option<[u8; 16]>,
    compression: Compression,
    /// The size of the tape image, all devices together.
    size: u64,
    length: u64,
    /// The file has sections and padding blocks (a product level of 6 or later).
    sections: bool,
    /// Extents carry the number of their device.
    multi: bool,
    devices: u8,
}

/// The constant SQL Backup 3 pads its key and counter with.
const PAD: &[u8; 32] = b"4FE7192ABB5C48838EE4F956AD3688C1";

impl Header {
    /// The header decrypted with `password`, or None when it does not decrypt.
    fn read(raw: &[u8; 1024], password: &str) -> Result<Option<Header>, BackupError> {
        let (major, length, modes): (u8, usize, &[Mode]) = if pascal(raw, 507).is_some_and(|s| s == b"SSBE" || s == b"SSBF") && pascal(raw, 512) == Some(b"SQB4") && pascal(raw, 1019) == Some(b"SQB4") {
            (4, 1024, &[Mode::None, Mode::V4(16), Mode::V4(32), Mode::V5(16), Mode::V5(32)])
        } else if pascal(raw, 507) == Some(b"SSBE") {
            (2, 512, &[Mode::None, Mode::V3])
        } else {
            return Err(BackupError::Reader("This backup file was created with an unknown version of SQL Backup.".into()));
        };
        // Characters are taken as single bytes.
        let pw: Vec<u8> = password.encode_utf16().map(|c| c as u8).collect();
        for &mode in modes {
            let mut b = raw[..length].to_vec();
            let (aes, counter) = match mode {
                Mode::None => (None, None),
                Mode::V3 => {
                    let (key, counter) = v3_keys(&pw);
                    let aes = Aes::new(&key);
                    aes.ctr(&[0; 16], &mut b[..506]);
                    (Some(aes), Some(counter))
                }
                Mode::V4(n) => {
                    let (key, counter) = v4_keys(&pw, n);
                    let aes = Aes::new(&key);
                    aes.ctr(&[0; 16], &mut b[..506]);
                    aes.ctr(&[0; 16], &mut b[518..776]);
                    (Some(aes), Some(counter))
                }
                Mode::V5(n) => {
                    let aes = Aes::new(&super::sha256::pbkdf2(&pw, &raw[983..999], 1000, n));
                    let mut first = [0u8; 16];
                    first[0] = 0xff;
                    let mut second = [1u8; 16];
                    second[0] = 0xff;
                    aes.ctr(&first, &mut b[..506]);
                    aes.ctr(&second, &mut b[518..776]);
                    (Some(aes), None)
                }
            };
            if b[5] != major || pascal(&b, 0).is_none() || pascal(&b, 0) != pascal(&b, 507) {
                continue;
            }
            let compression = match b[505] {
                0 => Compression::None,
                1 => Compression::Lz,
                2 | 3 => Compression::Zlib,
                4 => Compression::Lzma,
                n => return Err(BackupError::Reader(format!("Unknown compression level: {n}"))),
            };
            let (sections, multi, devices) = if major == 4 { (b[981] >= 60, b[1016] != 0, b[1017].max(1)) } else { (false, false, 1) };
            let size = u64::from_le_bytes(b[9..17].try_into().unwrap());
            let devices = if multi { devices } else { 1 };
            return Ok(Some(Header { aes, counter, compression, size, length: length as u64, sections, multi, devices }));
        }
        Ok(None)
    }

    /// The counter an extent is decrypted from.
    fn counter(&self, e: &Extent) -> [u8; 16] {
        if let Some(c) = self.counter {
            return c;
        }
        let mut c = [0u8; 16];
        c[0] = e.device;
        c[5..13].copy_from_slice(&(e.index as u64 + 1).to_be_bytes());
        c
    }
}

/// A string of `buf[at]` bytes after its length byte.
fn pascal(buf: &[u8], at: usize) -> Option<&[u8]> {
    buf.get(at + 1..at + 1 + *buf.get(at)? as usize)
}

/// The ELF hash of `b`.
fn elf(b: &[u8]) -> u32 {
    let mut h = 0u32;
    for &x in b {
        h = (h << 4).wrapping_add(x as u32);
        let high = h & 0xf000_0000;
        if high != 0 {
            h ^= high >> 24;
        }
        h &= !high;
    }
    h
}

fn decimal(v: u32) -> Vec<u8> {
    v.to_string().into_bytes()
}

/// SQL Backup 3: the key is the decimal hash of the password padded with a constant; the
/// counter the decimal hash of that hash, padded the same way.
fn v3_keys(pw: &[u8]) -> ([u8; 32], [u8; 16]) {
    let h = decimal(elf(pw));
    let mut key = [0u8; 32];
    key[..h.len()].copy_from_slice(&h);
    key[h.len()..].copy_from_slice(&PAD[..32 - h.len()]);
    let c = decimal(elf(&h));
    let mut counter = [0u8; 16];
    counter[..c.len()].copy_from_slice(&c);
    counter[c.len()..].copy_from_slice(&PAD[..16 - c.len()]);
    (key, counter)
}

/// Decimal hashes, each of everything so far, until there are `n` bytes.
fn chain(seed: &[u8], n: usize) -> Vec<u8> {
    let mut out = decimal(elf(seed));
    while out.len() < n {
        out.extend(decimal(elf(&out)));
    }
    out
}

/// SQL Backup 4: the key is a chain of hashes of the password; the counter is the password
/// (its first 16 bytes), completed by a chain of hashes of the key.
fn v4_keys(pw: &[u8], n: usize) -> (Vec<u8>, [u8; 16]) {
    let key = chain(pw, 32)[..n].to_vec();
    let fill = chain(&key, 16);
    let mut counter = [0u8; 16];
    let k = pw.len().min(16);
    counter[..k].copy_from_slice(&pw[..k]);
    counter[k..].copy_from_slice(&fill[..16 - k]);
    (key, counter)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    device: u8,
    /// The extent's number among its device's extents.
    index: usize,
    /// Where its (compressed, encrypted) data starts and how long it is.
    data: u64,
    size: u32,
    /// The decompressed length, stated with level 1 compression.
    stated: u32,
}

/// Reads the extent records of the whole file.
fn map_extents(file: &dyn Medium, h: &Header) -> Result<Vec<Extent>, String> {
    let mut pos = h.length;
    if h.sections {
        // Sections, each with a 20-byte header (type, length including the header); the
        // extents are in the section of type 1.
        loop {
            let mut s = [0u8; 20];
            if read_full(file, pos, &mut s) < 20 {
                return Err("Failed to read 20 byte block header".into());
            }
            if u32_at(&s, 0) == 1 {
                pos += 20;
                break;
            }
            let len = u64::from_le_bytes(s[4..12].try_into().unwrap());
            if len == 0 {
                return Err("Invalid section in SQL Backup file".into());
            }
            pos += len;
        }
    }
    let lz = h.compression == Compression::Lz;
    let multi = h.multi && h.devices > 1;
    let mut counts = vec![0usize; h.devices as usize];
    let mut finished = vec![false; h.devices as usize];
    let mut out = Vec::new();
    loop {
        let mut b = [0u8; 9];
        let head = if multi { 5 } else { 4 };
        if read_full(file, pos, &mut b[..head]) < head {
            break;
        }
        let (device, n) = if multi { (b[0], i32::from_le_bytes(b[1..5].try_into().unwrap())) } else { (0, i32::from_le_bytes(b[..4].try_into().unwrap())) };
        pos += head as u64;
        if h.sections && (device == 0xff || n == -1) {
            // Padding: its length follows (or, with devices, is the record's length).
            let size = if multi {
                n
            } else {
                if read_full(file, pos, &mut b[..4]) < 4 {
                    return Err("Failed to read size of vdi padding block (end of stream)".into());
                }
                pos += 4;
                i32::from_le_bytes(b[..4].try_into().unwrap())
            };
            pos = pos.checked_add_signed(size as i64).ok_or("Invalid padding block in SQL Backup file")?;
            continue;
        }
        let d = device as usize;
        if d >= counts.len() {
            return Err(format!("Extent of device {device} in a file of {} devices", counts.len()));
        }
        if finished[d] {
            return Err(format!("Read from Device {device} which had already finished"));
        }
        if n <= 0 {
            // The end of a device's image; without devices, of everything.
            if !(h.sections && multi) {
                break;
            }
            finished[d] = true;
            if finished.iter().all(|f| *f) {
                break;
            }
            if lz {
                pos += 4;
            }
            continue;
        }
        let stated = if lz {
            if read_full(file, pos, &mut b[..4]) < 4 {
                return Err("SQL Backup file ends early".into());
            }
            pos += 4;
            u32_at(&b, 0)
        } else {
            0
        };
        out.push(Extent { device, index: counts[d], data: pos, size: n as u32, stated });
        counts[d] += 1;
        pos += n as u64;
    }
    Ok(out)
}

/// Extents are decompressed into at most this many bytes.
const MAX_EXTENT: usize = 1 << 20;

/// One device's tape image.
struct Stream {
    file: Arc<RawFile>,
    header: Arc<Header>,
    extents: Vec<Extent>,
    len: u64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Where each extent decoded so far starts in the image, and where the last one ends.
    starts: Vec<u64>,
    cache: HashMap<usize, Arc<Vec<u8>>>,
}

impl Stream {
    fn decode(&self, e: &Extent) -> std::io::Result<Vec<u8>> {
        let mut data = vec![0u8; e.size as usize];
        if read_full(&*self.file, e.data, &mut data) < data.len() {
            return Err(std::io::Error::other(format!("Tried to read {} bytes of compressed data, only got less", e.size)));
        }
        if let Some(aes) = &self.header.aes {
            aes.ctr(&self.header.counter(e), &mut data);
        }
        let out = match self.header.compression {
            Compression::None => Ok(data),
            Compression::Lz if self.header.sections && e.stated == e.size => Ok(data),
            Compression::Lz => unpack(&data, e.stated as usize),
            Compression::Zlib => super::inflate::zlib(&data, MAX_EXTENT),
            Compression::Lzma => super::lzma::decompress(&data, MAX_EXTENT),
        };
        out.map_err(|m| std::io::Error::other(format!("Error trying to decompress SQB file: {m}")))
    }

    /// The extent holding `offset` and its data, decoding the extents before it first if
    /// their lengths are not known yet.
    fn extent_at(&self, offset: u64) -> std::io::Result<Option<(u64, Arc<Vec<u8>>)>> {
        let mut st = self.state.lock().unwrap();
        loop {
            if st.starts.last().is_some_and(|&end| offset < end) {
                let i = st.starts.partition_point(|&s| s <= offset) - 1;
                let start = st.starts[i];
                if let Some(d) = st.cache.get(&i) {
                    return Ok(Some((start, d.clone())));
                }
                let d = Arc::new(self.decode(&self.extents[i])?);
                Self::keep(&mut st, i, d.clone());
                return Ok(Some((start, d)));
            }
            // The next extent whose length is not known yet.
            let i = st.starts.len().saturating_sub(1);
            let Some(e) = self.extents.get(i) else { return Ok(None) };
            let d = Arc::new(self.decode(e)?);
            if st.starts.is_empty() {
                st.starts.push(0);
            }
            let end = st.starts[i] + d.len() as u64;
            st.starts.push(end);
            Self::keep(&mut st, i, d);
        }
    }

    fn keep(st: &mut State, i: usize, d: Arc<Vec<u8>>) {
        if st.cache.len() >= 16 {
            st.cache.clear();
        }
        st.cache.insert(i, d);
    }
}

impl Medium for Stream {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some((start, data)) = self.extent_at(offset)? else { return Ok(0) };
        let at = (offset - start) as usize;
        let n = buf.len().min(data.len() - at);
        buf[..n].copy_from_slice(&data[at..at + n]);
        Ok(n)
    }

    fn len(&self) -> u64 {
        self.len
    }
}

/// Level 1 decompression into `len` bytes: three literal bytes, then items marked by control
/// bits (16-bit little-endian words, most significant bit first, read when needed): 0, a
/// literal byte; 1, a match whose length and the high part of its distance follow as gamma
/// codes in control bits, the low byte of the distance in the byte stream.
fn unpack(src: &[u8], len: usize) -> Result<Vec<u8>, String> {
    let short = || "compressed extent ends early".to_string();
    let mut out: Vec<u8> = Vec::with_capacity(len);
    out.extend_from_slice(src.get(..3.min(len)).ok_or_else(short)?);
    let mut pos = 3;
    let (mut tag, mut left) = (0u32, 0u32);
    let mut bit = |pos: &mut usize| -> Result<u32, String> {
        if left == 0 {
            let w = src.get(*pos..*pos + 2).ok_or_else(short)?;
            tag = u16::from_le_bytes([w[0], w[1]]) as u32;
            *pos += 2;
            left = 16;
        }
        left -= 1;
        let b = (tag >> 15) & 1;
        tag <<= 1;
        Ok(b)
    };
    let gamma = |pos: &mut usize, bit: &mut dyn FnMut(&mut usize) -> Result<u32, String>| -> Result<usize, String> {
        let mut v = 1usize;
        loop {
            v = (v << 1) + bit(pos)? as usize;
            if bit(pos)? == 0 {
                return Ok(v);
            }
        }
    };
    while out.len() < len {
        if bit(&mut pos)? == 0 {
            out.push(*src.get(pos).ok_or_else(short)?);
            pos += 1;
            continue;
        }
        let length = gamma(&mut pos, &mut bit)? + 2;
        let high = gamma(&mut pos, &mut bit)? - 2;
        let distance = (high << 8) + *src.get(pos).ok_or_else(short)? as usize + 1;
        pos += 1;
        if distance > out.len() {
            return Err("invalid match in compressed extent".into());
        }
        let from = out.len() - distance;
        for k in 0..length {
            out.push(out[from + k]);
        }
    }
    // The last match may run past the stated length.
    out.truncate(len);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const EXTENT: usize = 16384;

    /// The payload of every sample (tests/fixtures/sqb/make.py).
    fn payload() -> Vec<u8> {
        let mut out = b"TAPE".to_vec();
        let mut i = 0u64;
        while out.len() < 6 * EXTENT {
            out.extend_from_slice(format!("row {i} of the sample backup, ").as_bytes());
            out.push((i * 7919 % 251) as u8);
            i += 1;
        }
        out.truncate(6 * EXTENT);
        out
    }

    fn sample(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sqb").join(name)
    }

    fn read_all(m: &dyn Medium) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 5000];
        loop {
            let n = read_full(m, out.len() as u64, &mut buf);
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    /// Device `d` of `devices` holds every devices-th extent of the payload.
    fn expected(d: usize, devices: usize) -> Vec<u8> {
        payload().chunks(EXTENT).enumerate().filter(|(g, _)| g % devices == d).flat_map(|(_, c)| c.to_vec()).collect()
    }

    #[test]
    fn samples_decode_to_their_payload() {
        let pw = |p: &str| vec!["wrong".to_string(), p.to_string()];
        for (name, passwords, devices) in [
            ("plain_zlib.sqb", vec![], 1),
            ("v5_aes256_lzma_2devices.sqb", pw("Quill!sqb"), 2),
            ("v5_aes128_lz.sqb", pw("Quill!sqb"), 1),
            ("v4_aes256_stored.sqb", pw("pw"), 1),
            ("v3_aes256_lz.sqb", pw("an older password"), 1),
        ] {
            let f = sample(name);
            assert!(is_sqb(&f), "{name}");
            let streams = open(&f, &passwords).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(streams.len(), devices, "{name}");
            for (d, s) in streams.iter().enumerate() {
                assert!(read_all(&**s) == expected(d, devices), "{name}, device {d}");
                // Reading backwards uses the extents already mapped.
                let mut b = [0u8; 4];
                read_full(&**s, 0, &mut b);
                assert_eq!(&b, &expected(d, devices)[..4], "{name}");
            }
            if !passwords.is_empty() {
                assert_eq!(open(&f, &["wrong".into()]).err(), Some(BackupError::Password("Cannot decrypt SQB file.".into())), "{name}");
            }
        }
        // An unencrypted file opens whatever the passwords.
        assert!(open(&sample("plain_zlib.sqb"), &["any".into()]).is_ok());
    }

    #[test]
    fn keys_from_passwords() {
        // SQL Backup 3: the decimal ELF hash of "abc" is 26499, padded with the constant.
        assert_eq!(elf(b"abc"), 26499);
        let (key, counter) = v3_keys(b"abc");
        assert_eq!(&key[..], b"264994FE7192ABB5C48838EE4F956AD3");
        let c = decimal(elf(b"26499"));
        assert_eq!(&counter[..c.len()], c.as_slice());
        assert_eq!(&counter[c.len()..], &PAD[..16 - c.len()]);
        let (key, counter) = v4_keys(b"abc", 16);
        assert_eq!(key, chain(b"abc", 32)[..16].to_vec());
        assert_eq!(&counter[..3], b"abc");
        assert_eq!(&counter[3..], &chain(&key, 16)[..13]);
    }

    #[test]
    fn level_1_matches() {
        // "abc", a literal "d", then a match of 8 bytes at distance 4. Control bits: 0
        // (literal), 1 (match), gamma(8 - 2 = 0b110) = 1 1 0 0, gamma(0 + 2 = 0b10) = 0 0; the
        // word 0b0111_0000_0000_0000 comes before the literal it governs, the low byte of the
        // distance (4 - 1) last.
        let src = [b'a', b'b', b'c', 0x00, 0x70, b'd', 3];
        assert_eq!(unpack(&src, 12).unwrap(), b"abcdabcdabcd");
        assert!(unpack(&src[..5], 12).is_err());
    }
}
