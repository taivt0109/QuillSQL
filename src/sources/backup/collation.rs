//! Collation ids as SQL Server stores them (syscolpars, the boot page, sql_variant values):
//! the name and the code page of non-Unicode text. Windows collations are a designator (the
//! low bits and the version) plus flags; SQL collations are listed whole.

/// (designator bits, name, code page), generated from fn_helpcollations().
const DESIGNATORS: &[(u32, &str, u16)] = &[
    (0x1, "Arabic", 1256),
    (0x3, "Chinese_Taiwan_Stroke", 950),
    (0x4, "Czech", 1250),
    (0x5, "Danish_Norwegian", 1252),
    (0x7, "Greek", 1253),
    (0x8, "Latin1_General", 1252),
    (0x9, "Traditional_Spanish", 1252),
    (0xa, "Finnish_Swedish", 1252),
    (0xb, "French", 1252),
    (0xc, "Hebrew", 1255),
    (0xd, "Hungarian", 1250),
    (0xe, "Icelandic", 1252),
    (0x10, "Japanese", 932),
    (0x11, "Korean_Wansung", 949),
    (0x13, "Polish", 1250),
    (0x14, "Romanian", 1250),
    (0x15, "Cyrillic_General", 1251),
    (0x16, "Croatian", 1250),
    (0x17, "Slovak", 1250),
    (0x18, "Albanian", 1250),
    (0x19, "Thai", 874),
    (0x1a, "Turkish", 1254),
    (0x1b, "Ukrainian", 1251),
    (0x1c, "Slovenian", 1250),
    (0x1d, "Estonian", 1257),
    (0x1e, "Latvian", 1257),
    (0x1f, "Lithuanian", 1257),
    (0x20, "Vietnamese", 1258),
    (0x24, "Chinese_PRC", 936),
    (0x28, "Modern_Spanish", 1252),
    (0x29, "German_PhoneBook", 1252),
    (0x2a, "Hungarian_Technical", 1250),
    (0x2b, "Japanese_Unicode", 932),
    (0x2d, "Georgian_Modern_Sort", 1252),
    (0x2e, "Chinese_PRC_Stroke", 936),
    (0x2f, "Chinese_Taiwan_Bopomofo", 950),
    (0x20030, "Japanese_90", 932),
    (0x20031, "Chinese_PRC_90", 936),
    (0x20032, "Chinese_PRC_Stroke_90", 936),
    (0x20033, "Chinese_Taiwan_Bopomofo_90", 950),
    (0x20034, "Chinese_Taiwan_Stroke_90", 950),
    (0x20035, "Indic_General_90", 0),
    (0x20037, "Kazakh_90", 1251),
    (0x20038, "Uzbek_Latin_90", 1254),
    (0x20039, "Tatar_90", 1251),
    (0x2003a, "Macedonian_FYROM_90", 1251),
    (0x2003b, "Syriac_90", 0),
    (0x2003c, "Divehi_90", 0),
    (0x2003e, "Chinese_Hong_Kong_Stroke_90", 950),
    (0x20040, "Korean_90", 949),
    (0x40001, "Arabic_100", 1256),
    (0x40002, "Dari_100", 1256),
    (0x40004, "Czech_100", 1250),
    (0x40006, "Yakut_100", 1251),
    (0x40007, "Greek_100", 1253),
    (0x40008, "Latin1_General_100", 1252),
    (0x40009, "Traditional_Spanish_100", 1252),
    (0x4000a, "Finnish_Swedish_100", 1252),
    (0x4000b, "French_100", 1252),
    (0x4000c, "Hebrew_100", 1255),
    (0x4000d, "Hungarian_100", 1250),
    (0x4000e, "Icelandic_100", 1252),
    (0x4000f, "Corsican_100", 1252),
    (0x40012, "Maori_100", 0),
    (0x40013, "Polish_100", 1250),
    (0x40014, "Romanian_100", 1250),
    (0x40015, "Cyrillic_General_100", 1251),
    (0x40016, "Croatian_100", 1250),
    (0x40017, "Slovak_100", 1250),
    (0x40018, "Albanian_100", 1250),
    (0x40019, "Thai_100", 874),
    (0x4001a, "Turkish_100", 1254),
    (0x4001b, "Ukrainian_100", 1251),
    (0x4001c, "Slovenian_100", 1250),
    (0x4001d, "Estonian_100", 1257),
    (0x4001e, "Latvian_100", 1257),
    (0x4001f, "Lithuanian_100", 1257),
    (0x40020, "Vietnamese_100", 1258),
    (0x40025, "Uighur_100", 1256),
    (0x40026, "Breton_100", 1252),
    (0x40028, "Modern_Spanish_100", 1252),
    (0x40029, "German_PhoneBook_100", 1252),
    (0x4002a, "Hungarian_Technical_100", 1250),
    (0x4002d, "Georgian_Modern_Sort_100", 1252),
    (0x40035, "Indic_General_100", 0),
    (0x40037, "Kazakh_100", 1251),
    (0x40038, "Uzbek_Latin_100", 1254),
    (0x40039, "Tatar_100", 1251),
    (0x4003a, "Macedonian_FYROM_100", 1251),
    (0x4003b, "Syriac_100", 0),
    (0x4003c, "Divehi_100", 0),
    (0x4003f, "Mohawk_100", 1252),
    (0x40040, "Korean_100", 949),
    (0x40041, "Chinese_Traditional_Stroke_Count_100", 950),
    (0x40042, "Chinese_Traditional_Bopomofo_100", 950),
    (0x40043, "Chinese_Simplified_Pinyin_100", 936),
    (0x40044, "Chinese_Simplified_Stroke_Order_100", 936),
    (0x40045, "Chinese_Traditional_Pinyin_100", 950),
    (0x40046, "Chinese_Traditional_Stroke_Order_100", 950),
    (0x40047, "Danish_Greenlandic_100", 1252),
    (0x40048, "Japanese_XJIS_100", 932),
    (0x40049, "Japanese_Bushu_Kakusu_100", 932),
    (0x4004a, "Norwegian_100", 1252),
    (0x4004b, "Romansh_100", 1252),
    (0x4004c, "Serbian_Latin_100", 1250),
    (0x4004d, "Serbian_Cyrillic_100", 1251),
    (0x4004e, "Bosnian_Latin_100", 1250),
    (0x4004f, "Bosnian_Cyrillic_100", 1251),
    (0x40050, "Urdu_100", 1256),
    (0x40051, "Persian_100", 1256),
    (0x40052, "Mapudungan_100", 1252),
    (0x40053, "Upper_Sorbian_100", 1252),
    (0x40054, "Bashkir_100", 1251),
    (0x40055, "Maltese_100", 0),
    (0x40056, "Sami_Norway_100", 1252),
    (0x40057, "Sami_Sweden_Finland_100", 1252),
    (0x40058, "Turkmen_100", 1250),
    (0x40059, "Bengali_100", 0),
    (0x4005a, "Assamese_100", 0),
    (0x4005b, "Pashto_100", 0),
    (0x4005c, "Tibetan_100", 0),
    (0x4005d, "Welsh_100", 1252),
    (0x4005e, "Khmer_100", 0),
    (0x4005f, "Lao_100", 0),
    (0x40060, "Frisian_100", 1252),
    (0x40061, "Tamazight_100", 1252),
    (0x40062, "Nepali_100", 0),
    (0x40063, "Azeri_Latin_100", 1254),
    (0x40064, "Azeri_Cyrillic_100", 1251),
    (0x60048, "Japanese_XJIS_140", 932),
    (0x60049, "Japanese_Bushu_Kakusu_140", 932),
];

