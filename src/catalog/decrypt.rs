//! DecryptPost2kEncryptedObjects: the text of modules created WITH ENCRYPTION, recovered as SQL
//! Compare does. SQL Server keeps the text RC4-encrypted in sys.sysobjvalues (readable only over
//! the dedicated administrator connection); the key is the SHA-1 of the database's family GUID,
//! the object id and the sub-object number.

use std::collections::HashMap;
use std::time::Duration;

use tiberius::ColumnData;

use super::model::*;
use crate::db::{self, Connection, ConnectionSpec};

/// The decrypted text, or `None` when the bytes don't decode to text.
pub fn decrypt(family_guid: &str, object_id: i32, number: u16, cipher: &[u8]) -> Option<String> {
    let mut material = guid_bytes(family_guid)?.to_vec();
    material.extend_from_slice(&(object_id as u32).to_le_bytes());
    material.extend_from_slice(&number.to_le_bytes());
    let key = sha1(&material);
    let mut plain = cipher.to_vec();
    rc4(&key, &mut plain);
    if !plain.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = plain.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16(&units).ok()
}

/// A GUID in its in-memory (little-endian first three fields) byte order.
fn guid_bytes(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.trim().trim_matches(['{', '}']).chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut b = [0u8; 16];
    for (i, byte) in b.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    b[0..4].reverse();
    b[4..6].reverse();
    b[6..8].reverse();
    Some(b)
}

fn rc4(key: &[u8], data: &mut [u8]) {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut x, mut y) = (0u8, 0u8);
    for byte in data.iter_mut() {
        x = x.wrapping_add(1);
        y = y.wrapping_add(s[x as usize]);
        s.swap(x as usize, y as usize);
        *byte ^= s[s[x as usize].wrapping_add(s[y as usize]) as usize];
    }
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// The dedicated administrator connection for `spec`: the instance's DAC port from the SQL
/// Browser (1434 for a default instance).
async fn dac_spec(spec: &ConnectionSpec) -> Option<ConnectionSpec> {
    if spec.connection_string.is_some() {
        return None;
    }
    let server = spec.server.trim();
    let server = server.strip_prefix("admin:").or_else(|| server.strip_prefix("ADMIN:")).unwrap_or(server);
    let addr = db::parse_server(server).ok()?;
    let port = match &addr.instance {
        Some(instance) => browser_dac_port(&addr.host, instance).await?,
        None => 1434,
    };
    let mut dac = spec.clone();
    dac.server = format!("{},{port}", addr.host);
    dac.connect_timeout = Duration::from_secs(5);
    Some(dac)
}

/// Asks the SQL Browser for a named instance's DAC port (CLNT_UCAST_DAC).
async fn browser_dac_port(host: &str, instance: &str) -> Option<u16> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    socket.connect((host, 1434)).await.ok()?;
    let mut request = vec![0x0F, 0x01];
    request.extend_from_slice(instance.as_bytes());
    request.push(0);
    socket.send(&request).await.ok()?;
    let mut reply = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut reply)).await.ok()?.ok()?;
    (n >= 6 && reply[0] == 0x05).then(|| u16::from_le_bytes([reply[4], reply[5]]))
}

