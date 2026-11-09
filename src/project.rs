//! Compare projects: `.qsc` (schema) and `.qdc` (data) files. JSON, readable in a diff, and
//! never holding a password: SQL logins keep only a "remember" flag, the password itself goes
//! to Windows Credential Manager (written by the UI) or comes from the environment.

use serde::{Deserialize, Serialize};

use crate::data::mapping::DataMappings;
use crate::db::{Auth, ConnectionSpec};
use crate::schema::compare::Mappings;

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
    /// new tables"). When false only `selected` tables are compared.
    #[serde(skip_serializing_if = "is_true")]
    pub include_new_tables: bool,
    /// Data projects with `include_new_tables` off: the tables to compare.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected: Vec<Deselected>,
    /// Data projects: the row kinds compared and deployed, in `RowKind` order (identical,
    /// different, only in 1, only in 2). Identical rows are also on with the IncludeIdenticalRecords option.
    #[serde(skip_serializing_if = "is_default_kinds")]
    pub row_kinds: [bool; 4],
    /// Back up target before deployment (SQL Compare's `SC_DeploymentOptions/BackupOptions`).
    #[serde(skip_serializing_if = "is_disabled")]
    pub backup: crate::schema::backup::BackupOptions,
    pub note: String,
    /// The project's filter (its `ProjectFilter` XML).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
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

    pub fn load(path: &std::path::Path) -> Result<Project, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        if ext == "scp" || ext == "sdc" {
            return Err(format!("{}: only QuillSQL projects (.qsc, .qdc) can be opened", path.display()));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
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
        let other = Project::load(&dir.join("p.scp")).unwrap_err();
        assert!(other.contains("only QuillSQL projects (.qsc, .qdc) can be opened"), "{other}");
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
    fn entra_logins_round_trip() {
        for (auth, name) in [(Authentication::EntraPassword, "entraPassword"), (Authentication::EntraIntegrated, "entraIntegrated"), (Authentication::EntraInteractive, "entraInteractive")] {
            let mut p = Project::new(ProjectKind::Schema);
            p.source = SourceSpec { authentication: auth, user: "ann@contoso.com".into(), ..SourceSpec::database("x.database.windows.net", "Shop") };
            let json = serde_json::to_string(&p).unwrap();
            assert!(json.contains(&format!("\"authentication\":\"{name}\"")), "{json}");
            assert_eq!(serde_json::from_str::<Project>(&json).unwrap().source.authentication, auth);
        }
        let side: SourceSpec = serde_json::from_str(r#"{"type":"database","server":"s","database":"d","authentication":"entraPassword","user":"ann@contoso.com","password":"pw","tenant":"contoso.com"}"#).unwrap();
        let spec = side.connection(1).unwrap();
        match spec.auth {
            Auth::Entra(l) => assert_eq!((l.method, l.user.as_str(), l.password.as_str(), l.tenant.as_str()), (crate::entra::Method::Password, "ann@contoso.com", "pw", "contoso.com")),
            other => panic!("{other:?}"),
        }
        assert!(serde_json::to_string(&side).unwrap().contains("\"authentication\":\"entraPassword\""));
    }

}