/// (collation id, name, code page) of the SQL collations.
const SQL_COLLATIONS: &[(u32, &str, u16)] = &[
    (0x1e000808, "SQL_Latin1_General_CP437_BIN2", 437),
    (0x1e010008, "SQL_Latin1_General_CP437_BIN", 437),
    (0x1f00c008, "SQL_Latin1_General_CP437_CS_AS", 437),
    (0x2000d008, "SQL_Latin1_General_CP437_CI_AS", 437),
    (0x2100d008, "SQL_Latin1_General_Pref_CP437_CI_AS", 437),
    (0x2200f008, "SQL_Latin1_General_CP437_CI_AI", 437),
    (0x28000808, "SQL_Latin1_General_CP850_BIN2", 850),
    (0x28010008, "SQL_Latin1_General_CP850_BIN", 850),
    (0x2900c008, "SQL_Latin1_General_CP850_CS_AS", 850),
    (0x2a00d008, "SQL_Latin1_General_CP850_CI_AS", 850),
    (0x2b00d008, "SQL_Latin1_General_Pref_CP850_CI_AS", 850),
    (0x2c00f008, "SQL_Latin1_General_CP850_CI_AI", 850),
    (0x3100d008, "SQL_1xCompat_CP850_CI_AS", 850),
    (0x3300c008, "SQL_Latin1_General_CP1_CS_AS", 1252),
    (0x3400d008, "SQL_Latin1_General_CP1_CI_AS", 1252),
    (0x3500d008, "SQL_Latin1_General_Pref_CP1_CI_AS", 1252),
    (0x3600f008, "SQL_Latin1_General_CP1_CI_AI", 1252),
    (0x3700c008, "SQL_AltDiction_CP850_CS_AS", 850),
    (0x3800d008, "SQL_AltDiction_Pref_CP850_CI_AS", 850),
    (0x3900f008, "SQL_AltDiction_CP850_CI_AI", 850),
    (0x3a00d00a, "SQL_Scandinavian_Pref_CP850_CI_AS", 850),
    (0x3b00c00a, "SQL_Scandinavian_CP850_CS_AS", 850),
    (0x3c00d00a, "SQL_Scandinavian_CP850_CI_AS", 850),
    (0x3d00d008, "SQL_AltDiction_CP850_CI_AS", 850),
    (0x5100c008, "SQL_Latin1_General_CP1250_CS_AS", 1250),
    (0x5200d008, "SQL_Latin1_General_CP1250_CI_AS", 1250),
    (0x5300c004, "SQL_Czech_CP1250_CS_AS", 1250),
    (0x5400d004, "SQL_Czech_CP1250_CI_AS", 1250),
    (0x5500c00d, "SQL_Hungarian_CP1250_CS_AS", 1250),
    (0x5600d00d, "SQL_Hungarian_CP1250_CI_AS", 1250),
    (0x5700c013, "SQL_Polish_CP1250_CS_AS", 1250),
    (0x5800d013, "SQL_Polish_CP1250_CI_AS", 1250),
    (0x5900c014, "SQL_Romanian_CP1250_CS_AS", 1250),
    (0x5a00d014, "SQL_Romanian_CP1250_CI_AS", 1250),
    (0x5b00c016, "SQL_Croatian_CP1250_CS_AS", 1250),
    (0x5c00d016, "SQL_Croatian_CP1250_CI_AS", 1250),
    (0x5d00c017, "SQL_Slovak_CP1250_CS_AS", 1250),
    (0x5e00d017, "SQL_Slovak_CP1250_CI_AS", 1250),
    (0x5f00c01c, "SQL_Slovenian_CP1250_CS_AS", 1250),
    (0x6000d01c, "SQL_Slovenian_CP1250_CI_AS", 1250),
    (0x6900c008, "SQL_Latin1_General_CP1251_CS_AS", 1251),
    (0x6a00d008, "SQL_Latin1_General_CP1251_CI_AS", 1251),
    (0x6b00c01b, "SQL_Ukrainian_CP1251_CS_AS", 1251),
    (0x6c00d01b, "SQL_Ukrainian_CP1251_CI_AS", 1251),
    (0x7100c008, "SQL_Latin1_General_CP1253_CS_AS", 1253),
    (0x7200d008, "SQL_Latin1_General_CP1253_CI_AS", 1253),
    (0x7800c008, "SQL_MixDiction_CP1253_CS_AS", 1253),
    (0x7a00c008, "SQL_AltDiction2_CP1253_CS_AS", 1253),
    (0x7c00f008, "SQL_Latin1_General_CP1253_CI_AI", 1253),
    (0x8100c01a, "SQL_Latin1_General_CP1254_CS_AS", 1254),
    (0x8200d01a, "SQL_Latin1_General_CP1254_CI_AS", 1254),
    (0x8900c008, "SQL_Latin1_General_CP1255_CS_AS", 1255),
    (0x8a00d008, "SQL_Latin1_General_CP1255_CI_AS", 1255),
    (0x9100c008, "SQL_Latin1_General_CP1256_CS_AS", 1256),
    (0x9200d008, "SQL_Latin1_General_CP1256_CI_AS", 1256),
    (0x9900c008, "SQL_Latin1_General_CP1257_CS_AS", 1257),
    (0x9a00d008, "SQL_Latin1_General_CP1257_CI_AS", 1257),
    (0x9b00c01d, "SQL_Estonian_CP1257_CS_AS", 1257),
    (0x9c00d01d, "SQL_Estonian_CP1257_CI_AS", 1257),
    (0x9d00c01e, "SQL_Latvian_CP1257_CS_AS", 1257),
    (0x9e00d01e, "SQL_Latvian_CP1257_CI_AS", 1257),
    (0x9f00c01f, "SQL_Lithuanian_CP1257_CS_AS", 1257),
    (0xa000d01f, "SQL_Lithuanian_CP1257_CI_AS", 1257),
    (0xb700d005, "SQL_Danish_Pref_CP1_CI_AS", 1252),
    (0xb800d00a, "SQL_SwedishPhone_Pref_CP1_CI_AS", 1252),
    (0xb900d00a, "SQL_SwedishStd_Pref_CP1_CI_AS", 1252),
    (0xba00d00e, "SQL_Icelandic_Pref_CP1_CI_AS", 1252),
    (0xd200c008, "SQL_EBCDIC037_CP1_CS_AS", 1252),
    (0xd300c029, "SQL_EBCDIC273_CP1_CS_AS", 1252),
    (0xd400c005, "SQL_EBCDIC277_CP1_CS_AS", 1252),
    (0xd500c00a, "SQL_EBCDIC278_CP1_CS_AS", 1252),
    (0xd600c008, "SQL_EBCDIC280_CP1_CS_AS", 1252),
    (0xd700c028, "SQL_EBCDIC284_CP1_CS_AS", 1252),
    (0xd800c008, "SQL_EBCDIC285_CP1_CS_AS", 1252),
    (0xd900c00b, "SQL_EBCDIC297_CP1_CS_AS", 1252),
    (0xda00c005, "SQL_EBCDIC277_2_CP1_CS_AS", 1252),
    (0xdb00c029, "SQL_EBCDIC1141_CP1_CS_AS", 1252),
];

