//! DES (FIPS 46-3) in CBC mode with PKCS#7 padding: only to read the passwords the classic compare tool and
//! The classic data compare tool save in project files, which use a fixed, publicly known key. Not for security.

const IP: [u8; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6, 64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51,
    43, 35, 27, 19, 11, 3, 61, 53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];
const FP: [u8; 64] = [
    40, 8, 48, 16, 56, 24, 64, 32, 39, 7, 47, 15, 55, 23, 63, 31, 38, 6, 46, 14, 54, 22, 62, 30, 37, 5, 45, 13, 53, 21, 61, 29, 36, 4, 44, 12, 52, 20, 60, 28, 35, 3,
    43, 11, 51, 19, 59, 27, 34, 2, 42, 10, 50, 18, 58, 26, 33, 1, 41, 9, 49, 17, 57, 25,
];
const E: [u8; 48] = [
    32, 1, 2, 3, 4, 5, 4, 5, 6, 7, 8, 9, 8, 9, 10, 11, 12, 13, 12, 13, 14, 15, 16, 17, 16, 17, 18, 19, 20, 21, 20, 21, 22, 23, 24, 25, 24, 25, 26, 27, 28, 29, 28, 29,
    30, 31, 32, 1,
];
const P: [u8; 32] = [16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19, 13, 30, 6, 22, 11, 4, 25];
const PC1: [u8; 56] = [
    57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60, 52, 44, 36, 63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22,
    14, 6, 61, 53, 45, 37, 29, 21, 13, 5, 28, 20, 12, 4,
];
const PC2: [u8; 48] = [
    14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2, 41, 52, 31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53,
    46, 42, 50, 36, 29, 32,
];
const SHIFTS: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];
const S: [[u8; 64]; 8] = [
    [
        14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12, 11, 9, 5, 3, 8, 4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0,
        15, 12, 8, 2, 4, 9, 1, 7, 5, 11, 3, 14, 10, 0, 6, 13,
    ],
    [
        15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1, 10, 6, 9, 11, 5, 0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15,
        13, 8, 10, 1, 3, 15, 4, 2, 11, 6, 7, 12, 0, 5, 14, 9,
    ],
    [
        10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5, 14, 12, 11, 15, 1, 13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7,
        1, 10, 13, 0, 6, 9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12,
    ],
    [
        7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2, 12, 1, 10, 14, 9, 10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4,
        3, 15, 0, 6, 10, 1, 13, 8, 9, 4, 5, 11, 12, 7, 2, 14,
    ],
    [
        2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15, 10, 3, 9, 8, 6, 4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14,
        11, 8, 12, 7, 1, 14, 2, 13, 6, 15, 0, 9, 10, 4, 5, 3,
    ],
    [
        12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13, 14, 0, 11, 3, 8, 9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6,
        4, 3, 2, 12, 9, 5, 15, 10, 11, 14, 1, 7, 6, 0, 8, 13,
    ],
    [
        4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5, 12, 2, 15, 8, 6, 1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2,
        6, 11, 13, 8, 1, 4, 10, 7, 9, 5, 0, 15, 14, 2, 3, 12,
    ],
    [
        13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6, 11, 0, 14, 9, 2, 7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8,
        2, 1, 14, 7, 4, 10, 8, 13, 15, 12, 9, 0, 3, 5, 6, 11,
    ],
];

/// Picks bits of `input` (`width` bits wide, bit 1 the most significant) in `table` order.
fn permute(input: u64, width: u32, table: &[u8]) -> u64 {
    table.iter().fold(0, |out, &pos| (out << 1) | ((input >> (width - pos as u32)) & 1))
}

fn subkeys(key: [u8; 8]) -> [u64; 16] {
    let k = permute(u64::from_be_bytes(key), 64, &PC1);
    let (mut c, mut d) = ((k >> 28) & 0xFFF_FFFF, k & 0xFFF_FFFF);
    let mut keys = [0; 16];
    for (i, shift) in SHIFTS.iter().enumerate() {
        let rot = |x: u64| ((x << shift) | (x >> (28 - shift))) & 0xFFF_FFFF;
        c = rot(c);
        d = rot(d);
        keys[i] = permute((c << 28) | d, 56, &PC2);
    }
    keys
}

fn feistel(r: u32, k: u64) -> u32 {
    let x = permute(r as u64, 32, &E) ^ k;
    let mut out = 0u64;
    for (i, sbox) in S.iter().enumerate() {
        let six = (x >> (42 - 6 * i)) & 0x3F;
        let row = ((six & 0x20) >> 4) | (six & 1);
        let col = (six >> 1) & 0xF;
        out = (out << 4) | sbox[(row * 16 + col) as usize] as u64;
    }
    permute(out, 32, &P) as u32
}

fn block(input: u64, keys: &[u64; 16], decrypt: bool) -> u64 {
    let x = permute(input, 64, &IP);
    let (mut l, mut r) = ((x >> 32) as u32, x as u32);
    for i in 0..16 {
        let k = keys[if decrypt { 15 - i } else { i }];
        (l, r) = (r, l ^ feistel(r, k));
    }
    permute(((r as u64) << 32) | l as u64, 64, &FP)
}

pub fn encrypt_cbc(key: [u8; 8], iv: [u8; 8], data: &[u8]) -> Vec<u8> {
    let keys = subkeys(key);
    let pad = 8 - data.len() % 8;
    let mut padded = data.to_vec();
    padded.extend(std::iter::repeat_n(pad as u8, pad));
    let mut prev = u64::from_be_bytes(iv);
    let mut out = Vec::with_capacity(padded.len());
    for chunk in padded.as_chunks::<8>().0 {
        prev = block(u64::from_be_bytes(*chunk) ^ prev, &keys, false);
        out.extend(prev.to_be_bytes());
    }
    out
}

/// None when the length or the padding is wrong.
pub fn decrypt_cbc(key: [u8; 8], iv: [u8; 8], data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || !data.len().is_multiple_of(8) {
        return None;
    }
    let keys = subkeys(key);
    let mut prev = u64::from_be_bytes(iv);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<8>().0 {
        let c = u64::from_be_bytes(*chunk);
        out.extend((block(c, &keys, true) ^ prev).to_be_bytes());
        prev = c;
    }
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 8 || out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_classic_test_vector_and_round_trips() {
        let keys = subkeys(0x133457799BBCDFF1u64.to_be_bytes());
        assert_eq!(block(0x0123456789ABCDEF, &keys, false), 0x85E813540F0AB405);
        assert_eq!(block(0x85E813540F0AB405, &keys, true), 0x0123456789ABCDEF);
        let (k, iv) = (*b"12345678", *b"abcdefgh");
        for text in ["", "p", "exactly8", "a longer password!"] {
            let c = encrypt_cbc(k, iv, text.as_bytes());
            assert_eq!(decrypt_cbc(k, iv, &c).unwrap(), text.as_bytes());
        }
        assert!(decrypt_cbc(k, iv, b"short").is_none());
    }
}
