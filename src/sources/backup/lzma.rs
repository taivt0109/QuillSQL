//! LZMA decoding of SQL Backup's level 4 compression: 5 property bytes (lc/lp/pb, dictionary
//! size), then the range-coded stream, which ends with the end marker or when the output is
//! complete.

const PROB_INIT: u16 = 1024;
const MATCH_LEN_MIN: usize = 2;

struct Range<'a> {
    src: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl Range<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.src.get(self.pos).ok_or("LZMA data ends early")?;
        self.pos += 1;
        Ok(b)
    }

    fn normalize(&mut self) -> Result<(), String> {
        if self.range < 1 << 24 {
            self.range <<= 8;
            self.code = (self.code << 8) | self.byte()? as u32;
        }
        Ok(())
    }

    fn bit(&mut self, p: &mut u16) -> Result<u32, String> {
        let bound = (self.range >> 11) * *p as u32;
        let b = if self.code < bound {
            self.range = bound;
            *p += (2048 - *p) >> 5;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *p -= *p >> 5;
            1
        };
        self.normalize()?;
        Ok(b)
    }

    fn direct(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0;
        for _ in 0..n {
            self.range >>= 1;
            let b = if self.code >= self.range {
                self.code -= self.range;
                1
            } else {
                0
            };
            v = (v << 1) | b;
            self.normalize()?;
        }
        Ok(v)
    }

    /// A symbol of `n` bits coded most significant bit first in the tree `p`.
    fn tree(&mut self, p: &mut [u16], n: u32) -> Result<u32, String> {
        let mut m = 1usize;
        for _ in 0..n {
            m = (m << 1) | self.bit(&mut p[m])? as usize;
        }
        Ok(m as u32 - (1 << n))
    }

    /// The same, least significant bit first.
    fn reverse(&mut self, p: &mut [u16], n: u32) -> Result<u32, String> {
        let (mut m, mut v) = (1usize, 0u32);
        for i in 0..n {
            let b = self.bit(&mut p[m])?;
            m = (m << 1) | b as usize;
            v |= b << i;
        }
        Ok(v)
    }
}

struct Lengths {
    choice: u16,
    choice2: u16,
    low: Vec<[u16; 8]>,
    mid: Vec<[u16; 8]>,
    high: [u16; 256],
}

impl Lengths {
    fn new() -> Lengths {
        Lengths { choice: PROB_INIT, choice2: PROB_INIT, low: vec![[PROB_INIT; 8]; 16], mid: vec![[PROB_INIT; 8]; 16], high: [PROB_INIT; 256] }
    }

    fn decode(&mut self, rc: &mut Range, pos_state: usize) -> Result<usize, String> {
        if rc.bit(&mut self.choice)? == 0 {
            return Ok(rc.tree(&mut self.low[pos_state], 3)? as usize);
        }
        if rc.bit(&mut self.choice2)? == 0 {
            return Ok(8 + rc.tree(&mut self.mid[pos_state], 3)? as usize);
        }
        Ok(16 + rc.tree(&mut self.high, 8)? as usize)
    }
}