const UTF8: u32 = 0x80;
const SC: u32 = 0x100;
const NOT_VSS: u32 = 0x200;
const BIN2: u32 = 0x800;
const CI: u32 = 0x1000;
const AI: u32 = 0x2000;
const KI: u32 = 0x4000;
const WI: u32 = 0x8000;
const BIN: u32 = 0x10000;

fn designator(id: u32) -> Option<&'static (u32, &'static str, u16)> {
    let key = (id & 0x7f) | (id & 0xe0000);
    DESIGNATORS.iter().find(|d| d.0 == key)
}

/// The collation's name; `None` for 0 and unknown ids.
pub fn name(id: u32) -> Option<String> {
    if id == 0 {
        return None;
    }
    if let Some(s) = SQL_COLLATIONS.iter().find(|s| s.0 == id) {
        return Some(s.1.to_string());
    }
    let d = designator(id)?;
    let version = (id >> 17) & 7;
    let mut n = d.1.to_string();
    if id & BIN2 != 0 {
        n.push_str("_BIN2");
    } else if id & BIN != 0 {
        n.push_str("_BIN");
    } else {
        n.push_str(if id & CI != 0 { "_CI" } else { "_CS" });
        n.push_str(if id & AI != 0 { "_AI" } else { "_AS" });
        if id & KI == 0 {
            n.push_str("_KS");
        }
        if id & WI == 0 {
            n.push_str("_WS");
        }
        if version < 3 && id & SC != 0 {
            n.push_str("_SC");
        }
        if version == 3 && id & NOT_VSS == 0 {
            n.push_str("_VSS");
        }
    }
    if id & UTF8 != 0 {
        n.push_str("_UTF8");
    }
    Some(n)
}