/// Fills in the text of encrypted views, routines and triggers when the server lets us read
/// the encrypted image. Objects that stay unreadable keep their `encrypted` flag.
pub(crate) async fn decrypt_modules(conn: &mut Connection, spec: &ConnectionSpec, model: &mut Database) -> Result<(), String> {
    let encrypted = model.views.iter().any(|v| v.encrypted)
        || model.routines.iter().any(|r| r.encrypted)
        || model.tables.iter().any(|t| t.triggers.iter().any(|tr| tr.encrypted))
        || model.views.iter().any(|v| v.triggers.iter().any(|tr| tr.encrypted))
        || model.ddl_triggers.iter().any(|t| t.encrypted);
    if !encrypted {
        return Ok(());
    }
    let rows = db::query(
        conn,
        "SELECT CONVERT(nvarchar(36), family_guid) FROM sys.database_recovery_status WHERE database_id = DB_ID()",
    )
    .await?;
    let Some(guid) = rows.first().and_then(|r| match r.get_column_data(0).ok()? {
        ColumnData::String(Some(s)) => Some(s.to_string()),
        _ => None,
    }) else {
        return Ok(());
    };
    // Which object each encrypted module is: (schema, name) for schema objects, name for DDL triggers.
    let ids = db::query(
        conn,
        "SELECT m.object_id, ISNULL(SCHEMA_NAME(o.schema_id), N''), ISNULL(o.name, tr.name)
         FROM sys.sql_modules AS m
         LEFT JOIN sys.objects AS o ON o.object_id = m.object_id
         LEFT JOIN sys.triggers AS tr ON tr.object_id = m.object_id AND tr.parent_class = 0
         WHERE m.definition IS NULL",
    )
    .await?;
    let names: HashMap<i32, (String, String)> = ids
        .iter()
        .filter_map(|r| Some((r.get::<i32, _>(0)?, (r.get::<&str, _>(1)?.to_string(), r.get::<&str, _>(2)?.to_string()))))
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    let Some(dac) = dac_spec(spec).await else { return Ok(()) };
    // Without the dedicated administrator connection the image can't be read; the modules stay encrypted.
    let Ok(mut admin) = db::connect_async(&dac).await else { return Ok(()) };
    let list = names.keys().map(i32::to_string).collect::<Vec<_>>().join(", ");
    // The same database (another server could answer on the port), then the encrypted images;
    // any failure leaves the modules encrypted.
    let sql = format!(
        "IF EXISTS (SELECT 1 FROM sys.database_recovery_status WHERE database_id = DB_ID() AND CONVERT(nvarchar(36), family_guid) = N'{guid}')
         SELECT objid, CAST(subobjid AS int), imageval FROM sys.sysobjvalues WHERE valclass = 1 AND objid IN ({list}) ORDER BY objid, subobjid"
    );
    let Ok(rows) = db::query(&mut admin, &sql).await else { return Ok(()) };
    let mut texts: HashMap<i32, String> = HashMap::new();
    for r in &rows {
        let (Some(id), Some(number)) = (r.get::<i32, _>(0), r.get::<i32, _>(1)) else { continue };
        let Ok(ColumnData::Binary(Some(bytes))) = r.get_column_data(2) else { continue };
        if let Some(text) = decrypt(&guid, id, number as u16, bytes) {
            texts.entry(id).or_default().push_str(&text);
        }
    }
    let by_name: HashMap<(String, String), String> = texts.into_iter().filter_map(|(id, t)| Some((names.get(&id)?.clone(), t))).collect();
    let take = |schema: &str, name: &str| by_name.get(&(schema.to_string(), name.to_string())).cloned();
    for v in model.views.iter_mut() {
        if v.encrypted
            && let Some(t) = take(&v.schema, &v.name)
        {
            v.definition = t;
            v.encrypted = false;
        }
        for tr in v.triggers.iter_mut().filter(|tr| tr.encrypted) {
            if let Some(t) = take(&v.schema, &tr.name) {
                tr.definition = t;
                tr.encrypted = false;
            }
        }
    }
    for r in model.routines.iter_mut().filter(|r| r.encrypted) {
        if let Some(t) = take(&r.schema, &r.name) {
            r.definition = t;
            r.encrypted = false;
        }
    }
    for table in model.tables.iter_mut() {
        for tr in table.triggers.iter_mut().filter(|tr| tr.encrypted) {
            if let Some(t) = take(&table.schema, &tr.name) {
                tr.definition = t;
                tr.encrypted = false;
            }
        }
    }
    for tr in model.ddl_triggers.iter_mut().filter(|t| t.encrypted) {
        if let Some(t) = take("", &tr.name) {
            tr.definition = t;
            tr.encrypted = false;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn sha1_vectors() {
        let h = |s: &str| sha1(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(h("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(h(""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(h(&"a".repeat(1000)), "291e9a6c66994949b57ba5e650361e98fc36b1ba");
    }

    #[test]
    fn rc4_vector() {
        let mut data = b"Plaintext".to_vec();
        rc4(b"Key", &mut data);
        assert_eq!(data, hex("BBF316E8D940AF0AD3"));
    }

    #[test]
    fn decrypts_an_encrypted_view() {
        // Read from SQL Server 2022 over the DAC: family_guid of the database and sys.sysobjvalues.imageval
        // of `CREATE VIEW dbo.Secret WITH ENCRYPTION AS SELECT 42 AS answer` (object 901578250, subobjid 0).
        let cipher = hex("A0AF470AFAA76FB216F6F3526C96826CE10948F8E28B68D6278776879E021D578243F8ACB33CEB517F7AAEDCA99F407BF2437B3C4BCB89C3D7917DAA8116CBEF9AFC78021078F60063E4ADC8E838934F03A3EA735481E51A8A2B76B82504602C753AB5A17D64D21588B8A50031BE9B3F6BCCA2D4D264A7A9D5D7");
        let text = decrypt("EA316B82-9C46-460B-98D7-E43A994E8030", 901_578_250, 0, &cipher).unwrap();
        assert_eq!(text, "CREATE VIEW dbo.Secret WITH ENCRYPTION AS SELECT 42 AS answer");
        assert_ne!(decrypt("EA316B82-9C46-460B-98D7-E43A994E8031", 901_578_250, 0, &cipher).as_deref(), Some(text.as_str()));
    }
}
