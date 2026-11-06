//! Scripts folders in SQL Compare's layout: one `.sql` file per object under folders by
//! object type (`Tables`, `Stored Procedures`, `Security\Roles`...), and the database settings
//! in `RedGateDatabaseInfo.xml`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::parse::{self, Defaults, FileKind, Message, ObjectKey, Origins};
use super::script;
use crate::catalog::*;
use crate::schema::{DiffKind, Difference, ObjectType, Options};

/// The settings file names, newest first; the middle one is what SQL Compare writes.
const INFO_FILES: [&str; 3] = ["RedGateDatabaseProperties.xml", "RedGateDatabaseInfo.xml", "SqlCompareDatabaseInfo.xml"];
pub const INFO_FILE: &str = "RedGateDatabaseInfo.xml";

/// Folder of pre- and post-deployment scripts and other files that are not objects.
const CUSTOM_SCRIPTS: &str = "Custom Scripts";

/// Element names of `<Prefixes>` with SQL Compare's default folders, in file order.
const PREFIXES: [(&str, &str); 41] = [
    ("None", ""),
    ("Table", "Tables"),
    ("StoredProcedure", "Stored Procedures"),
    ("View", "Views"),
    ("Default", "Defaults"),
    ("FullTextCatalog", "Storage\\Full Text Catalogs"),
    ("Function", "Functions"),
    ("Role", "Security\\Roles"),
    ("Rule", "Rules"),
    ("User", "Security\\Users"),
    ("UserDefinedType", "Types\\User-defined Data Types"),
    ("Trigger", ""),
    ("DdlTrigger", "Database Triggers"),
    ("Assembly", "Assemblies"),
    ("Synonym", "Synonyms"),
    ("XmlSchemaCollection", "Types\\XML Schema Collections"),
    ("MessageType", "Service Broker\\Message Types"),
    ("Contract", "Service Broker\\Contracts"),
    ("Queue", "Service Broker\\Queues"),
    ("Service", "Service Broker\\Services"),
    ("Route", "Service Broker\\Routes"),
    ("EventNotification", "Service Broker\\Event Notifications"),
    ("PartitionScheme", "Storage\\Partition Schemes"),
    ("PartitionFunction", "Storage\\Partition Functions"),
    ("Field", ""),
    ("Index", ""),
    ("Schema", "Security\\Schemas"),
    ("ServiceBinding", "Service Broker\\Remote Service Bindings"),
    ("Certificate", "Security\\Certificates"),
    ("SymmetricKey", "Security\\Symmetric Keys"),
    ("AsymmetricKey", "Security\\Asymmetric Keys"),
    ("CheckConstraint", ""),
    ("FullTextStoplist", "Storage\\Full Text Stoplists"),
    ("ExtendedProperty", "Extended Properties"),
    ("Data", "Data"),
    ("Sequence", "Sequences"),
    ("SearchPropertyList", "Search Property Lists"),
    ("SecurityPolicy", "Security Policies"),
    ("ExternalFileFormat", "External Resources\\External File Formats"),
    ("ExternalDataSource", "External Resources\\External Data Sources"),
    ("ExternalTable", "Tables\\External Tables"),
];

fn prefix_key(kind: FileKind) -> &'static str {
    let FileKind::Object(ty) = kind else { return "DdlTrigger" };
    crate::schema::report::type_name(ty)
}

/// `RedGateDatabaseInfo.xml`.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderInfo {
    pub encoding: String,
    pub collation: String,
    pub default_schema: String,
    pub default_user: String,
    pub default_filegroup: String,
    /// SQL Server major version: 16 = 2022.
    pub database_version: u32,
    pub server_version: Option<String>,
    pub compatibility_level: Option<u32>,
    /// `Enterprise`, `AzureSqlDatabase`...
    pub engine_edition: Option<String>,
    pub is_azure: bool,
    pub max_data_file_size: u64,
    /// Present only when the file sets them.
    pub ansi_nulls: Option<bool>,
    pub quoted_identifier: Option<bool>,
    pub project_guid: String,
    /// Object type element name → folder.
    pub prefixes: BTreeMap<String, String>,
    pub data_files: Vec<String>,
    /// The file it was read from, if any.
    pub file: Option<PathBuf>,
}