/// The code page of char and varchar data in the collation (65001 for UTF-8 collations).
pub fn code_page(id: u32) -> u16 {
    if let Some(s) = SQL_COLLATIONS.iter().find(|s| s.0 == id) {
        return s.2;
    }
    if id & UTF8 != 0 {
        return 65001;
    }
    designator(id).map_or(1252, |d| d.2)
}

/// Decodes char/varchar bytes in a code page. Code pages other than 1252, UTF-8 and the
/// single-byte Latin-1 family decode their ASCII range only (other bytes become U+FFFD).
pub fn decode(bytes: &[u8], code_page: u16) -> String {
    match code_page {
        65001 => String::from_utf8_lossy(bytes).into_owned(),
        1252 => bytes.iter().map(|&b| cp1252(b)).collect(),
        28591 => bytes.iter().map(|&b| b as char).collect(),
        _ => bytes.iter().map(|&b| if b < 0x80 { b as char } else { '\u{fffd}' }).collect(),
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20ac, 0x81, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8d, 0x017d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d,
        0x2022, 0x2013, 0x2014, 0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0x9d, 0x017e, 0x0178,
    ];
    match b {
        0x80..=0x9f => char::from_u32(HIGH[(b - 0x80) as usize] as u32).unwrap_or('\u{fffd}'),
        _ => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_ids() {
        assert_eq!(name(0x3400d008).as_deref(), Some("SQL_Latin1_General_CP1_CI_AS"));
        assert_eq!(name(0x0000d008).as_deref(), Some("Latin1_General_CI_AS"));
        assert_eq!(name(0x00040188).as_deref(), Some("Latin1_General_100_CS_AS_KS_WS_SC_UTF8"));
        assert_eq!(name(0x00040808).as_deref(), Some("Latin1_General_100_BIN2"));
        assert_eq!(name(0x6d148).as_deref(), Some("Japanese_XJIS_140_CI_AS_VSS"));
        assert_eq!(name(0x6d348).as_deref(), Some("Japanese_XJIS_140_CI_AS"));
        assert_eq!(code_page(0x00040188), 65001);
        assert_eq!(decode(&[0x80, b'a'], 1252), "\u{20ac}a");
    }
}
