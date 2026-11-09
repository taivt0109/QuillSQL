//! Strings compared as the collation compares them. The classic data compare tool compares non-key text in
//! the client with the collation's Windows locale and its case, accent, kana and width
//! settings, so canonically equivalent forms (e + combining acute and é) and characters the
//! locale gives no weight are equal. On Windows the same locale comparison is used here; elsewhere
//! (and for ASCII-only text, where the two agree) the folding in `value::Fold` decides.

use super::value::Fold;

/// Collation families (the name before the version and the option suffixes) and the Windows
/// locale id each compares by.
const LOCALES: &[(&str, u32)] = &[
    ("Albanian", 1052), ("Arabic", 1025), ("Assamese", 1101), ("Azeri_Cyrillic", 2092), ("Azeri_Latin", 1068), ("Bashkir", 1133),
    ("Bengali", 1093), ("Bosnian_Cyrillic", 8218), ("Bosnian_Latin", 5146), ("Breton", 1150), ("Chinese_Hong_Kong_Stroke", 3076),
    ("Chinese_PRC_Stroke", 133124), ("Chinese_PRC", 2052), ("Chinese_Simplified_Pinyin", 2052), ("Chinese_Simplified_Stroke_Order", 133124),
    ("Chinese_Taiwan_Bopomofo", 197636), ("Chinese_Taiwan_Stroke", 1028), ("Chinese_Traditional_Bopomofo", 197636),
    ("Chinese_Traditional_Pinyin", 5124), ("Chinese_Traditional_Stroke_Count", 1028), ("Chinese_Traditional_Stroke_Order", 136196),
    ("Corsican", 1155), ("Croatian", 1050), ("Cyrillic_General", 1049), ("Czech", 1029), ("Danish_Greenlandic", 1030),
    ("Danish_Norwegian", 1030), ("Dari", 1164), ("Divehi", 1125), ("Estonian", 1061), ("Finnish_Swedish", 1035), ("French", 1036),
    ("Frisian", 1122), ("Georgian_Modern_Sort", 66615), ("German_PhoneBook", 66567), ("Greek", 1032), ("Hebrew", 1037),
    ("Hungarian_Technical", 66574), ("Hungarian", 1038), ("Icelandic", 1039), ("Indic_General", 1081), ("Japanese_Bushu_Kakusu", 263185),
    ("Japanese_Unicode", 66577), ("Japanese_XJIS", 1041), ("Japanese", 1041), ("Kazakh", 1087), ("Khmer", 1107), ("Korean_Wansung", 1042),
    ("Korean", 1042), ("Lao", 1108), ("Latin1_General", 1033), ("Latvian", 1062), ("Lithuanian", 1063), ("Macedonian_FYROM", 1071),
    ("Maltese", 1082), ("Maori", 1153), ("Mapudungan", 1146), ("Modern_Spanish", 3082), ("Mohawk", 1148), ("Nepali", 1121),
    ("Norwegian", 1044), ("Pashto", 1123), ("Persian", 1065), ("Polish", 1045), ("Romanian", 1048), ("Romansh", 1047),
    ("Sami_Norway", 1083), ("Sami_Sweden_Finland", 2107), ("Serbian_Cyrillic", 3098), ("Serbian_Latin", 2074), ("Slovak", 1051),
    ("Slovenian", 1060), ("Syriac", 1114), ("Tamazight", 2143), ("Tatar", 1092), ("Thai", 1054), ("Tibetan", 1105),
    ("Traditional_Spanish", 1034), ("Turkish", 1055), ("Turkmen", 1090), ("Uighur", 1152), ("Ukrainian", 1058), ("Upper_Sorbian", 1070),
    ("Urdu", 1056), ("Uzbek_Latin", 1091), ("Vietnamese", 1066), ("Welsh", 1106), ("Yakut", 1157),
    // SQL collations.
    ("SQL_Croatian", 1050), ("SQL_Czech", 1029), ("SQL_Danish", 1030), ("SQL_EBCDIC273", 66567), ("SQL_EBCDIC277", 1030),
    ("SQL_EBCDIC278", 1035), ("SQL_EBCDIC284", 3082), ("SQL_EBCDIC297", 1036), ("SQL_Estonian", 1061), ("SQL_Hungarian", 1038),
    ("SQL_Icelandic", 1039), ("SQL_Latin1_General_CP1254", 1055), ("SQL_Latvian", 1062), ("SQL_Lithuanian", 1063), ("SQL_Polish", 1045),
    ("SQL_Romanian", 1048), ("SQL_Scandinavian", 1035), ("SQL_Slovak", 1051), ("SQL_Slovenian", 1060), ("SQL_SwedishPhone", 1035),
    ("SQL_SwedishStd", 1035), ("SQL_Ukrainian", 1058), ("SQL_", 1033),
];

/// The Windows locale id a (non-binary) collation compares by; 0 when not known.
pub fn locale(collation: Option<&str>) -> u32 {
    let Some(c) = collation.filter(|c| !c.is_empty()) else { return 0 };
    if c.to_ascii_uppercase().contains("_BIN") {
        return 0;
    }
    LOCALES
        .iter()
        .filter(|(f, _)| c.get(..f.len()).is_some_and(|h| h.eq_ignore_ascii_case(f)) && (f.ends_with('_') || c.as_bytes().get(f.len()) == Some(&b'_')))
        .max_by_key(|(f, _)| f.len())
        .map_or(0, |(_, l)| *l)
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CompareStringW(locale: u32, flags: u32, s1: *const u16, n1: i32, s2: *const u16, n2: i32) -> i32;
}

/// Whether the locale finds the strings equal under the fold; `None` when it cannot tell.
#[cfg(windows)]
pub fn equal(locale: u32, fold: Fold, a: &str, b: &str) -> Option<bool> {
    if locale == 0 {
        return None;
    }
    let flags = (fold.case as u32) | (fold.accent as u32) << 1 | (fold.kana as u32) << 16 | (fold.width as u32) << 17;
    let (x, y): (Vec<u16>, Vec<u16>) = (a.encode_utf16().collect(), b.encode_utf16().collect());
    let (n1, n2) = (i32::try_from(x.len()).ok()?, i32::try_from(y.len()).ok()?);
    // SAFETY: both buffers live for the call and their lengths are passed with them.
    let r = unsafe { CompareStringW(locale, flags, x.as_ptr(), n1, y.as_ptr(), n2) };
    (r != 0).then_some(r == 2)
}

#[cfg(not(windows))]
pub fn equal(_locale: u32, _fold: Fold, _a: &str, _b: &str) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales() {
        assert_eq!(locale(Some("SQL_Latin1_General_CP1_CI_AS")), 1033);
        assert_eq!(locale(Some("Latin1_General_100_CI_AS_SC")), 1033);
        assert_eq!(locale(Some("Chinese_PRC_Stroke_90_CI_AS")), 133124);
        assert_eq!(locale(Some("SQL_Latin1_General_CP1254_CI_AS")), 1055);
        assert_eq!(locale(Some("Latin1_General_BIN2")), 0);
        assert_eq!(locale(Some("Klingon_CI_AS")), 0);
    }

    #[cfg(windows)]
    #[test]
    fn canonical_forms_are_equal() {
        let ci = Fold::of(Some("SQL_Latin1_General_CP1_CI_AS"));
        assert_eq!(equal(1033, ci, "e\u{301}", "\u{e9}"), Some(true));
        assert_eq!(equal(1033, ci, "Caf\u{e9}", "cafe"), Some(false));
        assert_eq!(equal(1033, ci, "ABC", "abc"), Some(true));
    }
}
