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
    /// Snapshot file, for `type = snapshot`.
    pub path: String,
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

    /// `server / database` or the snapshot path.
    pub fn describe(&self) -> String {
        if self.is_snapshot() { self.path.clone() } else { format!("{} / {}", self.server, self.database) }
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
    pub note: String,
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

fn decode_text(bytes: &[u8]) -> String {
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
            use crate::data::DataOpt::*;
            let int = |name: &str| text_of(root, name).parse::<u64>().ok();
            let mut opts = Vec::new();
            let bits = |value: Option<u64>, table: &[(u64, crate::data::DataOpt)], out: &mut Vec<String>| {
                if let Some(v) = value {
                    for (bit, o) in table {
                        if v & bit != 0 {
                            out.push(o.name().to_string());
                        }
                    }
                }
            };
            let mapping = int("MappingOptions").or(Some(0x52));
            bits(mapping, &[(0x1, CaseSensitiveObjectDefinition), (0x2, IgnoreSpaces), (0x4, IgnoreUnderscores), (0x10, IncludeTimestamps), (0x20, IncludeViews), (0x40, IncludeIdentities), (0x200, IncludeGeneratedAlwaysColumns)], &mut opts);
            bits(int("ComparisonOptions"), &[(0x1, TrimTrailingSpaces), (0x2, ForceBinaryCollation), (0x4, UseChecksumComparison), (0x8, CompressTemporaryFiles), (0x10, TreatEmptyStringAsNull), (0x20, UseFullPrecisionForComparison)], &mut opts);
            bits(int("SQLOptions").or(Some(0xA)), &[(0x1, DisableTriggers), (0x2, DisableKeys), (0x4, DropConstraintsAndIndexes), (0x8, OutputCommentHeader), (0x10, DontUseTransactions), (0x20, TransportClrBinary), (0x40, DisableDdlTriggers), (0x80, DontOutputComments), (0x100, ReseedIdentity), (0x400, SkipFkChecks), (0x800, AddDatabaseUseStatement)], &mut opts);
            if int("SessionSettings").is_some_and(|s| s & 0x1 != 0) {
                opts.push(IncludeIdenticalRecords.name().to_string());
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
    Ok(p)
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
}
