//! The LZ77 + Huffman coding SQL Server uses for compressed backups (the "Xpress Huffman"
//! format of MS-XCA): a 256-byte table of 512 four-bit code lengths, then a bit stream read in
//! little-endian 16-bit words, most significant bit first. Symbols below 256 are literals; the
//! others are matches whose high nibble is the number of distance bits and low nibble the
//! length (15: the length continues in the byte stream).

/// Decodes one block into `out_len` bytes.
pub fn decompress(src: &[u8], out_len: usize) -> Result<Vec<u8>, String> {
    if src.len() < 256 {
        return Err("compressed block too short".into());
    }
    let table = Table::new(&src[..256])?;
    let mut r = Bits { src, pos: 256, bits: 0, avail: 0 };
    r.bits = (r.word() << 16) | r.word();
    r.avail = 32;
    let mut out: Vec<u8> = Vec::with_capacity(out_len);
    while out.len() < out_len {
        let entry = table.lookup[(r.bits >> (32 - Table::BITS)) as usize];
        let len = (entry & 15) as u32;
        if len == 0 {
            return Err(format!("invalid code in compressed block at byte {}", out.len()));
        }
        let sym = (entry >> 4) as usize;
        r.consume(len);
        if sym < 256 {
            out.push(sym as u8);
            continue;
        }
        let sym = sym - 256;
        let dist_bits = (sym >> 4) as u32;
        let mut length = sym & 15;
        if length == 15 {
            let b = r.byte()? as usize;
            length = if b == 255 {
                let w = r.byte16()? as usize;
                let total = if w == 0 { r.byte32()? as usize } else { w };
                total.checked_sub(15).ok_or("invalid match length")?
            } else {
                b
            } + 15;
        }
        length += 3;
        let distance = if dist_bits == 0 { 1 } else { ((r.bits >> (32 - dist_bits)) as usize) | (1 << dist_bits) };
        r.consume(dist_bits);
        if distance > out.len() {
            return Err(format!("match before the start of a compressed block at byte {}", out.len()));
        }
        let start = out.len() - distance;
        for i in 0..length.min(out_len - out.len()) {
            let b = out[start + i];
            out.push(b);
        }
    }
    Ok(out)
}

struct Table {
    /// Indexed by the next 15 bits: symbol << 4 | code length.
    lookup: Vec<u16>,
}

impl Table {
    const BITS: u32 = 15;

    fn new(lengths: &[u8]) -> Result<Table, String> {
        let len = |s: usize| if s.is_multiple_of(2) { lengths[s / 2] & 15 } else { lengths[s / 2] >> 4 };
        let mut lookup = vec![0u16; 1 << Self::BITS];
        let mut next = 0usize;
        // Canonical codes: shorter codes first, then by symbol.
        for bits in 1..=15u8 {
            for sym in (0..512).filter(|&s| len(s) == bits) {
                let span = 1usize << (Self::BITS - bits as u32);
                if next + span > lookup.len() {
                    return Err("invalid Huffman table in compressed block".into());
                }
                lookup[next..next + span].fill(((sym as u16) << 4) | bits as u16);
                next += span;
            }
        }
        Ok(Table { lookup })
    }
}

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    bits: u32,
    avail: u32,
}

impl Bits<'_> {
    fn word(&mut self) -> u32 {
        let lo = self.src.get(self.pos).copied().unwrap_or(0) as u32;
        let hi = self.src.get(self.pos + 1).copied().unwrap_or(0) as u32;
        self.pos += 2;
        lo | (hi << 8)
    }

    fn consume(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        self.bits = if n >= 32 { 0 } else { self.bits << n };
        self.avail -= n;
        if self.avail < 16 {
            self.bits |= self.word() << (16 - self.avail);
            self.avail += 16;
        }
    }

    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.src.get(self.pos).ok_or("compressed block ends early")?;
        self.pos += 1;
        Ok(b)
    }

    fn byte16(&mut self) -> Result<u16, String> {
        Ok(self.byte()? as u16 | ((self.byte()? as u16) << 8))
    }

    fn byte32(&mut self) -> Result<u32, String> {
        Ok(self.byte16()? as u32 | ((self.byte16()? as u32) << 16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of the 512 symbols has a 9-bit code equal to its value.
    #[test]
    fn literals_with_a_flat_table() {
        let mut src = vec![0x99u8; 256];
        let mut bits: Vec<bool> = Vec::new();
        for c in *b"AB" {
            for i in (0..9).rev() {
                bits.push((c as u16 >> i) & 1 == 1);
            }
        }
        bits.resize(64, false);
        for chunk in bits.chunks(16) {
            let w = chunk.iter().fold(0u16, |w, &b| (w << 1) | b as u16);
            src.extend_from_slice(&w.to_le_bytes());
        }
        assert_eq!(decompress(&src, 2).unwrap(), b"AB");
    }
}