/// Decodes `src` (properties first) into at most `limit` bytes.
pub fn decompress(src: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if src.len() < 5 + 5 {
        return Err("LZMA data too short".into());
    }
    let d = src[0] as usize;
    if d >= 9 * 5 * 5 {
        return Err("invalid LZMA properties".into());
    }
    let (lc, lp, pb) = (d % 9, (d / 9) % 5, d / 45);
    let mut rc = Range { src, pos: 6, range: u32::MAX, code: 0 };
    if src[5] != 0 {
        return Err("invalid LZMA stream".into());
    }
    for _ in 0..4 {
        rc.code = (rc.code << 8) | rc.byte()? as u32;
    }
    let mut literals = vec![[PROB_INIT; 0x300]; 1 << (lc + lp)];
    let mut is_match = [PROB_INIT; 192];
    let mut is_rep = [PROB_INIT; 12];
    let mut is_rep_g0 = [PROB_INIT; 12];
    let mut is_rep_g1 = [PROB_INIT; 12];
    let mut is_rep_g2 = [PROB_INIT; 12];
    let mut is_rep0_long = [PROB_INIT; 192];
    let mut slots = vec![[PROB_INIT; 64]; 4];
    let mut special = [PROB_INIT; 115];
    let mut align = [PROB_INIT; 16];
    let mut len = Lengths::new();
    let mut rep_len = Lengths::new();
    let (mut rep, mut state) = ([0usize; 4], 0usize);
    let mut out: Vec<u8> = Vec::new();
    while out.len() < limit {
        let pos_state = out.len() & ((1 << pb) - 1);
        if rc.bit(&mut is_match[(state << 4) + pos_state])? == 0 {
            let prev = out.last().copied().unwrap_or(0) as usize;
            let probs = &mut literals[((out.len() & ((1 << lp) - 1)) << lc) + (prev >> (8 - lc))];
            let mut sym = 1usize;
            if state >= 7 {
                let mut m = *out.get(out.len().wrapping_sub(rep[0] + 1)).ok_or("invalid LZMA distance")? as usize;
                while sym < 0x100 {
                    let mb = (m >> 7) & 1;
                    m <<= 1;
                    let b = rc.bit(&mut probs[((1 + mb) << 8) + sym])? as usize;
                    sym = (sym << 1) | b;
                    if mb != b {
                        break;
                    }
                }
            }
            while sym < 0x100 {
                sym = (sym << 1) | rc.bit(&mut probs[sym])? as usize;
            }
            out.push(sym as u8);
            state = if state < 4 { 0 } else if state < 10 { state - 3 } else { state - 6 };
            continue;
        }
        let length;
        if rc.bit(&mut is_rep[state])? == 1 {
            if out.is_empty() {
                return Err("invalid LZMA stream".into());
            }
            if rc.bit(&mut is_rep_g0[state])? == 0 {
                if rc.bit(&mut is_rep0_long[(state << 4) + pos_state])? == 0 {
                    state = if state < 7 { 9 } else { 11 };
                    let b = out[out.len() - rep[0] - 1];
                    out.push(b);
                    continue;
                }
            } else {
                let dist;
                if rc.bit(&mut is_rep_g1[state])? == 0 {
                    dist = rep[1];
                } else {
                    if rc.bit(&mut is_rep_g2[state])? == 0 {
                        dist = rep[2];
                    } else {
                        dist = rep[3];
                        rep[3] = rep[2];
                    }
                    rep[2] = rep[1];
                }
                rep[1] = rep[0];
                rep[0] = dist;
            }
            length = rep_len.decode(&mut rc, pos_state)?;
            state = if state < 7 { 8 } else { 11 };
        } else {
            rep = [0, rep[0], rep[1], rep[2]];
            length = len.decode(&mut rc, pos_state)?;
            state = if state < 7 { 7 } else { 10 };
            let slot = rc.tree(&mut slots[length.min(3)], 6)? as usize;
            rep[0] = if slot < 4 {
                slot
            } else {
                let bits = (slot >> 1) as u32 - 1;
                let base = (2 | (slot & 1)) << bits;
                if slot < 14 {
                    base + rc.reverse(&mut special[base - slot..], bits)? as usize
                } else {
                    let high = rc.direct(bits - 4)? as usize;
                    let low = rc.reverse(&mut align, 4)? as usize;
                    let d = base as u64 + ((high as u64) << 4) + low as u64;
                    if d == 0xffff_ffff {
                        // The end marker.
                        return Ok(out);
                    }
                    d as usize
                }
            };
        }
        let length = length + MATCH_LEN_MIN;
        if rep[0] >= out.len() {
            return Err("invalid LZMA distance".into());
        }
        let from = out.len() - rep[0] - 1;
        for k in 0..length.min(limit - out.len()) {
            out.push(out[from + k]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lzma_alone_stream() {
        // lzma.compress(b"abcabcabcabc hello hello hello", format=lzma.FORMAT_ALONE) without the
        // 8-byte size that follows the properties.
        let src = include_bytes!("../../../tests/fixtures/sqb/sample.lzma");
        let mut raw = src[..5].to_vec();
        raw.extend_from_slice(&src[13..]);
        assert_eq!(super::decompress(&raw, 1 << 20).unwrap(), b"abcabcabcabc hello hello hello".repeat(40));
    }
}
