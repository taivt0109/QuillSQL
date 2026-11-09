//! Deflate decoding (RFC 1951) of the zlib streams (RFC 1950) the .sqb backup tool compresses with at
//! levels 2 and 3.

/// Decodes a zlib stream: a 2-byte header, deflate data, the Adler-32 of the output (not
/// checked, as the reference reader does not either).
pub fn zlib(src: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if src.len() < 6 {
        return Err("zlib data too short".into());
    }
    if src[1] & 0x20 != 0 {
        return Err("zlib streams with a preset dictionary are not supported".into());
    }
    inflate(&src[2..], limit)
}

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    bit: u32,
}

impl Bits<'_> {
    fn get(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0;
        for i in 0..n {
            let byte = *self.src.get(self.pos).ok_or("deflate data ends early")?;
            v |= (((byte >> self.bit) & 1) as u32) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        Ok(v)
    }
}

/// A canonical Huffman code: the number of codes of each length and the symbols in code order.
struct Code {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Code {
    fn new(lengths: &[u8]) -> Code {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut symbols = Vec::new();
        for len in 1..16 {
            symbols.extend((0..lengths.len()).filter(|&s| lengths[s] as usize == len).map(|s| s as u16));
        }
        Code { counts, symbols }
    }

    fn decode(&self, r: &mut Bits) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= r.get(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("invalid deflate code".into())
    }
}

const LENGTH_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Decodes raw deflate data into at most `limit` bytes.
pub fn inflate(src: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut r = Bits { src, pos: 0, bit: 0 };
    let mut out = Vec::new();
    loop {
        let last = r.get(1)? == 1;
        match r.get(2)? {
            0 => {
                if r.bit != 0 {
                    r.bit = 0;
                    r.pos += 1;
                }
                let head = src.get(r.pos..r.pos + 4).ok_or("deflate data ends early")?;
                let len = u16::from_le_bytes([head[0], head[1]]) as usize;
                if len != !u16::from_le_bytes([head[2], head[3]]) as usize {
                    return Err("invalid stored deflate block".into());
                }
                r.pos += 4;
                out.extend_from_slice(src.get(r.pos..r.pos + len).ok_or("deflate data ends early")?);
                r.pos += len;
            }
            1 => {
                let mut lengths = [8u8; 288];
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                block(&mut r, &mut out, &Code::new(&lengths), &Code::new(&[5; 30]), limit)?;
            }
            2 => {
                let (hlit, hdist, hclen) = (r.get(5)? as usize + 257, r.get(5)? as usize + 1, r.get(4)? as usize + 4);
                let mut cl = [0u8; 19];
                for &i in &[16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15][..hclen] {
                    cl[i] = r.get(3)? as u8;
                }
                let cl = Code::new(&cl);
                let mut lengths = Vec::with_capacity(hlit + hdist);
                while lengths.len() < hlit + hdist {
                    let (value, repeat) = match cl.decode(&mut r)? {
                        s @ 0..=15 => (s as u8, 1),
                        16 => (*lengths.last().ok_or("invalid deflate code lengths")?, 3 + r.get(2)?),
                        17 => (0, 3 + r.get(3)?),
                        _ => (0, 11 + r.get(7)?),
                    };
                    lengths.extend(std::iter::repeat_n(value, repeat as usize));
                }
                if lengths.len() > hlit + hdist {
                    return Err("invalid deflate code lengths".into());
                }
                block(&mut r, &mut out, &Code::new(&lengths[..hlit]), &Code::new(&lengths[hlit..]), limit)?;
            }
            _ => return Err("invalid deflate block type".into()),
        }
        if out.len() > limit {
            return Err("deflate data longer than expected".into());
        }
        if last {
            return Ok(out);
        }
    }
}

fn block(r: &mut Bits, out: &mut Vec<u8>, lit: &Code, dist: &Code, limit: usize) -> Result<(), String> {
    loop {
        let s = lit.decode(r)? as usize;
        if s < 256 {
            out.push(s as u8);
        } else if s == 256 {
            return Ok(());
        } else {
            let i = s - 257;
            let len = *LENGTH_BASE.get(i).ok_or("invalid deflate length")? as usize + r.get(LENGTH_EXTRA[i] as u32)? as usize;
            let d = dist.decode(r)? as usize;
            let distance = *DIST_BASE.get(d).ok_or("invalid deflate distance")? as usize + r.get(DIST_EXTRA[d] as u32)? as usize;
            if distance > out.len() {
                return Err("deflate distance too far back".into());
            }
            let from = out.len() - distance;
            for k in 0..len {
                out.push(out[from + k]);
            }
        }
        if out.len() > limit {
            return Err("deflate data longer than expected".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_and_fixed_blocks() {
        // zlib.compress(b"hello hello hello hello", 9), a fixed-code block.
        let fixed = [0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03, 0x08, 0xb1];
        assert_eq!(zlib(&fixed, 100).unwrap(), b"hello hello hello hello");
        // A stored block.
        let mut stored = vec![0x78, 0x01, 0x01, 5, 0, !5u8, 0xff];
        stored.extend_from_slice(b"abcde");
        stored.extend_from_slice(&0x05c801f0u32.to_be_bytes());
        assert_eq!(zlib(&stored, 100).unwrap(), b"abcde");
        assert!(zlib(&stored[..9], 100).is_err());
        assert!(zlib(&fixed, 10).is_err());
    }
}