impl Default for FolderInfo {
    fn default() -> Self {
        FolderInfo {
            encoding: "UTF8".into(),
            collation: "Latin1_General_CI_AS".into(),
            default_schema: "dbo".into(),
            default_user: "dbo".into(),
            default_filegroup: "PRIMARY".into(),
            database_version: 16,
            server_version: None,
            compatibility_level: None,
            engine_edition: None,
            is_azure: false,
            max_data_file_size: 10_485_760,
            ansi_nulls: None,
            quoted_identifier: None,
            project_guid: new_guid(),
            prefixes: PREFIXES.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            data_files: Vec::new(),
            file: None,
        }
    }
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

/// A random version-4 GUID, without a crate for it.
fn new_guid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

pub fn engine_edition_name(n: u32) -> &'static str {
    match n {
        1 => "Personal",
        2 => "Standard",
        3 => "Enterprise",
        4 => "Express",
        5 => "AzureSqlDatabase",
        6 => "AzureDataWarehouse",
        8 => "AzureManagedInstance",
        _ => "Unknown",
    }
}

fn engine_edition_number(s: &str) -> u32 {
    (0..=8).find(|&n| engine_edition_name(n).eq_ignore_ascii_case(s)).filter(|&n| engine_edition_name(n) != "Unknown").unwrap_or(0)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl FolderInfo {
    /// Reads the settings file of `dir`, with warnings for what is missing.
    pub fn read(dir: &Path) -> (FolderInfo, Vec<String>) {
        let mut warnings = Vec::new();
        let Some(file) = INFO_FILES.iter().map(|f| dir.join(f)).find(|p| p.is_file()) else {
            warnings.push(format!(
                "Could not find the file '{INFO_FILE}' in the scripts folder '{}': the default settings will be used instead.",
                dir.display()
            ));
            return (FolderInfo::default(), warnings);
        };
        let text = std::fs::read(&file).map(|b| decode(&b, "UTF8")).unwrap_or_default();
        let mut info = FolderInfo { file: Some(file), ..Default::default() };
        let Ok(doc) = roxmltree::Document::parse(&text) else {
            warnings.push(format!("The SqlCompareDatabaseInfo.xml file in the scripts folder '{}' is not in the expected format.", dir.display()));
            return (info, warnings);
        };
        let root = doc.root_element();
        let text_of = |name: &str| child(root, name).map(|c| c.text().unwrap_or_default().trim().to_string());
        if root.tag_name().name() == "DatabaseInformation" {
            let version: u32 = root.attribute("Version").and_then(|v| v.parse().ok()).unwrap_or(1);
            let partial = std::cell::Cell::new(false);
            let get = |name: &str, required: bool| {
                let v = text_of(name);
                if v.is_none() && required {
                    partial.set(true);
                }
                v
            };
            if let Some(v) = get("ScriptFileEncoding", true) {
                info.encoding = v;
            }
            if let Some(v) = get("DefaultCollation", false) {
                info.collation = v;
            }
            if let Some(v) = get("DefaultSchema", true) {
                info.default_schema = v;
            }
            if let Some(v) = get("DefaultUser", true) {
                info.default_user = v;
            }
            if let Some(v) = get("DefaultFilegroup", true) {
                info.default_filegroup = v;
            }
            match get("DatabaseVersion", true).map(|v| v.parse::<u32>()) {
                Some(Ok(v)) if !(10..=16).contains(&v) => {
                    warnings.push(format!(
                        "The SQL Server version specified for the scripts folder '{}' is not one that is supported by SQL Compare. These scripts will be treated as if they came from a SQL Server 2008 database.",
                        dir.display()
                    ));
                    info.database_version = v.clamp(10, 16);
                }
                Some(Ok(v)) => info.database_version = v,
                Some(Err(_)) => partial.set(true),
                None => {}
            }
            info.server_version = get("ServerVersion", false);
            if let Some(v) = get("IsAzure", false) {
                info.is_azure = v.eq_ignore_ascii_case("true") || v == "1";
            }
            if let Some(v) = get("MaxDataFileSize", true) {
                info.max_data_file_size = v.parse().unwrap_or(info.max_data_file_size);
            }
            if version > 2 {
                info.compatibility_level = get("CompatibilityLevel", false).and_then(|v| v.parse().ok());
                info.engine_edition = get("EngineEdition", false);
                if info.compatibility_level.is_none() || info.engine_edition.is_none() {
                    partial.set(true);
                }
            }
            if version > 1 {
                info.ansi_nulls = get("AnsiNulls", false).map(|v| v.eq_ignore_ascii_case("on"));
                info.quoted_identifier = get("QuotedIdentifier", false).map(|v| v.eq_ignore_ascii_case("on"));
            }
            if let Some(v) = get("ProjectGuid", false) {
                info.project_guid = v;
            }
            match child(root, "DataFileSet") {
                Some(set) => {
                    info.data_files = set.children().filter(|c| c.tag_name().name() == "DataFile").filter_map(|c| c.text().map(str::to_string)).collect();
                }
                None => partial.set(true),
            }
            if partial.get() {
                warnings.push(format!(
                    "Some settings are missing from the {INFO_FILE} file in the scripts folder '{}': default values will be used for any omitted setting.",
                    dir.display()
                ));
            }
        }
        let options = if root.tag_name().name() == "WriteToFileOptions" { Some(root) } else { child(root, "WriteToFileOptions") };
        if let Some(prefixes) = options.and_then(|o| child(o, "Prefixes")) {
            for p in prefixes.children().filter(|c| c.is_element()) {
                let name = p.tag_name().name();
                if info.prefixes.contains_key(name) {
                    let v = p.text().unwrap_or_default().trim();
                    info.prefixes.insert(name.to_string(), if v == "\\" { String::new() } else { v.to_string() });
                }
            }
        }
        (info, warnings)
    }

    /// Settings for a folder made from `db`.
    pub fn for_database(db: &Database) -> FolderInfo {
        FolderInfo {
            collation: db.collation.clone(),
            database_version: if db.server.major_version == 0 { 16 } else { db.server.major_version.clamp(10, 16) },
            server_version: (!db.server.product_version.is_empty()).then(|| db.server.product_version.clone()),
            compatibility_level: (db.compatibility_level != 0).then_some(db.compatibility_level),
            engine_edition: Some(engine_edition_name(db.server.engine_edition).to_string()),
            is_azure: db.server.is_azure_sql_database(),
            ..Default::default()
        }
    }

    pub fn defaults(&self) -> Defaults {
        Defaults {
            collation: self.collation.clone(),
            schema: self.default_schema.clone(),
            filegroup: self.default_filegroup.clone(),
            ansi_nulls: self.ansi_nulls.unwrap_or(true),
            quoted_identifier: self.quoted_identifier.unwrap_or(true),
        }
    }

    fn prefix(&self, key: &str) -> &str {
        self.prefixes.get(key).map(String::as_str).unwrap_or_default()
    }

    /// The file as SQL Compare writes it (UTF-8 with BOM, CRLF).
    pub fn xml(&self) -> String {
        let mut x = String::from("\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<DatabaseInformation Version=\"3\">\n");
        let mut el = |name: &str, value: &str| x.push_str(&format!("  <{name}>{}</{name}>\n", xml_escape(value)));
        el("ScriptFileEncoding", &self.encoding);
        el("DefaultCollation", &self.collation);
        el("DefaultSchema", &self.default_schema);
        el("DefaultUser", &self.default_user);
        el("DefaultFilegroup", &self.default_filegroup);
        el("DatabaseVersion", &self.database_version.to_string());
        if let Some(v) = &self.server_version {
            el("ServerVersion", v);
        }
        if let Some(c) = self.compatibility_level {
            el("CompatibilityLevel", &c.to_string());
            el("EngineEdition", self.engine_edition.as_deref().unwrap_or("Unknown"));
        }
        el("IsAzure", if self.is_azure { "True" } else { "False" });
        el("ScriptFolderType", "Default");
        el("MaxDataFileSize", &self.max_data_file_size.to_string());
        if let (Some(a), Some(q)) = (self.ansi_nulls, self.quoted_identifier) {
            el("AnsiNulls", if a { "On" } else { "Off" });
            el("QuotedIdentifier", if q { "On" } else { "Off" });
        }
        el("ProjectGuid", &self.project_guid);
        x.push_str("  <WriteToFileOptions>\n    <Prefixes>\n");
        for (name, _) in PREFIXES {
            let v = self.prefix(name);
            if v.is_empty() {
                x.push_str(&format!("      <{name}>\n      </{name}>\n"));
            } else {
                x.push_str(&format!("      <{name}>{}</{name}>\n", xml_escape(v)));
            }
        }
        x.push_str("    </Prefixes>\n    <DataWriteAllFilesInOneDirectory>True</DataWriteAllFilesInOneDirectory>\n  </WriteToFileOptions>\n");
        x.push_str(&format!("  <DataFileSet>\n    <Count>{}</Count>\n", self.data_files.len()));
        for f in &self.data_files {
            x.push_str(&format!("    <DataFile>{}</DataFile>\n", xml_escape(f)));
        }
        x.push_str("  </DataFileSet>\n</DatabaseInformation>");
        x.replace('\n', "\r\n")
    }
}

/// Text of a script file: BOMs win, then the folder's encoding; invalid UTF-8 is read as Windows-1252.
pub fn decode(bytes: &[u8], encoding: &str) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) }).collect();
        return String::from_utf16_lossy(&units);
    }
    if encoding.eq_ignore_ascii_case("UTF16") && bytes.len() > 1 && bytes[1] == 0 {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
    }
}

fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–',
        '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    if (0x80..0xA0).contains(&b) { HIGH[(b - 0x80) as usize] } else { b as char }
}

/// Bytes of a script file in the folder's encoding.
fn encode(text: &str, encoding: &str) -> Vec<u8> {
    match encoding.to_ascii_uppercase().as_str() {
        "UTF8_BOM" => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        "UTF16" => [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        "ASCII" => text.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect(),
        "WINDOWS1252" => text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect(),
        _ => text.as_bytes().to_vec(),
    }
}

/// A scripts folder read into the model.
pub struct Folder {
    pub path: PathBuf,
    pub info: FolderInfo,
    pub database: Database,
    pub messages: Vec<Message>,
    pub origins: Origins,
    /// Problems with RedGateDatabaseInfo.xml.
    pub warnings: Vec<String>,
}

/// The `.sql` files that hold objects: everything except the Data folder's top level and
/// `Custom Scripts`.
pub fn script_files(dir: &Path, info: &FolderInfo) -> Vec<PathBuf> {
    let data = dir.join(info.prefix("Data"));
    let custom = dir.join(CUSTOM_SCRIPTS);
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !name.starts_with('.') && !same_path(&p, &custom) {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sql")) {
                let in_data = !info.prefix("Data").is_empty() && same_path(&d, &data);
                let listed = info.data_files.iter().any(|f| f.eq_ignore_ascii_case(&name));
                if !in_data && !listed {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase().trim_end_matches(['\\', '/']) == b.to_string_lossy().to_lowercase().trim_end_matches(['\\', '/'])
}

/// Reads a scripts folder. The error is the message for a missing folder.
pub fn read(dir: &Path) -> Result<Folder, String> {
    if !dir.is_dir() {
        return Err(format!("The scripts folder '{}' could not be found.", dir.display()));
    }
    let (info, warnings) = FolderInfo::read(dir);
    let mut reader = parse::Reader::new(info.defaults());
    for file in script_files(dir, &info) {
        let bytes = std::fs::read(&file).map_err(|e| format!("An error occurred while reading from the scripts folder '{}': {e}", dir.display()))?;
        reader.add_file(&file, &decode(&bytes, &info.encoding));
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| dir.display().to_string());
    let result = reader.finish(&name);
    let mut database = result.database;
    database.compatibility_level = info.compatibility_level.unwrap_or(info.database_version * 10);
    database.server = ServerInfo {
        major_version: info.database_version,
        product_version: info.server_version.clone().unwrap_or_default(),
        edition: String::new(),
        engine_edition: info.engine_edition.as_deref().map(engine_edition_number).unwrap_or(if info.is_azure { 5 } else { 0 }),
    };
    Ok(Folder { path: dir.to_path_buf(), info, database, messages: result.messages, origins: result.origins, warnings })
}

/// The pre- and post-deployment scripts of a folder (`Custom Scripts\Pre-Deployment\*.sql`).
pub fn pre_post_scripts(dir: &Path, info: &FolderInfo) -> Result<(Option<String>, Option<String>), String> {
    let one = |sub: &str| -> Result<Option<String>, String> {
        let d = dir.join(CUSTOM_SCRIPTS).join(sub);
        let Ok(entries) = std::fs::read_dir(&d) else { return Ok(None) };
        let files: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sql"))).collect();
        match files.as_slice() {
            [] => Ok(None),
            [f] => Ok(Some(decode(&std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?, &info.encoding))),
            _ => Err(format!("{} holds more than one script; only one pre- or post-deployment script is supported", d.display())),
        }
    };
    Ok((one("Pre-Deployment")?, one("Post-Deployment")?))
}

/// Replaces characters Windows does not allow in file names.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if (c as u32) < 32 || "\"<>|:*?\\/".contains(c) { '_' } else { c }).collect();
    const RESERVED: [&str; 22] =
        ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    let first = s.split('.').next().unwrap_or_default().trim().to_string();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&first)) {
        let rest = s[s.find('.').unwrap_or(s.len())..].to_string();
        s = format!("{}_{rest}", s.split('.').next().unwrap_or_default());
    }
    s
}

