//! Compare projects: `.qsc` (schema) and `.qdc` (data) files. JSON, readable in a diff, and
//! never holding a password: SQL logins keep only a "remember" flag, the password itself goes
//! to Windows Credential Manager (written by the UI) or comes from the environment.
//!
//! SQL Compare `.scp` and SQL Data Compare `.sdc` projects can be imported.

use serde::{Deserialize, Serialize};

use crate::catalog::ObjectName;
use crate::data::mapping::{DataMappings, TableOverride};
use crate::db::{Auth, ConnectionSpec};
use crate::schema::compare::{Mappings, TableMapping};

pub const PROJECT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    #[default]
    Schema,
    Data,
}

impl ProjectKind {
    pub fn extension(self) -> &'static str {
        match self {
            ProjectKind::Schema => "qsc",
            ProjectKind::Data => "qdc",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Authentication {
    #[default]
    Windows,
    Sql,
    /// Microsoft Entra: an access token supplied at run time.
    Entra,
}

/// One side of a comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SourceSpec {
    /// `database` or `snapshot`.
    #[serde(rename = "type")]
    pub kind: String,
    pub server: String,
    pub database: String,
    pub authentication: Authentication,
    pub user: String,
    /// Keep the password in Windows Credential Manager.
    pub remember_password: bool,
    pub encrypt: bool,
    pub trust_server_certificate: bool,
    /// Snapshot file for `type = snapshot`, folder for `type = scripts`.
    pub path: String,
    /// `type = scripts`: the source-control revision of the folder to read (HEAD for the latest).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub revision: String,
    /// `type = backup`: the name of the backup set to read (the latest when empty); the files
    /// are in `path`, separated by `;`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub backup_set: String,
    /// Run time only: never written to the file.
    #[serde(skip_serializing)]
    pub password: Option<String>,
    #[serde(skip_serializing)]
    pub access_token: Option<String>,
}

impl SourceSpec {
    pub fn database(server: &str, database: &str) -> SourceSpec {
        SourceSpec { kind: "database".into(), server: server.into(), database: database.into(), encrypt: true, ..Default::default() }
    }

    pub fn is_snapshot(&self) -> bool {
        self.kind == "snapshot"
    }

    /// A scripts folder (SQL Compare `FolderDataSource`), at `path`.
    pub fn is_scripts(&self) -> bool {
        self.kind == "scripts"
    }

    /// Backup files (SQL Compare `BackupDatabaseSource`), listed in `path`.
    pub fn is_backup(&self) -> bool {
        self.kind == "backup"
    }

    /// An empty database (`/empty2`).
    pub fn is_empty_database(&self) -> bool {
        self.kind == "empty"
    }

    /// `server / database` or the snapshot path.
    pub fn describe(&self) -> String {
        if self.is_snapshot() || self.is_scripts() || self.is_backup() { self.path.clone() } else { format!("{} / {}", self.server, self.database) }
    }

