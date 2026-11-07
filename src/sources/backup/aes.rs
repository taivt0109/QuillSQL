//! AES (FIPS-197) forward cipher and the counter mode SQL Backup encrypts with: before each
//! 16-byte block the counter, a 128-bit big-endian number, is incremented, and the block is
//! XORed with the encrypted counter. Decrypting is the same operation.

/// The S-box: the multiplicative inverse in GF(2^8), then the affine transform.
fn sbox() -> [u8; 256] {
    let mut s = [0u8; 256];
    let (mut p, mut q) = (1u8, 1u8);
    loop {
        // p runs through the field by multiplying by 3, q by dividing by 3.
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        s[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    s[0] = 0x63;
    s
}

fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
}

pub struct Aes {
    sbox: [u8; 256],
    /// Round keys, 16 bytes each.
    keys: Vec<[u8; 16]>,
}

impl Aes {
    /// A 16- or 32-byte key.
    pub fn new(key: &[u8]) -> Aes {
        let sbox = sbox();
        let nk = key.len() / 4;
        let rounds = nk + 6;
        let mut w: Vec<[u8; 4]> = key.chunks(4).map(|c| c.try_into().unwrap()).collect();
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [sbox[t[1] as usize] ^ rcon, sbox[t[2] as usize], sbox[t[3] as usize], sbox[t[0] as usize]];
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| sbox[b as usize]);
            }
            let prev = w[i - nk];
            w.push([prev[0] ^ t[0], prev[1] ^ t[1], prev[2] ^ t[2], prev[3] ^ t[3]]);
        }
        let keys = w.chunks(4).map(|c| c.concat().try_into().unwrap()).collect();
        Aes { sbox, keys }
    }

    pub fn encrypt(&self, block: &mut [u8; 16]) {
        let last = self.keys.len() - 1;
        xor(block, &self.keys[0]);
        for round in 1..=last {
            for b in block.iter_mut() {
                *b = self.sbox[*b as usize];
            }
            // ShiftRows: row r (bytes r, r+4, ...) moves r columns left.
            let s = *block;
            for c in 0..4 {
                for r in 0..4 {
                    block[4 * c + r] = s[4 * ((c + r) % 4) + r];
                }
            }
            if round != last {
                for c in block.chunks_mut(4) {
                    let a = [c[0], c[1], c[2], c[3]];
                    let all = a[0] ^ a[1] ^ a[2] ^ a[3];
                    for r in 0..4 {
                        c[r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
                    }
                }
            }
            xor(block, &self.keys[round]);
        }
    }

    /// Counter mode over `data`, starting from `counter` (incremented before the first block).
    pub fn ctr(&self, counter: &[u8; 16], data: &mut [u8]) {
        let mut n = u128::from_be_bytes(*counter);
        for chunk in data.chunks_mut(16) {
            n = n.wrapping_add(1);
            let mut stream = n.to_be_bytes();
            self.encrypt(&mut stream);
            for (d, s) in chunk.iter_mut().zip(stream) {
                *d ^= s;
            }
        }
    }
}

fn xor(block: &mut [u8; 16], key: &[u8; 16]) {
    for (b, k) in block.iter_mut().zip(key) {
        *b ^= k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn fips_197_examples() {
        let plain: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        for (key, cipher) in [
            ("000102030405060708090a0b0c0d0e0f", "69c4e0d86a7b0430d8cdb78070b4c55a"),
            ("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "8ea2b7ca516745bfeafc49904b496089"),
        ] {
            let mut b = plain;
            Aes::new(&hex(key)).encrypt(&mut b);
            assert_eq!(b.to_vec(), hex(cipher));
        }
    }

    #[test]
    fn counter_mode() {
        // SP 800-38A F.5.1, whose counter is the one before ours.
        let aes = Aes::new(&hex("2b7e151628aed2a6abf7158809cf4f3c"));
        let counter: [u8; 16] = hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfefe").try_into().unwrap();
        let mut data = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        aes.ctr(&counter, &mut data);
        assert_eq!(data, hex("874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff"));
    }
}