/// `schema.name` file stem, made safe.
fn file_stem(schema: &str, name: &str) -> String {
    let base = if schema.is_empty() { sanitize(name) } else { format!("{}.{}", sanitize(schema), sanitize(name)) };
    let mut s = base;
    if s.starts_with('.') || s.starts_with(' ') {
        s.replace_range(..1, "_");
    }
    if s.ends_with('.') || s.ends_with(' ') {
        s.pop();
        s.push('_');
    }
    s
}

/// Where SQL Compare puts an object in a new folder, relative to the folder.
pub fn default_path(info: &FolderInfo, key: &ObjectKey) -> PathBuf {
    let (ty, schema, name) = key;
    let prefix = info.prefix(prefix_key(*ty)).replace('\\', std::path::MAIN_SEPARATOR_STR);
    let stem = file_stem(schema, name);
    Path::new(&prefix).join(format!("{stem}.sql"))
}

/// Roles that are part of every database; their files only list members.
const FIXED_ROLES: [&str; 9] =
    ["db_owner", "db_accessadmin", "db_securityadmin", "db_ddladmin", "db_backupoperator", "db_datareader", "db_datawriter", "db_denydatareader", "db_denydatawriter"];

/// Every object of `db` with its file content.
pub fn object_scripts(db: &Database, info: &FolderInfo, o: &Options) -> Vec<(ObjectKey, String)> {
    let fg = info.default_filegroup.as_str();
    let mut out: Vec<(ObjectKey, String)> = Vec::new();
    let key = |ty, s: &str, n: &str| (FileKind::Object(ty), s.to_string(), n.to_string());
    for s in db.schemas.iter().filter(|s| s.name != "dbo") {
        out.push((key(ObjectType::Schema, "", &s.name), script::schema(db, s, o)));
    }
    let roles: Vec<&Principal> = db.principals.iter().filter(|p| p.is_role()).collect();
    for p in &roles {
        out.push((key(ObjectType::Role, "", &p.name), script::role(db, p, o)));
    }
    for r in FIXED_ROLES {
        let text = script::fixed_role(db, r);
        if !text.is_empty() {
            out.push((key(ObjectType::Role, "", r), text));
        }
    }
    if !o.has(crate::schema::Opt::IgnoreUsers) {
        for p in db.principals.iter().filter(|p| !p.is_role()) {
            out.push((key(ObjectType::User, "", &p.name), script::user(db, p, o)));
        }
    }
    for t in &db.types {
        out.push((key(ObjectType::UserDefinedType, &t.schema, &t.name), script::user_type(db, t, o)));
    }
    for t in db.tables.iter().filter(|t| !t.history_table) {
        out.push((key(ObjectType::Table, &t.schema, &t.name), script::table(db, t, o, fg)));
    }
    for v in &db.views {
        out.push((key(ObjectType::View, &v.schema, &v.name), script::view(db, v, o, fg)));
    }
    for r in &db.routines {
        let ty = if matches!(r.kind, RoutineKind::Procedure | RoutineKind::ClrProcedure) { ObjectType::StoredProcedure } else { ObjectType::Function };
        out.push((key(ty, &r.schema, &r.name), script::routine(db, r, o)));
    }
    for s in &db.sequences {
        out.push((key(ObjectType::Sequence, &s.schema, &s.name), script::sequence(db, s, o)));
    }
    for s in &db.synonyms {
        out.push((key(ObjectType::Synonym, &s.schema, &s.name), script::synonym(db, s, o)));
    }
    if !o.has(crate::schema::Opt::IgnoreExtendedProperties) {
        for p in db.extended_properties.iter().filter(|p| p.class == "DATABASE") {
            out.push((key(ObjectType::ExtendedProperty, "", &p.name), script::database_property(p)));
        }
    }
    for t in db.ddl_triggers.iter().filter(|t| !t.definition.is_empty()) {
        out.push(((FileKind::DdlTrigger, String::new(), t.name.clone()), script::ddl_trigger(t)));
    }
    out.extend(super::objects::object_files(db, o));
    out
}