    /// The connection, with the password from the spec or from `QUILL_PASSWORD<n>`.
    pub fn connection(&self, n: u8) -> Result<ConnectionSpec, String> {
        let auth = match self.authentication {
            Authentication::Windows => Auth::Windows,
            Authentication::Sql => {
                let password = self
                    .password
                    .clone()
                    .or_else(|| std::env::var(format!("QUILL_PASSWORD{n}")).ok())
                    .ok_or_else(|| format!("no password for {} on {}; set QUILL_PASSWORD{n} or give /password{n}", self.user, self.server))?;
                Auth::Sql { user: self.user.clone(), password }
            }
            Authentication::Entra => Auth::AccessToken(
                self.access_token
                    .clone()
                    .or_else(|| std::env::var(format!("QUILL_ACCESS_TOKEN{n}")).ok())
                    .ok_or_else(|| format!("no Microsoft Entra access token for {}; set QUILL_ACCESS_TOKEN{n}", self.server))?,
            ),
        };
        let mut spec = ConnectionSpec::new(&self.server, &self.database, auth);
        spec.encrypt = self.encrypt;
        spec.trust_server_certificate = self.trust_server_certificate;
        Ok(spec)
    }
}

fn is_disabled(b: &crate::schema::backup::BackupOptions) -> bool {
    !b.enabled
}

/// An object left out of deployment in the results grid.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Deselected {
    /// Object type label (schema projects) or `Table` (data projects).
    #[serde(rename = "type")]
    pub kind: String,
    pub schema: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Project {
    pub version: u32,
    pub kind: ProjectKind,
    pub source: SourceSpec,
    pub target: SourceSpec,
    /// Option names (schema: `schema::Opt`, data: `data::DataOpt`).
    pub options: Vec<String>,
    pub schema_mappings: Mappings,
    pub data_mappings: DataMappings,
    /// `/include` and `/exclude` rules.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub deselected: Vec<Deselected>,
    /// Back up target before deployment (SQL Compare's `SC_DeploymentOptions/BackupOptions`).
    #[serde(skip_serializing_if = "is_disabled")]
    pub backup: crate::schema::backup::BackupOptions,
    pub note: String,
    /// The filter of an imported SQL Compare project (its `ProjectFilter` XML).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

impl Project {
    pub fn new(kind: ProjectKind) -> Project {
        let options = match kind {
            ProjectKind::Schema => crate::schema::Options::default_set().iter().map(|o| o.name().to_string()).collect(),
            ProjectKind::Data => crate::data::DataOptions::default_set().iter().map(|o| o.name().to_string()).collect(),
        };
        Project {
            version: PROJECT_VERSION,
            kind,
            source: SourceSpec::database("", ""),
            target: SourceSpec::database("", ""),
            options,
            ..Default::default()
        }
    }

    pub fn load(path: &std::path::Path) -> Result<Project, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if ext == "scp" || ext == "sdc" {
            return import_vendor(&bytes, if ext == "sdc" { ProjectKind::Data } else { ProjectKind::Schema });
        }
        let mut p: Project = serde_json::from_slice(without_bom(&bytes)).map_err(|e| format!("{} is not a QuillSQL project: {e}", path.display()))?;
        if p.version > PROJECT_VERSION {
            return Err(format!("{} was saved by a newer QuillSQL; update QuillSQL to open it", path.display()));
        }
        if p.version == 0 {
            p.version = PROJECT_VERSION;
        }
        Ok(p)
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Write next to it and rename, so a failed write never leaves half a project.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Text of a file in UTF-16 (with or without BOM) or UTF-8.
pub fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) || (bytes.len() > 1 && bytes[1] == 0 && bytes[0] != 0) {
        let start = if bytes.starts_with(&[0xFF, 0xFE]) { 2 } else { 0 };
        let units: Vec<u16> = bytes[start..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    let start = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Splits `[schema].[name]` (brackets optional).
fn split_name(s: &str) -> Option<ObjectName> {
    let s = s.trim();
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_bracket = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' if !in_bracket => in_bracket = true,
            ']' if in_bracket => {
                if chars.peek() == Some(&']') {
                    chars.next();
                    cur.push(']');
                } else {
                    in_bracket = false;
                }
            }
            '.' if !in_bracket => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    match parts.as_slice() {
        [schema, name] => Some(ObjectName::new(schema, name)),
        [name] if !name.is_empty() => Some(ObjectName::new("dbo", name)),
        _ => None,
    }
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn text_of(node: roxmltree::Node<'_, '_>, name: &str) -> String {
    child(node, name).and_then(|c| c.text()).unwrap_or_default().trim().to_string()
}

/// Reads a SQL Compare (.scp) or SQL Data Compare (.sdc) project. Passwords are not imported.
pub fn import_vendor(bytes: &[u8], kind: ProjectKind) -> Result<Project, String> {
    let text = decode_text(bytes);
    let doc = roxmltree::Document::parse(&text).map_err(|e| format!("not a Vendor project file: {e}"))?;
    let root = doc.root_element();
    let mut p = Project::new(kind);

    let source = |name: &str| -> SourceSpec {
        let Some(n) = child(root, name) else { return SourceSpec::default() };
        let ty = n.attribute("type").unwrap_or_default();
        if ty.eq_ignore_ascii_case("SnapshotSource") {
            return SourceSpec { kind: "snapshot".into(), path: text_of(n, "FileName"), ..Default::default() };
        }
        if ty.eq_ignore_ascii_case("FolderDataSource") {
            return SourceSpec { kind: "scripts".into(), path: text_of(n, "Path"), ..Default::default() };
        }
        if ty.eq_ignore_ascii_case("EmptyDatabaseSource") {
            return SourceSpec { kind: "empty".into(), ..Default::default() };
        }
        if ty.eq_ignore_ascii_case("BackupDatabaseSource") {
            let files: Vec<String> = child(n, "Files").map(|f| f.children().filter(|c| c.is_element()).filter_map(|c| c.text()).map(|t| t.trim().to_string()).collect()).unwrap_or_default();
            let backup_set = child(n, "BackupSet").map(|b| text_of(b, "BackupSetName")).unwrap_or_default();
            return SourceSpec { kind: "backup".into(), path: files.join(";"), backup_set, ..Default::default() };
        }
        let mut s = SourceSpec::database(&text_of(n, "ServerName"), &text_of(n, "DatabaseName"));
        let auth = text_of(n, "AuthenticationType");
        let user = text_of(n, "Username");
        s.authentication = if auth.to_ascii_lowercase().contains("sqlpassword") || (!user.is_empty() && auth.is_empty()) {
            Authentication::Sql
        } else if auth.to_ascii_lowercase().contains("activedirectory") || auth.to_ascii_lowercase().contains("entra") {
            Authentication::Entra
        } else {
            Authentication::Windows
        };
        s.user = user;
        s.remember_password = text_of(n, "SavePassword").eq_ignore_ascii_case("true");
        if let Some(e) = child(n, "EncryptConnection").and_then(|c| c.text()) {
            s.encrypt = e.trim().eq_ignore_ascii_case("true");
        }
        s.trust_server_certificate = text_of(n, "TrustServerCertificate").eq_ignore_ascii_case("true");
        s
    };
    p.source = source("DataSource1");
    p.target = source("DataSource2");
    if text_of(root, "Direction") == "1" {
        std::mem::swap(&mut p.source, &mut p.target);
    }

    match kind {
        ProjectKind::Schema => {
            let options = text_of(root, "Options");
            if !options.is_empty() {
                // Group names (Default, NoSQLPlumbing...) expand; unknown names are dropped.
                let names: Vec<&str> = options.split(',').map(str::trim).filter(|n| crate::schema::Options::parse(n).is_ok()).collect();
                if let Ok(o) = crate::schema::Options::parse(&names.join(",")) {
                    p.options = o.iter().map(|x| x.name().to_string()).collect();
                }
            }
        }
        ProjectKind::Data => {
            let int = |name: &str| text_of(root, name).parse::<u64>().ok();
            let mut opts = Vec::new();
            for (value, table) in [(int("MappingOptions").or(Some(0x52)), MAPPING_BITS), (int("ComparisonOptions"), COMPARISON_BITS), (int("SQLOptions").or(Some(0xA)), SQL_BITS)] {
                if let Some(v) = value {
                    opts.extend(table.iter().filter(|(bit, _)| v & bit != 0).map(|(_, o)| o.name().to_string()));
                }
            }
            if int("SessionSettings").is_some_and(|s| s & 0x1 != 0) {
                opts.push(DataOpt::IncludeIdenticalRecords.name().to_string());
            }
            p.options = opts;
        }
    }

    // Mapping actions.
    if let Some(actions) = child(root, "TableActions") {
        for ev in actions.children().filter(|c| c.is_element()) {
            let action = text_of(ev, "action");
            let side = |s: &str| -> (Option<ObjectName>, Option<ObjectName>) {
                let (a, b) = s.split_once(':').unwrap_or((s, "null"));
                let n = |x: &str| if x == "null" { None } else { split_name(x) };
                (n(a), n(b))
            };
            match action.as_str() {
                "MapTables" => {
                    let (a, _) = side(&text_of(ev, "Table1"));
                    let (_, b) = side(&text_of(ev, "Table2"));
                    if let (Some(a), Some(b)) = (a, b) {
                        match kind {
                            ProjectKind::Schema => p.schema_mappings.tables.push(TableMapping { source: a, target: b, columns: Vec::new() }),
                            ProjectKind::Data => {
                                p.data_mappings.tables.push(TableOverride { source: a, target: Some(b), ..Default::default() })
                            }
                        }
                    }
                }
                "MapOwners" => match kind {
                    ProjectKind::Schema => p.schema_mappings.schemas.push((text_of(ev, "Owner1"), text_of(ev, "Owner2"))),
                    ProjectKind::Data => p.data_mappings.schemas.push((text_of(ev, "Owner1"), text_of(ev, "Owner2"))),
                },
                "MapColumn" => {
                    let (Some(t1), c1, c2) = (split_name(&text_of(ev, "Table1")), text_of(ev, "Column1"), text_of(ev, "Column2")) else { continue };
                    if kind == ProjectKind::Data {
                        entry(&mut p.data_mappings, &t1).columns.push((c1, c2));
                    } else if let Some(m) = p.schema_mappings.tables.iter_mut().find(|m| m.source == t1) {
                        m.columns.push((c1, c2));
                    }
                }
                "DeselectItem" => {
                    let (a, b) = side(&text_of(ev, "val"));
                    if let Some(n) = a.or(b) {
                        if kind == ProjectKind::Data {
                            entry(&mut p.data_mappings, &n).excluded = true;
                        } else {
                            p.deselected.push(Deselected { kind: String::new(), schema: n.schema, name: n.name });
                        }
                    }
                }
                "UseKey" => {
                    if let Some(t) = split_name(&text_of(ev, "TableName")) {
                        entry(&mut p.data_mappings, &t).key = vec![format!("index:{}", text_of(ev, "IndexName"))];
                    }
                }
                "SelectColumnAsKey" => {
                    if let Some(t) = split_name(&text_of(ev, "TableName")) {
                        let e = entry(&mut p.data_mappings, &t);
                        e.key.retain(|k| !k.starts_with("index:"));
                        e.key.push(text_of(ev, "ColumnName"));
                    }
                }
                "DeselectColumn" => {
                    if let Some(t) = split_name(&text_of(ev, "TableName")) {
                        entry(&mut p.data_mappings, &t).excluded_columns.push(text_of(ev, "ColumnName"));
                    }
                }
                "SetLeftWhereClause" | "SetRightWhereClause" => {
                    if let Some(t) = split_name(&text_of(ev, "TableName")) {
                        let w = text_of(ev, "IndexName");
                        let e = entry(&mut p.data_mappings, &t);
                        if action.starts_with("SetLeft") { e.where1 = Some(w) } else { e.where2 = Some(w) }
                    }
                }
                _ => {}
            }
        }
    }
    p.note = text_of(root, "UserNote");
    // Keep a filter that leaves anything out; the default one includes everything.
    if kind == ProjectKind::Schema
        && let Some(f) = child(root, "ProjectFilter")
        && f.descendants().any(|e| e.tag_name().name() == "Expression" && e.text().is_none_or(|t| t.trim() != "TRUE"))
    {
        p.filter = Some(text[f.range()].to_string());
    }
    // Back up target before deployment; the password is not imported.
    if let Some(b) = child(root, "SC_DeploymentOptions").and_then(|d| child(d, "BackupOptions")).filter(|b| b.attribute("isNull") != Some("1")) {
        let truth = |name: &str| text_of(b, name).eq_ignore_ascii_case("true");
        let number = |name: &str| text_of(b, name).trim().parse::<u8>().unwrap_or(0);
        p.backup = crate::schema::backup::BackupOptions {
            enabled: truth("BackupEnabled"),
            provider: if text_of(b, "BackupProvider").eq_ignore_ascii_case("Sqb") { "sqb".into() } else { "native".into() },
            differential: text_of(b, "TypeOfBackup").eq_ignore_ascii_case("Differential"),
            folder: text_of(b, "Folder"),
            file: text_of(b, "Filename"),
            name_automatically: truth("NameFileAutomatically"),
            overwrite: truth("OverwriteIfExists"),
            compression_level: number("CompressionLevel"),
            encryption: text_of(b, "EncryptionLevel").to_ascii_lowercase(),
            thread_count: number("ThreadCount"),
            password: None,
        };
        if p.backup.encryption.is_empty() {
            p.backup.encryption = "none".into();
        }
    }
    Ok(p)
}

use crate::data::DataOpt;

/// Bits of SQL Data Compare's MappingOptions, ComparisonOptions and SQLOptions project values.
const MAPPING_BITS: &[(u64, DataOpt)] = &[
    (0x1, DataOpt::CaseSensitiveObjectDefinition),
    (0x2, DataOpt::IgnoreSpaces),
    (0x4, DataOpt::IgnoreUnderscores),
    (0x10, DataOpt::IncludeTimestamps),
    (0x20, DataOpt::IncludeViews),
    (0x40, DataOpt::IncludeIdentities),
    (0x200, DataOpt::IncludeGeneratedAlwaysColumns),
];
const COMPARISON_BITS: &[(u64, DataOpt)] = &[
    (0x1, DataOpt::TrimTrailingSpaces),
    (0x2, DataOpt::ForceBinaryCollation),
    (0x4, DataOpt::UseChecksumComparison),
    (0x8, DataOpt::CompressTemporaryFiles),
    (0x10, DataOpt::TreatEmptyStringAsNull),
    (0x20, DataOpt::UseFullPrecisionForComparison),
];
const SQL_BITS: &[(u64, DataOpt)] = &[
    (0x1, DataOpt::DisableTriggers),
    (0x2, DataOpt::DisableKeys),
    (0x4, DataOpt::DropConstraintsAndIndexes),
    (0x8, DataOpt::OutputCommentHeader),
    (0x10, DataOpt::DontUseTransactions),
    (0x20, DataOpt::TransportClrBinary),
    (0x40, DataOpt::DisableDdlTriggers),
    (0x80, DataOpt::DontOutputComments),
    (0x100, DataOpt::ReseedIdentity),
    (0x400, DataOpt::SkipFkChecks),
    (0x800, DataOpt::AddDatabaseUseStatement),
];

/// Indented XML written the way SQL Compare's serializer writes it.
struct XmlOut {
    out: String,
    depth: usize,
}

impl XmlOut {
    fn escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    fn line(&mut self, s: &str) {
        self.out.push_str("\r\n");
        self.out.push_str(&"  ".repeat(self.depth));
        self.out.push_str(s);
    }

    fn value(&mut self, name: &str, value: &str) {
        if value.is_empty() {
            self.line(&format!("<{name} />"));
        } else {
            self.line(&format!("<{name}>{}</{name}>", Self::escape(value)));
        }
    }

    fn open(&mut self, name: &str, attrs: &str) {
        self.line(&format!("<{name}{attrs}>"));
        self.depth += 1;
    }

    fn close(&mut self, name: &str) {
        self.depth -= 1;
        self.line(&format!("</{name}>"));
    }
}

fn quote_name(n: &ObjectName) -> String {
    format!("[{}].[{}]", n.schema.replace(']', "]]"), n.name.replace(']', "]]"))
}

fn bool_text(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Writes a SQL Compare (.scp) or SQL Data Compare (.sdc) project: UTF-16 with a byte order
/// mark. Passwords are never written. `last_compared` is in .NET's invariant format.
pub fn export_vendor(p: &Project, last_compared: &str) -> Vec<u8> {
    let mut x = XmlOut { out: "<?xml version=\"1.0\" encoding=\"utf-16\" standalone=\"yes\"?>".into(), depth: 0 };
    x.line(&format!("<!--\r\nQuillSQL Compare\r\nVersion:{}-->", env!("CARGO_PKG_VERSION")));
    x.open("Project", " version=\"5\" type=\"SQLComparisonToolsProject\"");
    for (element, side) in [("DataSource1", &p.source), ("DataSource2", &p.target)] {
        if side.is_snapshot() {
            x.open(element, " version=\"1\" type=\"SnapshotSource\"");
            x.value("FileName", &side.path);
            x.close(element);
        } else if side.is_backup() {
            // The backup set is named; SQL Compare finds it again among the files' sets.
            x.open(element, " version=\"1\" type=\"BackupDatabaseSource\"");
            x.open("Files", " type=\"ArrayList\" version=\"1\"");
            for f in side.path.split(';').filter(|f| !f.is_empty()) {
                x.line(&format!("<value type=\"string\">{}</value>", XmlOut::escape(f)));
            }
            x.close("Files");
            x.open("BackupSet", " version=\"5\" type=\"ProjectBackupSet\"");
            x.value("BackupSetName", &side.backup_set);
            for name in ["DatabaseName", "ServerName"] {
                x.value(name, "");
            }
            x.value("StartDate", "01/01/0001 00:00:00");
            x.value("BackupType", "0");
            for name in ["BindingID", "DifferentialBaseGuid", "FamilyID"] {
                x.value(name, "00000000-0000-0000-0000-000000000000");
            }
            x.value("RaidSequenceNumber", "1");
            for name in ["FirstLsn", "LastLsn", "CheckpointLsn", "FullLsn"] {
                x.value(name, "");
            }
            x.value("Size", "0");
            x.value("Encrypted", "False");
            x.value("Incomplete", "False");
            x.close("BackupSet");
            x.close(element);
        } else if side.is_scripts() {
            x.open(element, " version=\"2\" type=\"FolderDataSource\"");
            x.value("Path", &side.path);
            let name = std::path::Path::new(&side.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            x.value("DatabaseName", &name);
            x.value("ServerName", "Scripts");
            x.close(element);
        } else if side.is_empty_database() || (side.server.is_empty() && side.database.is_empty()) {
            x.line(&format!("<{element} version=\"1\" type=\"EmptyDatabaseSource\" />"));
        } else {
            x.open(element, " version=\"5\" type=\"LiveDatabaseSource\"");
            x.value("ServerName", &side.server);
            x.value("DatabaseName", &side.database);
            x.value("Username", if side.authentication == Authentication::Sql { &side.user } else { "" });
            x.value("SavePassword", "False");
            x.value("EncryptConnection", bool_text(side.encrypt));
            x.value("TrustServerCertificate", bool_text(side.trust_server_certificate));
            x.value("Password", "");
            x.value("ScriptFolderLocation", "");
            x.value("MigrationsFolderLocation", "");
            x.value(
                "AuthenticationType",
                match side.authentication {
                    Authentication::Windows => "WindowsIntegrated",
                    Authentication::Sql => "SqlPassword",
                    Authentication::Entra => "MicrosoftEntraIntegrated",
                },
            );
            x.close(element);
        }
    }
    x.value("LastCompared", last_compared);
    let schema_options = match p.kind {
        ProjectKind::Schema => p.options.join(", "),
        ProjectKind::Data => crate::schema::Options::default_set().iter().map(|o| o.name()).collect::<Vec<_>>().join(", "),
    };
    x.value("Options", &schema_options);
    x.value("InRecycleBin", "False");
    x.value("Direction", "0");
    match &p.filter {
        Some(f) if p.kind == ProjectKind::Schema => x.line(f),
        _ => x.line("<ProjectFilter version=\"1\" type=\"SQLCompareDifferenceFilter\" isNull=\"1\" />"),
    }
    x.value("ProjectFilterName", "UnnamedFilter");
    x.value("UserNote", &p.note);
    x.line("<SelectedSyncObjects version=\"1\" type=\"SelectedSyncObjects\" isNull=\"1\" />");
    x.value("SCGroupingStyle", "0");
    // Schema projects keep SQL Data Compare's defaults.
    let bits = |table: &[(u64, DataOpt)], default: u64| -> String {
        if p.kind == ProjectKind::Schema {
            return default.to_string();
        }
        table.iter().filter(|(_, o)| p.options.iter().any(|n| n.eq_ignore_ascii_case(o.name()))).map(|(b, _)| b).sum::<u64>().to_string()
    };
    x.value("SQLOptions", &bits(SQL_BITS, 0xA));
    x.value("MappingOptions", &bits(MAPPING_BITS, 0x52));
    x.value("ComparisonOptions", &bits(COMPARISON_BITS, 0));

    let mut actions: Vec<Vec<(&str, String)>> = Vec::new();
    for (a, b) in &p.schema_mappings.schemas {
        actions.push(vec![("action", "MapOwners".into()), ("Owner1", a.clone()), ("Owner2", b.clone())]);
    }
    for t in &p.schema_mappings.tables {
        actions.push(vec![("action", "MapTables".into()), ("Table1", format!("{}:null", quote_name(&t.source))), ("Table2", format!("null:{}", quote_name(&t.target)))]);
        for (c1, c2) in &t.columns {
            actions.push(vec![
                ("action", "MapColumn".into()),
                ("Table1", quote_name(&t.source)),
                ("Column1", c1.clone()),
                ("Table2", quote_name(&t.target)),
                ("Column2", c2.clone()),
            ]);
        }
    }
    for t in &p.data_mappings.tables {
        let table = quote_name(&t.source);
        if let Some(target) = &t.target {
            actions.push(vec![("action", "MapTables".into()), ("Table1", format!("{table}:null")), ("Table2", format!("null:{}", quote_name(target)))]);
        }
        if t.excluded {
            actions.push(vec![("action", "DeselectItem".into()), ("val", format!("{table}:null"))]);
        }
        for k in &t.key {
            match k.strip_prefix("index:") {
                Some(ix) => actions.push(vec![("action", "UseKey".into()), ("IndexName", ix.to_string()), ("TableName", table.clone())]),
                None => actions.push(vec![("action", "SelectColumnAsKey".into()), ("ColumnName", k.clone()), ("TableName", table.clone())]),
            }
        }
        for c in &t.excluded_columns {
            actions.push(vec![("action", "DeselectColumn".into()), ("ColumnName", c.clone()), ("TableName", table.clone())]);
        }
        for (action, w) in [("SetLeftWhereClause", &t.where1), ("SetRightWhereClause", &t.where2)] {
            if let Some(w) = w {
                actions.push(vec![("action", action.into()), ("TableName", table.clone()), ("IndexName", w.clone())]);
            }
        }
    }
    if p.kind == ProjectKind::Schema {
        for d in &p.deselected {
            actions.push(vec![("action", "DeselectItem".into()), ("val", format!("{}:null", quote_name(&ObjectName::new(&d.schema, &d.name))))]);
        }
    }
    if actions.is_empty() {
        x.line("<TableActions type=\"ArrayList\" version=\"1\" />");
    } else {
        x.open("TableActions", " type=\"ArrayList\" version=\"1\"");
        for a in actions {
            x.open("value", " version=\"1\" type=\"SelectTableEvent\"");
            for (k, v) in a {
                x.value(k, &v);
            }
            x.close("value");
        }
        x.close("TableActions");
    }
    let identical_records = p.kind == ProjectKind::Data && p.options.iter().any(|o| o == DataOpt::IncludeIdenticalRecords.name());
    x.value("SessionSettings", if identical_records { "15" } else { "14" });
    x.value("DCGroupingStyle", "0");
    x.open("SC_DeploymentOptions", " version=\"1\" type=\"SC_DeploymentOptions\"");
    let b = &p.backup;
    if b.enabled || *b != crate::schema::backup::BackupOptions::default() {
        x.open("BackupOptions", " version=\"1\" type=\"ProjectBackupOptions\"");
        x.value("BackupProvider", if b.sqb() { "Sqb" } else { "Native" });
        x.value("TypeOfBackup", if b.differential { "Differential" } else { "Full" });
        x.value("Folder", &b.folder);
        x.value("Filename", &b.file);
        x.value("SqbLicenseType", "None");
        x.value("SqbVersion", "0");
        x.value("DefaultNativeFolder", "");
        x.value("DefaultSqbFolder", "");
        // Passwords are never written.
        x.value("Password", "");
        x.value("NameFileAutomatically", bool_text(b.name_automatically));
        x.value("OverwriteIfExists", bool_text(b.overwrite));
        x.value("CompressionLevel", &b.compression_level.to_string());
        x.value("EncryptionLevel", match b.encryption.as_str() { "aes128" => "Aes128", "aes256" => "Aes256", _ => "None" });
        x.value("ThreadCount", &b.thread_count.to_string());
        x.value("BackupEnabled", bool_text(b.enabled));
        x.close("BackupOptions");
    } else {
        x.line("<BackupOptions version=\"1\" type=\"ProjectBackupOptions\" isNull=\"1\" />");
    }
    x.close("SC_DeploymentOptions");
    x.close("Project");
    [0xFF, 0xFE].into_iter().chain(x.out.encode_utf16().flat_map(u16::to_le_bytes)).collect()
}

fn entry<'a>(m: &'a mut DataMappings, table: &ObjectName) -> &'a mut TableOverride {
    if let Some(i) = m.tables.iter().position(|t| t.source == *table) {
        return &mut m.tables[i];
    }
    m.tables.push(TableOverride { source: table.clone(), ..Default::default() });
    m.tables.last_mut().unwrap()
}

/// The bytes after a UTF-8 byte order mark, which Notepad and PowerShell 5 write.
pub fn without_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_project_saved_with_a_bom() {
        let dir = std::env::temp_dir().join(format!("quill-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.qsc");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(serde_json::to_vec(&Project::new(ProjectKind::Data)).unwrap());
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(Project::load(&path).unwrap().kind, ProjectKind::Data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn round_trip_without_passwords() {
        let mut p = Project::new(ProjectKind::Schema);
        p.source = SourceSpec { authentication: Authentication::Sql, user: "sa".into(), password: Some("secret".into()), ..SourceSpec::database("dev", "Shop") };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("secret"));
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source.user, "sa");
        assert!(back.source.password.is_none());
        assert!(back.options.contains(&"IgnoreWhiteSpace".to_string()));
    }

    #[test]
    fn imports_a_sql_compare_project() {
        let scp = r#"<?xml version="1.0" encoding="utf-16" standalone="yes"?>
<!-- SQL Compare Version:15.4.17.28422 -->
<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="5" type="LiveDatabaseSource">
    <ServerName>dev</ServerName><DatabaseName>Shop</DatabaseName><Username>sa</Username>
    <SavePassword>True</SavePassword><EncryptConnection>True</EncryptConnection><TrustServerCertificate>True</TrustServerCertificate>
    <Password encrypted="1">AAAA</Password><AuthenticationType>SqlPassword</AuthenticationType>
  </DataSource1>
  <DataSource2 version="1" type="SnapshotSource"><FileName>C:\snap\prod.snp</FileName></DataSource2>
  <Options>Default,IgnoreComments,IgnoreFileGroupsPartitionSchemesAndPartitionFunctions</Options>
  <Direction>0</Direction>
  <TableActions type="ArrayList" version="1">
    <value version="1" type="SelectTableEvent"><action>MapOwners</action><Owner1>dev</Owner1><Owner2>dbo</Owner2></value>
    <value version="1" type="SelectTableEvent"><action>MapTables</action><Table1>[dbo].[Old]:null</Table1><Table2>null:[dbo].[New]</Table2></value>
  </TableActions>
</Project>"#;
        let bytes: Vec<u8> = [0xFF, 0xFE].into_iter().chain(scp.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let p = import_vendor(&bytes, ProjectKind::Schema).unwrap();
        assert_eq!((p.source.server.as_str(), p.source.authentication, p.source.user.as_str()), ("dev", Authentication::Sql, "sa"));
        assert!(p.source.trust_server_certificate && p.source.password.is_none());
        assert!(p.target.is_snapshot() && p.target.path.ends_with("prod.snp"));
        assert!(p.options.contains(&"IgnoreComments".to_string()) && p.options.contains(&"IgnoreFileGroups".to_string()));
        assert_eq!(p.schema_mappings.schemas, [("dev".to_string(), "dbo".to_string())]);
        assert_eq!(p.schema_mappings.tables[0].target, ObjectName::new("dbo", "New"));
    }

    #[test]
    fn imports_a_scripts_folder_and_its_filter() {
        let scp = r#"<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="2" type="FolderDataSource"><Path>C:\db\scripts</Path><DatabaseName>scripts</DatabaseName><ServerName>Scripts</ServerName></DataSource1>
  <DataSource2 version="5" type="LiveDatabaseSource"><ServerName>b</ServerName><DatabaseName>y</DatabaseName></DataSource2>
  <ProjectFilter version="1" type="DifferenceFilter">
    <FilterCaseSensitive>False</FilterCaseSensitive>
    <Filters version="1">
      <None version="1"><Include>True</Include><Expression>TRUE</Expression></None>
      <Table version="1"><Include>True</Include><Expression>@SCHEMA = 'dbo'</Expression></Table>
    </Filters>
  </ProjectFilter>
</Project>"#;
        let p = import_vendor(scp.as_bytes(), ProjectKind::Schema).unwrap();
        assert!(p.source.is_scripts() && p.source.path == r"C:\db\scripts");
        let f = crate::schema::filter::Filter::from_xml(p.filter.as_deref().unwrap()).unwrap();
        let d = crate::schema::Difference {
            object_type: crate::schema::ObjectType::Table,
            name: ObjectName::new("Sales", "Order"),
            target_name: None,
            kind: crate::schema::DiffKind::Different,
            properties: Vec::new(),
            selected: true,
            trivial: false,
        };
        assert!(!f.includes(&d));
    }

    #[test]
    fn writes_projects_sql_compare_can_read_and_reads_them_back() {
        let mut p = Project::new(ProjectKind::Schema);
        p.source = SourceSpec { authentication: Authentication::Sql, user: "sa".into(), password: Some("secret".into()), ..SourceSpec::database("dev", "Shop") };
        p.target = SourceSpec { kind: "snapshot".into(), path: "C:\\snap\\prod.snp".into(), ..Default::default() };
        p.options = vec!["IgnoreComments".into(), "IgnoreWhiteSpace".into()];
        p.schema_mappings.schemas.push(("dev".into(), "dbo".into()));
        p.schema_mappings.tables.push(TableMapping { source: ObjectName::new("dbo", "Old"), target: ObjectName::new("dbo", "New"), columns: vec![("A".into(), "B".into())] });
        p.note = "a & b".into();
        let bytes = export_vendor(&p, "11/07/2026 14:03:09");
        let text = decode_text(&bytes);
        assert!(bytes.starts_with(&[0xFF, 0xFE]) && !text.contains("secret"));
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"utf-16\" standalone=\"yes\"?>\r\n<!--"));
        assert!(text.contains("\r\n  <DataSource1 version=\"5\" type=\"LiveDatabaseSource\">\r\n    <ServerName>dev</ServerName>"));
        assert!(text.contains("<Password />") && text.contains("<AuthenticationType>SqlPassword</AuthenticationType>"));
        assert!(text.contains("<UserNote>a &amp; b</UserNote>"));
        let back = import_vendor(&bytes, ProjectKind::Schema).unwrap();
        assert_eq!((back.source.server.as_str(), back.source.user.as_str(), back.source.authentication), ("dev", "sa", Authentication::Sql));
        assert!(back.target.is_snapshot() && back.target.path.ends_with("prod.snp"));
        assert_eq!(back.schema_mappings, p.schema_mappings);
        assert!(back.options.contains(&"IgnoreComments".to_string()) && back.options.len() == 2);
        assert_eq!(back.note, "a & b");
        let mut e = Project::new(ProjectKind::Schema);
        e.target = SourceSpec { kind: "empty".into(), ..Default::default() };
        assert!(import_vendor(&export_vendor(&e, ""), ProjectKind::Schema).unwrap().target.is_empty_database());

        let mut d = Project::new(ProjectKind::Data);
        d.data_mappings.tables.push(TableOverride { source: ObjectName::new("dbo", "T"), key: vec!["Code".into()], where1: Some("Id > 5".into()), ..Default::default() });
        let back = import_vendor(&export_vendor(&d, ""), ProjectKind::Data).unwrap();
        let (mut want, mut got) = (d.options.clone(), back.options.clone());
        want.sort();
        got.sort();
        assert_eq!(got, want);
        assert_eq!(back.data_mappings.tables[0].key, ["Code"]);
        assert_eq!(back.data_mappings.tables[0].where1.as_deref(), Some("Id > 5"));
    }

    #[test]
    fn imports_data_compare_options() {
        let sdc = r#"<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="5" type="LiveDatabaseSource"><ServerName>a</ServerName><DatabaseName>x</DatabaseName><AuthenticationType>WindowsIntegrated</AuthenticationType></DataSource1>
  <DataSource2 version="5" type="LiveDatabaseSource"><ServerName>b</ServerName><DatabaseName>y</DatabaseName></DataSource2>
  <SQLOptions>3</SQLOptions><MappingOptions>66</MappingOptions><ComparisonOptions>2</ComparisonOptions>
  <TableActions type="ArrayList" version="1">
    <value version="1" type="SelectTableEvent"><action>SetLeftWhereClause</action><TableName>[dbo].[T]</TableName><IndexName>Id &gt; 5</IndexName></value>
    <value version="1" type="SelectTableEvent"><action>SelectColumnAsKey</action><TableName>[dbo].[T]</TableName><ColumnName>Code</ColumnName></value>
    <value version="1" type="SelectTableEvent"><action>MapOwners</action><Owner1>dev</Owner1><Owner2>dbo</Owner2></value>
    <value version="1" type="SelectTableEvent"><action>MapColumn</action><Table1>[dbo].[T]</Table1><Column1>Amount</Column1><Column2>Total</Column2></value>
  </TableActions>
</Project>"#;
        let p = import_vendor(sdc.as_bytes(), ProjectKind::Data).unwrap();
        for o in ["DisableTriggers", "DisableKeys", "IgnoreSpaces", "IncludeIdentities", "ForceBinaryCollation"] {
            assert!(p.options.contains(&o.to_string()), "{o}: {:?}", p.options);
        }
        let t = &p.data_mappings.tables[0];
        assert_eq!((t.where1.as_deref(), t.key.as_slice()), (Some("Id > 5"), &["Code".to_string()][..]));
        assert_eq!(t.columns, [("Amount".to_string(), "Total".to_string())]);
        assert_eq!(p.data_mappings.schemas, [("dev".to_string(), "dbo".to_string())]);
    }

    #[test]
    fn backup_sources_round_trip() {
        let scp = r#"<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="1" type="BackupDatabaseSource"><Files type="ArrayList" version="1"><value type="string">C:\b\a.bak</value><value type="string">C:\b\b.bak</value></Files>
    <BackupSet version="5" type="ProjectBackupSet"><BackupSetName>Nightly</BackupSetName><DatabaseName>Shop</DatabaseName></BackupSet></DataSource1>
  <DataSource2 version="5" type="LiveDatabaseSource"><ServerName>b</ServerName><DatabaseName>y</DatabaseName></DataSource2>
</Project>"#;
        let p = import_vendor(scp.as_bytes(), ProjectKind::Schema).unwrap();
        assert!(p.source.is_backup());
        assert_eq!((p.source.path.as_str(), p.source.backup_set.as_str()), (r"C:\b\a.bak;C:\b\b.bak", "Nightly"));
        let out = export_vendor(&p, "01/01/2026 00:00:00");
        let text = String::from_utf16_lossy(&out[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
        let again = import_vendor(text.as_bytes(), ProjectKind::Schema).unwrap();
        assert_eq!(again.source, p.source);
        assert!(text.contains("<DataSource1 version=\"1\" type=\"BackupDatabaseSource\">"), "{text}");
    }
}
