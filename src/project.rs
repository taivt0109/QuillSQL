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
    /// Microsoft Entra user name and password (SQL Compare's MicrosoftEntraPassword).
    #[serde(rename = "entraPassword", alias = "entrapassword")]
    EntraPassword,
    /// The Entra account signed in on this machine (MicrosoftEntraIntegrated).
    #[serde(rename = "entraIntegrated", alias = "entraintegrated")]
    EntraIntegrated,
    /// Entra with MFA, by device-code sign-in (MicrosoftEntraWithMfa).
    #[serde(rename = "entraInteractive", alias = "entrainteractive")]
    EntraInteractive,
}

impl Authentication {
    /// The project-file AuthenticationType name.
    pub fn external_name(self) -> &'static str {
        match self {
            Authentication::Windows => "WindowsIntegrated",
            Authentication::Sql => "SqlPassword",
            Authentication::Entra | Authentication::EntraIntegrated => "MicrosoftEntraIntegrated",
            Authentication::EntraPassword => "MicrosoftEntraPassword",
            Authentication::EntraInteractive => "MicrosoftEntraWithMfa",
        }
    }

    /// Signs in with a user name and password (SQL or Entra).
    pub fn uses_password(self) -> bool {
        matches!(self, Authentication::Sql | Authentication::EntraPassword)
    }
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
    /// Microsoft Entra tenant (id or domain); empty for the user's domain or `organizations`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tenant: String,
    /// `type = scripts` from SQL Source Control: the scripts folder location XML it was linked
    /// with (written back as a SourceControlDataSource).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub location_xml: String,
    /// `type = scripts`: SQL Server major version (e.g. `16`) used when the folder has no
    /// QuillDatabaseInfo.xml.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sql_version: String,
    /// `type = scripts`: default collation used when the folder has no QuillDatabaseInfo.xml.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub default_collation: String,
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

    /// The project side of a live connection; passwords and tokens are left out.
    pub fn from_connection(c: &ConnectionSpec) -> SourceSpec {
        let (authentication, user, tenant) = match &c.auth {
            Auth::Sql { user, .. } => (Authentication::Sql, user.clone(), String::new()),
            Auth::AccessToken(_) => (Authentication::Entra, String::new(), String::new()),
            Auth::Entra(l) => (
                match l.method {
                    crate::entra::Method::Password => Authentication::EntraPassword,
                    crate::entra::Method::Integrated => Authentication::EntraIntegrated,
                    crate::entra::Method::Interactive => Authentication::EntraInteractive,
                },
                l.user.clone(),
                l.tenant.clone(),
            ),
            Auth::Windows => (Authentication::Windows, String::new(), String::new()),
        };
        SourceSpec { authentication, user, tenant, encrypt: c.encrypt, trust_server_certificate: c.trust_server_certificate, ..SourceSpec::database(&c.server, &c.database) }
    }

    pub fn is_snapshot(&self) -> bool {
        self.kind == "snapshot"
    }

    /// A scripts folder (SQL Compare `FolderDataSource`), at `path`; also a SQL Change
    /// Automation project (`type = sca`, `path` its project file), read the same way.
    pub fn is_scripts(&self) -> bool {
        self.kind == "scripts" || self.is_sca()
    }

    /// A SQL Change Automation project (SQL Compare `ScaDataSource`).
    pub fn is_sca(&self) -> bool {
        self.kind == "sca"
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
            Authentication::EntraPassword | Authentication::EntraIntegrated | Authentication::EntraInteractive => {
                let method = match self.authentication {
                    Authentication::EntraPassword => crate::entra::Method::Password,
                    Authentication::EntraIntegrated => crate::entra::Method::Integrated,
                    _ => crate::entra::Method::Interactive,
                };
                let password = match method {
                    crate::entra::Method::Password => self
                        .password
                        .clone()
                        .or_else(|| std::env::var(format!("QUILL_PASSWORD{n}")).ok())
                        .ok_or_else(|| format!("no password for {} on {}; set QUILL_PASSWORD{n} or give /password{n}", self.user, self.server))?,
                    _ => String::new(),
                };
                Auth::Entra(crate::entra::Login { method, user: self.user.clone(), password, tenant: self.tenant.clone() })
            }
        };
        let mut spec = ConnectionSpec::new(&self.server, &self.database, auth);
        spec.encrypt = self.encrypt;
        spec.trust_server_certificate = self.trust_server_certificate;
        Ok(spec)
    }
}

pub const DEFAULT_ROW_KINDS: [bool; 4] = [false, true, true, true];