/// Chooses a file for every object: its current file, else the default path (numbered when
/// another object already has that file).
fn assign_paths(keys: &[ObjectKey], info: &FolderInfo, origins: &HashMap<ObjectKey, PathBuf>) -> HashMap<ObjectKey, PathBuf> {
    let mut used: HashMap<String, ObjectKey> = HashMap::new();
    let mut out = HashMap::new();
    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    for k in keys {
        if let Some(p) = origins.get(k) {
            used.insert(lower(p), k.clone());
            out.insert(k.clone(), p.clone());
        }
    }
    for k in keys {
        if out.contains_key(k) {
            continue;
        }
        let ideal = default_path(info, k);
        let stem = ideal.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let parent = ideal.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut n = 0;
        let path = loop {
            let p = if n == 0 { ideal.clone() } else { parent.join(format!("{stem}{n}.sql")) };
            if !used.contains_key(&lower(&p)) {
                break p;
            }
            n += 1;
        };
        used.insert(lower(&path), k.clone());
        out.insert(k.clone(), path);
    }
    out
}

/// `/makescripts`: writes every object of `db` into `dir` (created when missing) with a new
/// RedGateDatabaseInfo.xml. Returns the number of object files written.
pub fn write_all(db: &Database, dir: &Path, o: &Options) -> Result<usize, String> {
    let mut info = FolderInfo::for_database(db);
    if let (old @ FolderInfo { file: Some(_), .. }, _) = FolderInfo::read(dir) {
        // Keep the folder's own layout and identity.
        info.prefixes = old.prefixes;
        info.project_guid = old.project_guid;
        info.encoding = old.encoding;
    }
    let scripts = object_scripts(db, &info, o);
    let keys: Vec<ObjectKey> = scripts.iter().map(|(k, _)| k.clone()).collect();
    let paths = assign_paths(&keys, &info, &HashMap::new());
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (k, text) in &scripts {
        let p = dir.join(&paths[k]);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&p, encode(text, &info.encoding)).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let info_path = dir.join(INFO_FILE);
    std::fs::write(&info_path, info.xml()).map_err(|e| format!("Failed to write database information file: {}. Original error message: {e}", info_path.display()))?;
    Ok(scripts.len())
}

