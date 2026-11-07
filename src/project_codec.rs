//! Encodings inside SQL Compare / SQL Data Compare project files: saved passwords
//! (`encrypted="1"`: fixed-key DES, Base64) and the hashed object selection of
//! `SelectedSyncObjects`.

const KEY: [u8; 8] = [247, 218, 50, 184, 11, 167, 5, 117];
const IV: [u8; 8] = [147, 110, 85, 85, 133, 135, 198, 30];

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(b: &[u8]) -> String {
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = ((c[0] as u32) << 16) | ((*c.get(1).unwrap_or(&0) as u32) << 8) | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { B64[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        let v = B64.iter().position(|&x| x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A password saved by SQL Compare; empty when it cannot be read (as Redgate does).
pub fn decrypt_password(text: &str) -> String {
    base64_decode(text.trim())
        .and_then(|b| crate::des::decrypt_cbc(KEY, IV, &b))
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

/// The same scheme the other way, for tests and for files other tools must read.
pub fn encrypt_password(password: &str) -> String {
    base64_encode(&crate::des::encrypt_cbc(KEY, IV, password.as_bytes()))
}

/// The schema key SelectedSyncObjects uses for objects without a schema.
pub const DEFAULT_SCHEMA: &str = "RedGate_SQLCompare_Default_Schema";

/// The element name of a schema in SelectedSyncObjects, and whether it needs format version 2.
pub fn schema_token(schema: &str) -> (String, bool) {
    let units: Vec<u8> = schema.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let token = base64_encode(&units).replace('=', "_").replace('/', "-");
    let first = token.as_bytes().first().copied().unwrap_or(b'0');
    let valid = !token.contains('+') && !token.to_ascii_lowercase().starts_with("xml") && first != b'-' && first != b'.' && !first.is_ascii_digit();
    if valid { (token, false) } else { (format!("A{}", token.replace('+', ".")), true) }
}

/// The schema name back from its element name.
pub fn schema_from_token(token: &str, version: u32) -> Option<String> {
    let token = if version >= 2 && token.starts_with('A') { token[1..].replace('.', "+") } else { token.to_string() };
    let bytes = base64_decode(&token.replace('_', "=").replace('-', "/"))?;
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16(&units).ok()
}

/// MD5 of the object name in UTF-16, as SelectedSyncObjects keys objects.
pub fn object_hash(name: &str) -> [u8; 16] {
    let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    md5::compute(units).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_passwords_round_trip() {
        for p in ["", "secret", "pässwörd with spaces 123"] {
            assert_eq!(decrypt_password(&encrypt_password(p)), p);
        }
        assert_eq!(decrypt_password("not base64!"), "");
        assert_eq!(base64_decode(&base64_encode(b"ab")).unwrap(), b"ab");
    }

    #[test]
    fn schema_tokens_round_trip() {
        for s in ["dbo", "Sales", "x/y+z", DEFAULT_SCHEMA] {
            let (t, v2) = schema_token(s);
            assert_eq!(schema_from_token(&t, if v2 { 2 } else { 1 }).unwrap(), s, "{t}");
        }
    }
}