fn is_default_kinds(k: &[bool; 4]) -> bool {
    *k == DEFAULT_ROW_KINDS
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn is_true(b: &bool) -> bool {
    *b
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Data projects: compare tables that are not in `selected` (SQL Data Compare's "include
    /// new tables"). When false only `selected` tables are compared (`.sdc` DeselectAll + SelectItem).
    #[serde(skip_serializing_if = "is_true")]
    pub include_new_tables: bool,
    /// Data projects with `include_new_tables` off: the tables to compare.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected: Vec<Deselected>,
    /// Data projects: the row kinds compared and deployed, in `RowKind` order (identical,
    /// different, only in 1, only in 2); `.sdc` SessionSettings bits 1, 2, 4, 8. Identical rows
    /// are also on with the IncludeIdenticalRecords option.
    #[serde(skip_serializing_if = "is_default_kinds")]
    pub row_kinds: [bool; 4],
    /// Back up target before deployment (SQL Compare's `SC_DeploymentOptions/BackupOptions`).
    #[serde(skip_serializing_if = "is_disabled")]
    pub backup: crate::schema::backup::BackupOptions,
    pub note: String,
    /// The filter of an imported SQL Compare project (its `ProjectFilter` XML).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// The deployment selection saved by the SQL Compare window (`SelectedSyncObjects`), whose
    /// objects are known only by a hash of their names: see `imported_selected`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_selection: Option<ImportedSelection>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportedSelection {
    /// Objects without an entry are selected.
    pub select_all: bool,
    pub entries: Vec<SelectionEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SelectionEntry {
    /// The object's schema (`Default_Schema` for objects without one).
    pub schema: String,
    /// MD5 of the object name in UTF-16, in hex.
    pub hash: String,
    /// SQL Compare's object type + 1 and difference type; 1 and 0 for "any".
    pub object_type: u8,
    pub difference: u8,
    pub selected: bool,
}

impl Default for Project {
    fn default() -> Project {
        Project {
            version: 0,
            kind: ProjectKind::default(),
            source: SourceSpec::default(),
            target: SourceSpec::default(),
            options: Vec::new(),
            schema_mappings: Mappings::default(),
            data_mappings: DataMappings::default(),
            include: Vec::new(),
            exclude: Vec::new(),
            deselected: Vec::new(),
            include_new_tables: true,
            selected: Vec::new(),
            row_kinds: DEFAULT_ROW_KINDS,
            backup: Default::default(),
            note: String::new(),
            filter: None,
            imported_selection: None,
        }
    }
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

    /// Whether an imported SQL Compare selection deploys the object; None without one.
    pub fn imported_selected(&self, schema: &str, name: &str) -> Option<bool> {
        let sel = self.imported_selection.as_ref()?;
        let schema = if schema.is_empty() { crate::project_codec::DEFAULT_SCHEMA } else { schema };
        let hash = hex(&crate::project_codec::object_hash(name));
        let mut found = sel.entries.iter().filter(|e| e.schema == schema && e.hash.eq_ignore_ascii_case(&hash));
        let all: Vec<&SelectionEntry> = found.by_ref().collect();
        Some(all.iter().find(|e| e.object_type == 1 && e.difference == 0).or(all.first()).map(|e| e.selected).unwrap_or(sel.select_all))
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

/// Splits Vendor's `one:two` pair at the colon outside brackets.
fn split_pair(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let (mut in_bracket, mut i) = (false, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'[' if !in_bracket => in_bracket = true,
            b']' if in_bracket => {
                if bytes.get(i + 1) == Some(&b']') {
                    i += 1;
                } else {
                    in_bracket = false;
                }
            }
            b':' if !in_bracket => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The database 1 part of a column or index pair (`Code:Code`), or the name itself.
fn pair_part(s: &str) -> String {
    let s = s.trim();
    // A name holding a colon, paired with itself, splits in the middle.
    let half = s.len() / 2;
    let part = if s.len() % 2 == 1 && s.is_char_boundary(half) && s.as_bytes()[half] == b':' && s[..half] == s[half + 1..] {
        &s[..half]
    } else {
        match split_pair(s) {
            Some((a, b)) if a.trim() == "null" => b,
            Some((a, _)) => a,
            None => s,
        }
    };
    let part = part.trim();
    match part.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
        Some(inner) => inner.replace("]]", "]"),
        None => part.to_string(),
    }
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn text_of(node: roxmltree::Node<'_, '_>, name: &str) -> String {
    child(node, name).and_then(|c| c.text()).unwrap_or_default().trim().to_string()
}

/// Reads a SQL Compare (.scp) or SQL Data Compare (.sdc) project. Saved passwords are read for
/// the run only.
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
        if ty.eq_ignore_ascii_case("ScaDataSource") {
            return SourceSpec { kind: "sca".into(), path: text_of(n, "ProjectFilePath"), ..Default::default() };
        }
        // A database linked to SQL Source Control: its scripts folder at a revision.
        if ty.eq_ignore_ascii_case("SourceControlDataSource") {
            let location_xml = text_of(n, "ScriptFolderLocation");
            let path = crate::sources::scm::location_from_xml(&location_xml).unwrap_or_default();
            let latest = match text_of(n, "VersionType").as_str() {
                "" => text_of(n, "UseNewestRevision").eq_ignore_ascii_case("true") || text_of(n, "VersionToUse") == "0",
                v => !v.eq_ignore_ascii_case("Specific"),
            };
            let specific = match text_of(n, "SpecificVersion") {
                v if v.is_empty() => text_of(n, "Revision"),
                v => v,
            };
            let revision = if latest || specific.is_empty() { "HEAD".to_string() } else { specific };
            return SourceSpec { kind: "scripts".into(), path, revision, location_xml, ..Default::default() };
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
        // Projects before version 4 have IntegratedSecurity instead of AuthenticationType.
        let integrated = text_of(n, "IntegratedSecurity");
        s.authentication = match auth.to_ascii_lowercase().as_str() {
            "" if !integrated.is_empty() => if integrated.eq_ignore_ascii_case("true") { Authentication::Windows } else { Authentication::Sql },
            "" if !user.is_empty() => Authentication::Sql,
            a if a.contains("sqlpassword") => Authentication::Sql,
            "microsoftentrapassword" | "activedirectorypassword" => Authentication::EntraPassword,
            "microsoftentrawithmfa" | "activedirectoryinteractive" => Authentication::EntraInteractive,
            a if a.contains("activedirectory") || a.contains("entra") => Authentication::EntraIntegrated,
            _ => Authentication::Windows,
        };
        s.user = user;
        s.remember_password = text_of(n, "SavePassword").eq_ignore_ascii_case("true");
        // A saved password (fixed-key encryption) is used at run time and never written back.
        if s.remember_password && s.authentication.uses_password()
            && let Some(pw) = child(n, "Password")
        {
            let text = pw.text().unwrap_or_default();
            let plain = if pw.attribute("encrypted") == Some("1") { crate::project_codec::decrypt_password(text) } else { text.to_string() };
            s.password = (!plain.is_empty()).then_some(plain);
        }
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
            let mut options = text_of(root, "Options");
            // Old SQL Compare projects store the options as a number.
            if let Ok(bits) = options.parse::<u64>() {
                options = OLD_OPTION_BITS.iter().filter(|(b, _)| bits & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join(",");
            }
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
            // ExcludeNewTablesByDefault: only the tables the project selects are compared.
            if int("MappingOptions").is_some_and(|m| m & 0x100 != 0) {
                p.include_new_tables = false;
            }
            if let Some(bits) = int("SessionSettings") {
                p.row_kinds = [0x1, 0x2, 0x4, 0x8].map(|b| bits & b != 0);
                if p.row_kinds[0] {
                    opts.push(DataOpt::IncludeIdenticalRecords.name().to_string());
                }
            }
            p.options = opts;
        }
    }

    // Mapping and selection actions, replayed in order. Vendor names a table by its pair,
    // `[s].[t1]:[s].[t2]` (`null` for a missing side); older files use the plain name.
    if let Some(actions) = child(root, "TableActions") {
        for ev in actions.children().filter(|c| c.is_element()) {
            let action = text_of(ev, "action");
            let side = |s: &str| -> (Option<ObjectName>, Option<ObjectName>) {
                let (a, b) = split_pair(s).unwrap_or((s, "null"));
                let n = |x: &str| if x.trim() == "null" { None } else { split_name(x) };
                (n(a), n(b))
            };
            let table = |name: &str| -> Option<ObjectName> {
                let (a, b) = side(&text_of(ev, name));
                a.or(b)
            };
            let column = || pair_part(&text_of(ev, "ColumnName"));
            match action.as_str() {
                "MapTables" => {
                    let (a, _) = side(&text_of(ev, "Table1"));
                    let (_, b) = side(&text_of(ev, "Table2"));
                    if let (Some(a), Some(b)) = (a, b) {
                        match kind {
                            ProjectKind::Schema => {
                                p.schema_mappings.tables.retain(|m| m.source != a);
                                p.schema_mappings.tables.push(TableMapping { source: a, target: b, columns: Vec::new() })
                            }
                            ProjectKind::Data => entry(&mut p.data_mappings, &a).target = Some(b),
                        }
                    }
                }
                // A default pair broken up: the source table is left unmapped (an empty target).
                "UnmapTables" => {
                    let (Some(a), _) = side(&text_of(ev, "val")) else { continue };
                    match kind {
                        ProjectKind::Schema => {
                            if !p.schema_mappings.tables.iter().any(|m| m.source == a) {
                                p.schema_mappings.tables.push(TableMapping { source: a, target: ObjectName::default(), columns: Vec::new() });
                            }
                        }
                        ProjectKind::Data => {
                            let e = entry(&mut p.data_mappings, &a);
                            if e.target.is_none() {
                                e.target = Some(ObjectName::default());
                            }
                        }
                    }
                }
                "UnmapOwners" => {
                    let owner = text_of(ev, "val");
                    let list = if kind == ProjectKind::Schema { &mut p.schema_mappings.schemas } else { &mut p.data_mappings.schemas };
                    if !owner.is_empty() && !list.iter().any(|(a, _)| *a == owner) {
                        list.push((owner, String::new()));
                    }
                }
                "MapOwners" => match kind {
                    ProjectKind::Schema => p.schema_mappings.schemas.push((text_of(ev, "Owner1"), text_of(ev, "Owner2"))),
                    ProjectKind::Data => p.data_mappings.schemas.push((text_of(ev, "Owner1"), text_of(ev, "Owner2"))),
                },
                "MapColumn" => {
                    let Some(t1) = table("Table1") else { continue };
                    let (c1, c2) = (pair_part(&text_of(ev, "Column1")), pair_part(&text_of(ev, "Column2")));
                    if kind == ProjectKind::Data {
                        let e = entry(&mut p.data_mappings, &t1);
                        e.columns.retain(|(a, _)| !a.eq_ignore_ascii_case(&c1));
                        e.excluded_columns.retain(|c| !c.eq_ignore_ascii_case(&c1));
                        e.columns.push((c1, c2));
                    } else if let Some(m) = p.schema_mappings.tables.iter_mut().find(|m| m.source == t1) {
                        m.columns.push((c1, c2));
                    }
                }
                // Table selection is for data projects only: SQL Compare selects schema objects
                // with SelectedSyncObjects, and its command line by filter.
                "SelectAll" | "DeselectAll" if kind == ProjectKind::Data => {
                    p.include_new_tables = action == "SelectAll";
                    p.selected.clear();
                    p.data_mappings.tables.iter_mut().for_each(|t| t.excluded = false);
                }
                "SelectItem" | "DeselectItem" if kind == ProjectKind::Data => {
                    let Some(n) = table("val") else { continue };
                    let select = action == "SelectItem";
                    if p.include_new_tables {
                        if select {
                            if let Some(t) = p.data_mappings.tables.iter_mut().find(|t| t.source == n) {
                                t.excluded = false;
                            }
                        } else {
                            entry(&mut p.data_mappings, &n).excluded = true;
                        }
                    } else {
                        p.selected.retain(|d| !(d.schema.eq_ignore_ascii_case(&n.schema) && d.name.eq_ignore_ascii_case(&n.name)));
                        if select {
                            p.selected.push(Deselected { kind: "Table".into(), schema: n.schema, name: n.name });
                        }
                    }
                }
                "UseKey" => {
                    if let Some(t) = table("TableName") {
                        entry(&mut p.data_mappings, &t).key = vec![format!("index:{}", pair_part(&text_of(ev, "IndexName")))];
                    }
                }
                // A key of chosen columns follows (SelectColumnAsKey).
                "UseCustomKey" => {
                    if let Some(t) = table("val") {
                        entry(&mut p.data_mappings, &t).key.retain(|k| !k.starts_with("index:"));
                    }
                }
                "SelectColumnAsKey" => {
                    if let Some(t) = table("TableName") {
                        let (e, c) = (entry(&mut p.data_mappings, &t), column());
                        e.key.retain(|k| !k.starts_with("index:") && !k.eq_ignore_ascii_case(&c));
                        e.key.push(c);
                    }
                }
                "DeselectColumnAsKey" => {
                    if let Some(t) = table("TableName") {
                        let c = column();
                        entry(&mut p.data_mappings, &t).key.retain(|k| !k.eq_ignore_ascii_case(&c));
                    }
                }
                // An unmapped column is compared on neither side.
                "DeselectColumn" | "UnmapColumns" => {
                    if let Some(t) = table("TableName") {
                        let (e, c) = (entry(&mut p.data_mappings, &t), column());
                        if !e.excluded_columns.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
                            e.excluded_columns.push(c);
                        }
                    }
                }
                "SelectColumn" => {
                    if let Some(t) = table("TableName") {
                        let c = column();
                        entry(&mut p.data_mappings, &t).excluded_columns.retain(|x| !x.eq_ignore_ascii_case(&c));
                    }
                }
                "SetLeftWhereClause" | "SetRightWhereClause" => {
                    if let Some(t) = table("TableName") {
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
    // Old SQL Compare projects (before SelectedSyncObjects): owner mappings and the object
    // selection as user actions.
    if kind == ProjectKind::Schema {
        for ev in child(root, "OwnerMappingUserActions").into_iter().flat_map(|l| l.children().filter(|c| c.is_element())) {
            match text_of(ev, "action").as_str() {
                "MapOwners" => p.schema_mappings.schemas.push((text_of(ev, "Owner1"), text_of(ev, "Owner2"))),
                "UnmapOwners" if !text_of(ev, "val").is_empty() => p.schema_mappings.schemas.push((text_of(ev, "val"), String::new())),
                _ => {}
            }
        }
        if let Some(list) = child(root, "ObjectSelectionUserActions") {
            let mut sel = ImportedSelection { select_all: true, entries: Vec::new() };
            for ev in list.children().filter(|c| c.is_element()) {
                match text_of(ev, "action").as_str() {
                    "SelectAll" => sel = ImportedSelection { select_all: true, entries: Vec::new() },
                    "DeselectAll" => sel = ImportedSelection { select_all: false, entries: Vec::new() },
                    a @ ("SelectItem" | "DeselectItem") => {
                        let Some(n) = split_name(&text_of(ev, "val")) else { continue };
                        let hash = hex(&crate::project_codec::object_hash(&n.name));
                        sel.entries.retain(|e| !(e.schema == n.schema && e.hash == hash));
                        sel.entries.push(SelectionEntry { schema: n.schema, hash, object_type: 1, difference: 0, selected: a == "SelectItem" });
                    }
                    _ => {}
                }
            }
            p.imported_selection = Some(sel);
        }
    }
    if kind == ProjectKind::Schema
        && let Some(sel) = child(root, "SelectedSyncObjects").filter(|n| n.attribute("isNull") != Some("1"))
    {
        let version = sel.attribute("version").and_then(|v| v.parse().ok()).unwrap_or(1);
        let mut out = ImportedSelection { select_all: text_of(sel, "SelectAll").eq_ignore_ascii_case("true"), entries: Vec::new() };
        let tokens: Vec<String> = child(sel, "Schemas").map(|l| l.children().filter(|c| c.is_element()).filter_map(|c| c.text()).map(|t| t.trim().to_string()).collect()).unwrap_or_default();
        for token in tokens {
            let Some(schema) = crate::project_codec::schema_from_token(&token, version) else { continue };
            let Some(bytes) = crate::project_codec::base64_decode(&text_of(sel, &token).replace('_', "=")) else { continue };
            for e in bytes.as_chunks::<19>().0 {
                out.entries.push(SelectionEntry { schema: schema.clone(), hash: hex(&e[..16]), object_type: e[16], difference: e[17], selected: e[18] > 0 });
            }
        }
        p.imported_selection = Some(out);
    }
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

/// Option bits of old SQL Compare projects (a number in place of the names).
const OLD_OPTION_BITS: &[(u64, &str)] = &[
    (0x1, "ForceColumnOrder"),
    (0x2, "IgnoreFillFactor"),
    (0x4, "IgnorePermissions"),
    (0x8, "IgnoreWhiteSpace"),
    (0x10, "IgnoreBindings"),
    (0x20, "IgnoreQuotedIdentifiersAndAnsiNullSettings"),
    (0x40, "CaseSensitiveObjectDefinition"),
    (0x80, "IgnoreExtendedProperties"),
    (0x100, "IgnoreFullTextIndexing"),
    (0x200, "NoSQLPlumbing"),
    (0x400, "IgnoreCollations"),
    (0x800, "IgnoreComments"),
    (0x2000, "IgnoreIndexes"),
    (0x4000, "IgnoreKeys"),
    (0x8000, "IgnoreChecks"),
    (0x10000, "IgnoreTriggers"),
    (0x20000, "IncludeDependencies"),
    (0x40000, "IgnoreInsteadOfTriggers"),
    (0x80000, "IgnoreFileGroups"),
    (0x100000, "IgnoreIdentitySeedAndIncrement"),
    (0x200000, "IgnoreWithNocheck"),
    (0x400000, "IgnoreConstraintNames"),
    (0x800000, "IgnoreStatistics"),
    (0x1000000, "DoNotOutputCommentHeader"),
    (0x2000000, "AddWithEncryption"),
    (0x4000000, "IgnoreSynonymDependencies"),
    (0x8000000, "SeparateTriggers"),
    (0x10000000, "IgnoreOwners"),
    (0x20000000, "IgnoreQueueEventNotifications"),
    (0x40000000, "TargetIsPre2005"),
    (0x80000000, "UseClrUdtToStringForClrMigration"),
    (0x100000000, "ConsiderNextFilegroupInPartitionSchemes"),
    (0x200000000, "IgnoreCertificatesAndCryptoKeys"),
    (0x400000000, "IgnoreTriggerOrder"),
    (0x800000000, "IgnoreUsers"),
    (0x1000000000, "IgnoreUserProperties"),
    (0x2000000000, "DisableAndReenableDdlTriggers"),
    (0x4000000000, "IgnoreWithElementOrder"),
    (0x8000000000, "IgnoreIndexLockProperties"),
    (0x10000000000, "IgnoreReplicationTriggers"),
    (0x20000000000, "IgnoreIdentityProperties"),
    (0x40000000000, "IgnoreNotForReplication"),
    (0x80000000000, "TargetIsPre2008"),
    (0x100000000000, "IgnoreDataCompression"),
    (0x200000000000, "IgnoreDatabaseAndServerName"),
    (0x400000000000, "AddDatabaseUseStatement"),
    (0x800000000000, "IgnoreSchemaObjectAuthorization"),
    (0x1000000000000, "DecryptPost2kEncryptedObjects"),
    (0x2000000000000, "ForceSyncScriptGeneration"),
    (0x4000000000000, "IgnoreStatisticsNorecompute"),
    (0x8000000000000, "IgnoreSquareBrackets"),
    (0x10000000000000, "ThrowOnFileParseFailed"),
    (0x40000000000000, "TargetIsSqlAzure"),
    (0x80000000000000, "ObjectExistenceChecks"),
    (0x100000000000000, "DropAndCreateInsteadOfAlter"),
    (0x200000000000000, "IgnoreMigrationScripts"),
    (0x400000000000000, "IgnoreTSQLT"),
    (0x800000000000000, "IgnoreSystemNamedConstraintNames"),
    (0x2000000000000000, "SsdtCompatibleOptions"),
    (0x4000000000000000, "IgnoreInternallyUsedMicrosoftExtendedProperties"),
];

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
        } else if side.is_sca() {
            x.open(element, " version=\"1\" type=\"ScaDataSource\"");
            x.value("ProjectFilePath", &side.path);
            let name = std::path::Path::new(&side.path).file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            x.value("DatabaseName", &name);
            x.value("ServerName", "");
            x.close(element);
        } else if side.is_scripts() && !side.location_xml.is_empty() {
            x.open(element, " version=\"4\" type=\"SourceControlDataSource\"");
            let latest = side.revision.is_empty() || side.revision.eq_ignore_ascii_case("HEAD") || side.revision.eq_ignore_ascii_case("LATEST");
            x.value("VersionType", if latest { "Latest" } else { "Specific" });
            x.value("SpecificVersion", if latest { "" } else { &side.revision });
            x.value("ServerName", "");
            let name = std::path::Path::new(&side.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            x.value("DatabaseName", &name);
            x.value("ScriptFolderLocation", &side.location_xml);
            x.value("MigrationsFolderLocation", "");
            x.value("ScriptFolderDescription", "");
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
            let named = side.authentication.uses_password() || side.authentication == Authentication::EntraInteractive;
            x.value("Username", if named { &side.user } else { "" });
            x.value("SavePassword", "False");
            x.value("EncryptConnection", bool_text(side.encrypt));
            x.value("TrustServerCertificate", bool_text(side.trust_server_certificate));
            x.value("Password", "");
            x.value("ScriptFolderLocation", "");
            x.value("MigrationsFolderLocation", "");
            x.value("AuthenticationType", side.authentication.external_name());
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
    write_selection(&mut x, p);
    x.value("SCGroupingStyle", "0");
    // Schema projects keep SQL Data Compare's defaults.
    let bits = |table: &[(u64, DataOpt)], default: u64| -> String {
        if p.kind == ProjectKind::Schema {
            return default.to_string();
        }
        table.iter().filter(|(_, o)| p.options.iter().any(|n| n.eq_ignore_ascii_case(o.name()))).map(|(b, _)| b).sum::<u64>().to_string()
    };
    x.value("SQLOptions", &bits(SQL_BITS, 0xA));
    let mapping = bits(MAPPING_BITS, 0x52).parse::<u64>().unwrap_or(0x52) | if p.kind == ProjectKind::Data && !p.include_new_tables { 0x100 } else { 0 };
    x.value("MappingOptions", &mapping.to_string());
    x.value("ComparisonOptions", &bits(COMPARISON_BITS, 0));

    let mut actions: Vec<Vec<(&str, String)>> = Vec::new();
    // Unmapping (an empty target) is SQL Compare's UnmapOwners / UnmapTables of the default pair.
    let owners = if p.kind == ProjectKind::Data { &p.data_mappings.schemas } else { &p.schema_mappings.schemas };
    for (a, _) in owners.iter().filter(|(_, b)| b.is_empty()) {
        actions.push(vec![("action", "UnmapOwners".into()), ("val", a.clone())]);
    }
    let mapped_schema = |s: &str| owners.iter().find(|(a, _)| a == s).map(|(_, b)| b.clone()).filter(|b| !b.is_empty()).unwrap_or_else(|| s.to_string());
    let unmap = |n: &ObjectName| vec![("action", "UnmapTables".into()), ("val", format!("{}:{}", quote_name(n), quote_name(&ObjectName::new(&mapped_schema(&n.schema), &n.name))))];
    for t in p.schema_mappings.tables.iter().filter(|t| t.target.name.is_empty()) {
        actions.push(unmap(&t.source));
    }
    for t in p.data_mappings.tables.iter().filter(|t| t.target.as_ref().is_some_and(|x| x.name.is_empty())) {
        actions.push(unmap(&t.source));
    }
    let data_owners = if p.kind == ProjectKind::Data { p.data_mappings.schemas.as_slice() } else { &[] };
    for (a, b) in p.schema_mappings.schemas.iter().chain(data_owners).filter(|(_, b)| !b.is_empty()) {
        actions.push(vec![("action", "MapOwners".into()), ("Owner1", a.clone()), ("Owner2", b.clone())]);
    }
    for t in p.schema_mappings.tables.iter().filter(|t| !t.target.name.is_empty()) {
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
    // Data actions name tables and columns by their pair, as SQL Data Compare does.
    let pair_of = |t: &TableOverride| -> String {
        let target = t.target.clone().filter(|x| !x.name.is_empty()).unwrap_or_else(|| ObjectName::new(&mapped_schema(&t.source.schema), &t.source.name));
        format!("{}:{}", quote_name(&t.source), quote_name(&target))
    };
    let mut selection: Vec<Vec<(&str, String)>> = Vec::new();
    for t in &p.data_mappings.tables {
        let table = pair_of(t);
        if let Some(target) = t.target.as_ref().filter(|x| !x.name.is_empty()) {
            actions.push(vec![("action", "MapTables".into()), ("Table1", format!("{}:null", quote_name(&t.source))), ("Table2", format!("null:{}", quote_name(target)))]);
        }
        let column_pair = |c: &str| t.columns.iter().find(|(a, _)| a.eq_ignore_ascii_case(c)).map(|(_, b)| format!("{c}:{b}")).unwrap_or_else(|| format!("{c}:{c}"));
        for (c1, c2) in &t.columns {
            actions.push(vec![
                ("action", "MapColumn".into()),
                ("Table1", table.clone()),
                ("Column1", format!("{c1}:null")),
                ("Table2", table.clone()),
                ("Column2", format!("null:{c2}")),
            ]);
        }
        for c in &t.excluded_columns {
            actions.push(vec![("action", "DeselectColumn".into()), ("ColumnName", column_pair(c)), ("TableName", table.clone())]);
        }
        if t.key.iter().any(|k| !k.starts_with("index:")) {
            actions.push(vec![("action", "UseCustomKey".into()), ("val", table.clone())]);
        }
        for k in &t.key {
            match k.strip_prefix("index:") {
                Some(ix) => actions.push(vec![("action", "UseKey".into()), ("IndexName", format!("{ix}:{ix}")), ("TableName", table.clone())]),
                None => actions.push(vec![("action", "SelectColumnAsKey".into()), ("ColumnName", column_pair(k)), ("TableName", table.clone())]),
            }
        }
        for (action, w) in [("SetLeftWhereClause", &t.where1), ("SetRightWhereClause", &t.where2)] {
            if let Some(w) = w {
                actions.push(vec![("action", action.into()), ("TableName", table.clone()), ("IndexName", w.clone())]);
            }
        }
        if t.excluded && p.include_new_tables {
            selection.push(vec![("action", "DeselectItem".into()), ("val", table.clone())]);
        }
    }
    if p.kind == ProjectKind::Data && !p.include_new_tables {
        selection.insert(0, vec![("action", "DeselectAll".into())]);
        for d in &p.selected {
            let n = ObjectName::new(&d.schema, &d.name);
            let t = p.data_mappings.tables.iter().find(|t| t.source == n).cloned().unwrap_or(TableOverride { source: n, ..Default::default() });
            selection.push(vec![("action", "SelectItem".into()), ("val", pair_of(&t))]);
        }
    }
    actions.extend(selection);
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
    let kinds = if p.kind == ProjectKind::Data { p.row_kinds } else { DEFAULT_ROW_KINDS };
    let session = [0x1, 0x2, 0x4, 0x8].iter().zip(kinds).filter(|(_, on)| *on).map(|(b, _)| b).sum::<u32>() | if identical_records { 1 } else { 0 };
    x.value("SessionSettings", &session.to_string());
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

/// SelectedSyncObjects: the objects deselected in the results (any type and difference), or
/// the imported selection as it was.
fn write_selection(x: &mut XmlOut, p: &Project) {
    let mut sel = match &p.imported_selection {
        Some(s) if p.kind == ProjectKind::Schema => s.clone(),
        _ => ImportedSelection { select_all: true, entries: Vec::new() },
    };
    if p.kind == ProjectKind::Schema {
        for d in &p.deselected {
            let schema = if d.schema.is_empty() { crate::project_codec::DEFAULT_SCHEMA.to_string() } else { d.schema.clone() };
            let hash = hex(&crate::project_codec::object_hash(&d.name));
            sel.entries.retain(|e| !(e.schema == schema && e.hash == hash));
            sel.entries.push(SelectionEntry { schema, hash, object_type: 1, difference: 0, selected: false });
        }
    }
    if p.kind != ProjectKind::Schema || (p.imported_selection.is_none() && sel.entries.is_empty()) {
        x.line("<SelectedSyncObjects version=\"1\" type=\"SelectedSyncObjects\" isNull=\"1\" />");
        return;
    }
    let mut schemas: Vec<String> = Vec::new();
    for e in &sel.entries {
        if !schemas.contains(&e.schema) {
            schemas.push(e.schema.clone());
        }
    }
    let tokens: Vec<(String, bool)> = schemas.iter().map(|s| crate::project_codec::schema_token(s)).collect();
    let version = if tokens.iter().any(|(_, v2)| *v2) { 2 } else { 1 };
    x.open("SelectedSyncObjects", &format!(" version=\"{version}\" type=\"SelectedSyncObjects\""));
    if tokens.is_empty() {
        x.line("<Schemas type=\"ListString\" version=\"2\" />");
    } else {
        x.open("Schemas", " type=\"ListString\" version=\"2\"");
        for (t, _) in &tokens {
            x.line(&format!("<value type=\"string\">{t}</value>"));
        }
        x.close("Schemas");
    }
    for (schema, (token, _)) in schemas.iter().zip(&tokens) {
        let mut bytes = Vec::new();
        for e in sel.entries.iter().filter(|e| &e.schema == schema) {
            bytes.extend((0..16).map(|i| u8::from_str_radix(e.hash.get(2 * i..2 * i + 2).unwrap_or("00"), 16).unwrap_or(0)));
            bytes.extend([e.object_type, e.difference, e.selected as u8]);
        }
        x.value(token, &crate::project_codec::base64_encode(&bytes).replace('=', "_"));
    }
    // The "select all" group flags, as the SQL Compare window writes them after Select all.
    x.open("Grouping", " type=\"ListByte\" version=\"2\"");
    for b in [63, 0, 0, 0, 0, 0] {
        x.line(&format!("<value type=\"Byte\">{}</value>", if sel.select_all { b } else { 0 }));
    }
    x.close("Grouping");
    x.value("SelectAll", bool_text(sel.select_all));
    x.close("SelectedSyncObjects");
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

    fn sdc(actions: &str, session: &str) -> Project {
        let text = format!(
            r#"<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="5" type="LiveDatabaseSource"><ServerName>a</ServerName><DatabaseName>x</DatabaseName></DataSource1>
  <DataSource2 version="5" type="LiveDatabaseSource"><ServerName>b</ServerName><DatabaseName>y</DatabaseName></DataSource2>
  <TableActions type="ArrayList" version="1">{actions}</TableActions>
  <SessionSettings>{session}</SessionSettings>
</Project>"#
        );
        import_vendor(text.as_bytes(), ProjectKind::Data).unwrap()
    }

    fn ev(action: &str, fields: &[(&str, &str)]) -> String {
        let f: String = fields.iter().map(|(k, v)| format!("<{k}>{v}</{k}>")).collect();
        format!(r#"<value version="1" type="SelectTableEvent"><action>{action}</action>{f}</value>"#)
    }

    #[test]
    fn replays_data_selection_events_in_order() {
        let p = sdc(&[ev("DeselectAll", &[]), ev("SelectItem", &[("val", "[dbo].[A]:[dbo].[A]")]), ev("SelectItem", &[("val", "[dbo].[B]:[dbo].[B]")]), ev("DeselectItem", &[("val", "[dbo].[B]:[dbo].[B]")])].concat(), "14");
        assert!(!p.include_new_tables);
        assert_eq!(p.selected.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["A"]);
        let back = import_vendor(&export_vendor(&p, ""), ProjectKind::Data).unwrap();
        assert_eq!((back.include_new_tables, back.selected.clone()), (false, p.selected.clone()));
        let text = decode_text(&export_vendor(&p, ""));
        assert!(text.contains("<action>DeselectAll</action>") && text.contains("<val>[dbo].[A]:[dbo].[A]</val>"), "{text}");

        let p = sdc(&[ev("DeselectItem", &[("val", "[dbo].[B]:[dbo].[B]")]), ev("DeselectAll", &[]), ev("SelectAll", &[]), ev("DeselectItem", &[("val", "[dbo].[C]:null")])].concat(), "14");
        assert!(p.include_new_tables && p.selected.is_empty());
        let excluded: Vec<&str> = p.data_mappings.tables.iter().filter(|t| t.excluded).map(|t| t.source.name.as_str()).collect();
        assert_eq!(excluded, ["C"]);
    }

    #[test]
    fn reads_pair_names_and_column_events() {
        let t = "[dbo].[Orders]:[dbo].[Orders]";
        let p = sdc(
            &[
                ev("SetLeftWhereClause", &[("TableName", t), ("IndexName", "Region = 'EU'")]),
                ev("SetRightWhereClause", &[("TableName", t), ("IndexName", "Region = 'EU'")]),
                ev("UseCustomKey", &[("val", t)]),
                ev("SelectColumnAsKey", &[("ColumnName", "Code:Code"), ("TableName", t)]),
                ev("SelectColumnAsKey", &[("ColumnName", "Region:Region"), ("TableName", t)]),
                ev("DeselectColumnAsKey", &[("ColumnName", "Region:Region"), ("TableName", t)]),
                ev("DeselectColumn", &[("ColumnName", "Note:Note"), ("TableName", t)]),
                ev("DeselectColumn", &[("ColumnName", "Extra:Extra"), ("TableName", t)]),
                ev("SelectColumn", &[("ColumnName", "Extra:Extra"), ("TableName", t)]),
                ev("UnmapColumns", &[("ColumnName", "Old:Old"), ("TableName", t)]),
                ev("MapColumn", &[("Table1", t), ("Column1", "Amount:null"), ("Table2", t), ("Column2", "null:Total")]),
            ]
            .concat(),
            "6",
        );
        let o = &p.data_mappings.tables[0];
        assert_eq!(o.source, ObjectName::new("dbo", "Orders"));
        assert_eq!((o.where1.as_deref(), o.where2.as_deref()), (Some("Region = 'EU'"), Some("Region = 'EU'")));
        assert_eq!(o.key, ["Code"]);
        assert_eq!(o.excluded_columns, ["Note", "Old"]);
        assert_eq!(o.columns, [("Amount".to_string(), "Total".to_string())]);
        assert_eq!(p.row_kinds, [false, true, true, false]);
        assert!(!p.options.contains(&"IncludeIdenticalRecords".to_string()));
        // Written back in the pair form, with the real SessionSettings.
        let text = decode_text(&export_vendor(&p, ""));
        assert!(text.contains(&format!("<TableName>{t}</TableName>")) && text.contains("<ColumnName>Code:Code</ColumnName>"), "{text}");
        assert!(text.contains("<SessionSettings>6</SessionSettings>"));
        let back = import_vendor(&export_vendor(&p, ""), ProjectKind::Data).unwrap();
        assert_eq!(back.data_mappings.tables[0], *o);
        assert_eq!(back.row_kinds, p.row_kinds);
        assert_eq!(pair_part("[a:b]:[a:b]"), "a:b");
        assert_eq!(split_pair("[x].[a:b]:null"), Some(("[x].[a:b]", "null")));
    }

    #[test]
    fn schema_selection_round_trips_through_selected_sync_objects() {
        let mut p = Project::new(ProjectKind::Schema);
        p.deselected.push(Deselected { kind: "Table".into(), schema: "dbo".into(), name: "Old".into() });
        p.deselected.push(Deselected { kind: "Role".into(), schema: String::new(), name: "Readers".into() });
        let bytes = export_vendor(&p, "");
        let text = decode_text(&bytes);
        assert!(!text.contains("<action>DeselectItem</action>") && text.contains("<SelectAll>True</SelectAll>"), "{text}");
        let back = import_vendor(&bytes, ProjectKind::Schema).unwrap();
        assert!(back.deselected.is_empty());
        assert_eq!(back.imported_selected("dbo", "Old"), Some(false));
        assert_eq!(back.imported_selected("", "Readers"), Some(false));
        assert_eq!(back.imported_selected("dbo", "New"), Some(true));
        assert_eq!(Project::new(ProjectKind::Schema).imported_selected("dbo", "Old"), None);
        // Written again unchanged.
        assert_eq!(import_vendor(&export_vendor(&back, ""), ProjectKind::Schema).unwrap().imported_selection, back.imported_selection);
    }

    #[test]
    fn reads_saved_passwords_and_old_layouts() {
        let saved = crate::project_codec::encrypt_password("pw!");
        let scp = format!(
            r#"<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="5" type="LiveDatabaseSource"><ServerName>a</ServerName><DatabaseName>x</DatabaseName><Username>sa</Username><SavePassword>True</SavePassword><Password encrypted="1">{saved}</Password><AuthenticationType>SqlPassword</AuthenticationType></DataSource1>
  <DataSource2 version="3" type="LiveDatabaseSource"><ServerName>b</ServerName><DatabaseName>y</DatabaseName><Username>u</Username><IntegratedSecurity>False</IntegratedSecurity></DataSource2>
</Project>"#
        );
        let p = import_vendor(scp.as_bytes(), ProjectKind::Schema).unwrap();
        assert_eq!(p.source.password.as_deref(), Some("pw!"));
        assert_eq!((p.target.authentication, p.target.password.as_deref()), (Authentication::Sql, None));
        // Never written back.
        assert!(!decode_text(&export_vendor(&p, "")).contains(&saved));
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("pw!"));
    }

    #[test]
    fn entra_logins_round_trip() {
        for (auth, name) in [(Authentication::EntraPassword, "MicrosoftEntraPassword"), (Authentication::EntraIntegrated, "MicrosoftEntraIntegrated"), (Authentication::EntraInteractive, "MicrosoftEntraWithMfa")] {
            let mut p = Project::new(ProjectKind::Schema);
            p.source = SourceSpec { authentication: auth, user: "ann@contoso.com".into(), ..SourceSpec::database("x.database.windows.net", "Shop") };
            let text = decode_text(&export_vendor(&p, ""));
            assert!(text.contains(&format!("<AuthenticationType>{name}</AuthenticationType>")), "{text}");
            assert_eq!(import_vendor(&export_vendor(&p, ""), ProjectKind::Schema).unwrap().source.authentication, auth);
        }
        let side: SourceSpec = serde_json::from_str(r#"{"type":"database","server":"s","database":"d","authentication":"entraPassword","user":"ann@contoso.com","password":"pw","tenant":"contoso.com"}"#).unwrap();
        let spec = side.connection(1).unwrap();
        match spec.auth {
            Auth::Entra(l) => assert_eq!((l.method, l.user.as_str(), l.password.as_str(), l.tenant.as_str()), (crate::entra::Method::Password, "ann@contoso.com", "pw", "contoso.com")),
            other => panic!("{other:?}"),
        }
        assert!(serde_json::to_string(&side).unwrap().contains("\"authentication\":\"entraPassword\""));
    }


    #[test]
    fn old_layouts_are_read() {
        let scp = r#"<Project version="9" type="SQLCompareProject">
  <DataSource1 version="3" type="LiveDatabaseSource"><ServerName>a</ServerName><DatabaseName>x</DatabaseName><IntegratedSecurity>True</IntegratedSecurity></DataSource1>
  <DataSource2 version="3" type="LiveDatabaseSource"><ServerName>b</ServerName><DatabaseName>y</DatabaseName></DataSource2>
  <Options>2056</Options>
  <ObjectSelectionUserActions type="ArrayList" version="1">
    <value version="1" type="UserActionItem"><action>DeselectAll</action></value>
    <value version="1" type="UserActionItem"><action>SelectItem</action><val>[dbo].[Keep]</val></value>
  </ObjectSelectionUserActions>
  <OwnerMappingUserActions type="ArrayList" version="1">
    <value version="1" type="UserActionItem"><action>MapOwners</action><Owner1>dev</Owner1><Owner2>dbo</Owner2></value>
  </OwnerMappingUserActions>
</Project>"#;
        let p = import_vendor(scp.as_bytes(), ProjectKind::Schema).unwrap();
        assert_eq!(p.source.authentication, Authentication::Windows);
        // 2056 = IgnoreWhiteSpace (8) + IgnoreComments (0x800).
        let mut o = p.options.clone();
        o.sort();
        assert_eq!(o, ["IgnoreComments", "IgnoreWhiteSpace"]);
        assert_eq!(p.schema_mappings.schemas, [("dev".to_string(), "dbo".to_string())]);
        assert_eq!((p.imported_selected("dbo", "Keep"), p.imported_selected("dbo", "Other")), (Some(true), Some(false)));
    }

    #[test]
    fn sca_and_source_control_sides_round_trip() {
        let location = "&lt;?xml version=\"1.0\"?&gt;&lt;ISOCCompareLocation version=\"1\" type=\"GitLocation\"&gt;&lt;RepositoryUrl&gt;C:\\repo&lt;/RepositoryUrl&gt;&lt;/ISOCCompareLocation&gt;";
        let scp = format!(
            r#"<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="1" type="ScaDataSource"><ProjectFilePath>C:\p\Db.sqlproj</ProjectFilePath><DatabaseName>Db</DatabaseName><ServerName /></DataSource1>
  <DataSource2 version="4" type="SourceControlDataSource"><VersionType>Specific</VersionType><SpecificVersion>abc123</SpecificVersion><ServerName>s</ServerName><DatabaseName>d</DatabaseName><ScriptFolderLocation>{location}</ScriptFolderLocation></DataSource2>
</Project>"#
        );
        let p = import_vendor(scp.as_bytes(), ProjectKind::Schema).unwrap();
        assert!(p.source.is_sca() && p.source.is_scripts() && p.source.path == r"C:\p\Db.sqlproj");
        assert!(p.target.is_scripts() && !p.target.is_sca());
        assert_eq!(p.target.revision, "abc123");
        assert!(p.target.location_xml.contains("GitLocation"));
        let text = decode_text(&export_vendor(&p, ""));
        assert!(text.contains("type=\"ScaDataSource\"") && text.contains("<ProjectFilePath>C:\\p\\Db.sqlproj</ProjectFilePath>"), "{text}");
        assert!(text.contains("type=\"SourceControlDataSource\"") && text.contains("<VersionType>Specific</VersionType>") && text.contains("<SpecificVersion>abc123</SpecificVersion>"), "{text}");
        let back = import_vendor(text.as_bytes(), ProjectKind::Schema).unwrap();
        assert_eq!((back.source.clone(), back.target.revision.clone(), back.target.location_xml.clone()), (p.source.clone(), p.target.revision.clone(), p.target.location_xml.clone()));
        // Older versions: newest revision, or version 2's VersionToUse 0.
        let v1 = r#"<Project><DataSource1 version="1" type="SourceControlDataSource"><ServerName>s</ServerName><DatabaseName>d</DatabaseName><Revision>7</Revision><UseNewestRevision>True</UseNewestRevision></DataSource1></Project>"#;
        assert_eq!(import_vendor(v1.as_bytes(), ProjectKind::Schema).unwrap().source.revision, "HEAD");
        let v2 = r#"<Project><DataSource1 version="2" type="SourceControlDataSource"><VersionToUse>1</VersionToUse><SpecificVersion>9</SpecificVersion></DataSource1></Project>"#;
        assert_eq!(import_vendor(v2.as_bytes(), ProjectKind::Schema).unwrap().source.revision, "9");
    }

}