/// What deploying to a scripts folder changed (paths relative to the folder).
#[derive(Debug, Default)]
pub struct FolderChanges {
    pub written: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

/// The target model after deploying the selected differences: each selected object is taken
/// from `source` (or removed when only the target has it), with its permissions and
/// extended properties.
pub fn apply(source: &Database, target: &Database, differences: &[Difference]) -> Database {
    let mut db = target.clone();
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    for d in differences.iter().filter(|d| d.selected && d.kind != DiffKind::Equal) {
        let n1 = &d.name;
        let n2 = d.target_name.as_ref().unwrap_or(&d.name);
        let from_source = d.kind != DiffKind::OnlyIn2;
        macro_rules! swap {
            ($field:ident) => {{
                db.$field.retain(|x| !(eq(&x.schema, &n2.schema) && eq(&x.name, &n2.name)));
                if from_source {
                    if let Some(x) = source.$field.iter().find(|x| eq(&x.schema, &n1.schema) && eq(&x.name, &n1.name)) {
                        let mut x = x.clone();
                        x.schema = n2.schema.clone();
                        x.name = n2.name.clone();
                        db.$field.push(x);
                    }
                }
            }};
        }
        let object_extras = |db: &mut Database| {
            db.permissions.retain(|p| !(p.class == "OBJECT" && p.schema.as_deref().is_some_and(|s| eq(s, &n2.schema)) && p.object.as_deref().is_some_and(|o| eq(o, &n2.name))));
            db.extended_properties.retain(|p| !(p.class != "DATABASE" && p.class != "SCHEMA" && p.schema.as_deref().is_some_and(|s| eq(s, &n2.schema)) && p.object.as_deref().is_some_and(|o| eq(o, &n2.name))));
            if from_source {
                for p in source.permissions.iter().filter(|p| p.class == "OBJECT" && p.schema.as_deref().is_some_and(|s| eq(s, &n1.schema)) && p.object.as_deref().is_some_and(|o| eq(o, &n1.name))) {
                    let mut p = p.clone();
                    p.schema = Some(n2.schema.clone());
                    p.object = Some(n2.name.clone());
                    db.permissions.push(p);
                }
                for p in source.extended_properties.iter().filter(|p| p.class != "DATABASE" && p.class != "SCHEMA" && p.schema.as_deref().is_some_and(|s| eq(s, &n1.schema)) && p.object.as_deref().is_some_and(|o| eq(o, &n1.name))) {
                    let mut p = p.clone();
                    p.schema = Some(n2.schema.clone());
                    p.object = Some(n2.name.clone());
                    db.extended_properties.push(p);
                }
            }
        };
        match d.object_type {
            ObjectType::Table => {
                swap!(tables);
                object_extras(&mut db);
            }
            ObjectType::View => {
                swap!(views);
                object_extras(&mut db);
            }
            ObjectType::Function | ObjectType::StoredProcedure => {
                swap!(routines);
                object_extras(&mut db);
            }
            ObjectType::UserDefinedType => {
                swap!(types);
                object_extras(&mut db);
            }
            ObjectType::Sequence => {
                swap!(sequences);
                object_extras(&mut db);
            }
            ObjectType::Synonym => {
                swap!(synonyms);
                object_extras(&mut db);
            }
            ObjectType::Schema => {
                let name = &n2.name;
                db.schemas.retain(|s| !eq(&s.name, name));
                db.permissions.retain(|p| !(p.class == "SCHEMA" && p.schema.as_deref().is_some_and(|s| eq(s, name))));
                db.extended_properties.retain(|p| !(p.class == "SCHEMA" && p.schema.as_deref().is_some_and(|s| eq(s, name))));
                if from_source {
                    if let Some(s) = source.schemas.iter().find(|s| eq(&s.name, &n1.name)) {
                        db.schemas.push(Schema { name: name.clone(), owner: s.owner.clone() });
                    }
                    for p in source.permissions.iter().filter(|p| p.class == "SCHEMA" && p.schema.as_deref().is_some_and(|s| eq(s, &n1.name))) {
                        db.permissions.push(Permission { schema: Some(name.clone()), ..p.clone() });
                    }
                    for p in source.extended_properties.iter().filter(|p| p.class == "SCHEMA" && p.schema.as_deref().is_some_and(|s| eq(s, &n1.name))) {
                        db.extended_properties.push(ExtendedProperty { schema: Some(name.clone()), ..p.clone() });
                    }
                }
            }
            ObjectType::Role | ObjectType::User => {
                let name = &n2.name;
                db.principals.retain(|p| !eq(&p.name, name));
                db.permissions.retain(|p| !(p.class == "DATABASE" && eq(&p.grantee, name)));
                if from_source {
                    if let Some(p) = source.principals.iter().find(|p| eq(&p.name, &n1.name)) {
                        db.principals.push(Principal { name: name.clone(), ..p.clone() });
                    }
                    for p in source.permissions.iter().filter(|p| p.class == "DATABASE" && eq(&p.grantee, &n1.name)) {
                        db.permissions.push(Permission { grantee: name.clone(), ..p.clone() });
                    }
                }
                if d.kind == DiffKind::OnlyIn2 {
                    for p in &mut db.principals {
                        p.member_of.retain(|r| !eq(r, name));
                    }
                }
            }
            ObjectType::ExtendedProperty => {
                db.extended_properties.retain(|p| !(p.class == "DATABASE" && eq(&p.name, &n2.name)));
                if from_source && let Some(p) = source.extended_properties.iter().find(|p| p.class == "DATABASE" && eq(&p.name, &n1.name)) {
                    db.extended_properties.push(p.clone());
                }
            }
            other => super::objects::apply(&mut db, source, other, n1, n2, from_source),
        }
    }
    db.permissions.sort();
    db
}

/// Deploys the selected differences to the scripts folder `target`: rewrites the files of
/// objects that change, adds files for new objects and deletes files left without objects.
pub fn sync(source: &Database, target: &Folder, differences: &[Difference], o: &Options) -> Result<FolderChanges, String> {
    let after = apply(source, &target.database, differences);
    let before: HashMap<ObjectKey, String> = object_scripts(&target.database, &target.info, o).into_iter().collect();
    let after_scripts = object_scripts(&after, &target.info, o);
    let after_map: HashMap<ObjectKey, String> = after_scripts.iter().cloned().collect();
    // Current files, relative to the folder.
    let origins: HashMap<ObjectKey, PathBuf> = target
        .origins
        .iter()
        .map(|(k, p)| (k.clone(), p.strip_prefix(&target.path).unwrap_or(p).to_path_buf()))
        .collect();
    let mut keys: Vec<ObjectKey> = after_scripts.iter().map(|(k, _)| k.clone()).collect();
    for k in before.keys() {
        if !after_map.contains_key(k) {
            keys.push(k.clone());
        }
    }
    let paths = assign_paths(&keys, &target.info, &origins);
    let mut dirty: Vec<PathBuf> = Vec::new();
    for k in &keys {
        if before.get(k) != after_map.get(k) {
            let p = &paths[k];
            if !dirty.contains(p) {
                dirty.push(p.clone());
            }
        }
    }
    dirty.sort();
    let mut changes = FolderChanges::default();
    for rel in dirty {
        // Everything that lives in this file after the deployment, in model order.
        let text: String = after_scripts.iter().filter(|(k, _)| paths.get(k) == Some(&rel)).map(|(_, t)| t.as_str()).collect();
        let full = target.path.join(&rel);
        if text.is_empty() {
            if full.exists() {
                std::fs::remove_file(&full).map_err(|e| format!("{}: {e}", full.display()))?;
                changes.deleted.push(rel);
            }
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&full)
            && meta.permissions().readonly()
        {
            return Err("Some of the scripts in the folder being synchronized to are read only.".into());
        }
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&full, encode(&text, &target.info.encoding)).map_err(|e| format!("{}: {e}", full.display()))?;
        changes.written.push(rel);
    }
    if target.info.file.is_none() {
        let mut info = FolderInfo::for_database(&target.database);
        info.collation = target.info.collation.clone();
        std::fs::write(target.path.join(INFO_FILE), info.xml()).map_err(|e| format!("{}: {e}", target.path.display()))?;
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_round_trip() {
        let mut info = FolderInfo { collation: "Latin1_General_CS_AS".into(), compatibility_level: Some(150), engine_edition: Some("Standard".into()), ..Default::default() };
        info.prefixes.insert("Table".into(), "My Tables".into());
        let dir = std::env::temp_dir().join(format!("quill-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(INFO_FILE), info.xml()).unwrap();
        let (back, warnings) = FolderInfo::read(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.collation, "Latin1_General_CS_AS");
        assert_eq!(back.compatibility_level, Some(150));
        assert_eq!(back.prefix("Table"), "My Tables");
        assert_eq!(back.prefix("None"), "");
        assert_eq!(back.project_guid, info.project_guid);
        assert!(info.xml().contains("      <None>\r\n      </None>\r\n"));
    }

    #[test]
    fn file_names() {
        let info = FolderInfo::default();
        let p = default_path(&info, &(FileKind::Object(ObjectType::StoredProcedure), "Sales".into(), "Get/Orders?".into()));
        assert_eq!(default_path(&info, &(FileKind::DdlTrigger, String::new(), "Audit".into())), Path::new("Database Triggers").join("Audit.sql"));
        assert_eq!(p, Path::new("Stored Procedures").join("Sales.Get_Orders_.sql"));
        assert_eq!(sanitize("CON"), "CON_");
        assert_eq!(file_stem("", "x."), "x_");
        let table = FileKind::Object(ObjectType::Table);
        let keys = vec![(table, "a".into(), "b.c".into()), (table, "a.b".into(), "c".into())];
        let paths = assign_paths(&keys, &info, &HashMap::new());
        assert_ne!(paths[&keys[0]], paths[&keys[1]]);
        assert!(paths[&keys[1]].to_string_lossy().ends_with("a.b.c1.sql"));
    }

    #[test]
    fn decodes_files() {
        assert_eq!(decode(&[0xEF, 0xBB, 0xBF, b'a'], "UTF8"), "a");
        assert_eq!(decode(&[0xFF, 0xFE, b'a', 0], "UTF8"), "a");
        assert_eq!(decode(&[b'a', 0xE9], "UTF8"), "aé");
    }
}
