//! "Back up target before deployment", as SQL Compare does it: the backup is the first batch of
//! the deployment script (after the comment header), with a commented restore command at the
//! end. Native SQL Server backups or Redgate SQL Backup through its extended procedure.

use serde::{Deserialize, Serialize};

use super::deploy::DeploymentScript;
use crate::db::{self, ConnectionSpec};

/// The backup settings of a project (SQL Compare's `SC_DeploymentOptions/BackupOptions`).
/// Passwords are never kept here: they come with the request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackupOptions {
    pub enabled: bool,
    /// `native` or `sqb` (Redgate SQL Backup).
    pub provider: String,
    pub differential: bool,
    /// A folder as the SQL Server sees it; empty = SQL Server's default backup folder.
    pub folder: String,
    pub file: String,
    pub name_automatically: bool,
    /// WITH INIT: overwrite the backup sets already in the file.
    pub overwrite: bool,
    /// SQL Backup: 0 (none) to 4.
    pub compression_level: u8,
    /// SQL Backup: `none`, `aes128` or `aes256`.
    pub encryption: String,
    /// SQL Backup: threads; 1 or less uses one.
    pub thread_count: u8,
    #[serde(skip_serializing)]
    pub password: Option<String>,
}

impl Default for BackupOptions {
    fn default() -> Self {
        BackupOptions {
            enabled: false,
            provider: "native".into(),
            differential: false,
            folder: String::new(),
            file: String::new(),
            name_automatically: true,
            overwrite: false,
            compression_level: 1,
            encryption: "none".into(),
            thread_count: 1,
            password: None,
        }
    }
}

impl BackupOptions {
    pub fn sqb(&self) -> bool {
        self.provider.eq_ignore_ascii_case("sqb")
    }
}

/// What a server offers for backups.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub native_folder: String,
    /// The SQL Backup version; 0 when it is not installed.
    pub sqb_version: i32,
    pub sqb_folder: String,
    pub managed_instance: bool,
}

/// A backup to add to a script: the settings, the database, the time it is named after (UTC,
/// `yyyy-MM-dd HH mm ss`) and what the server offers.
pub struct Plan<'a> {
    pub options: &'a BackupOptions,
    pub database: &'a str,
    pub utc: String,
    pub caps: &'a Capabilities,
}

fn bracket(s: &str) -> String {
    s.replace(']', "]]")
}

fn sql_text(s: &str) -> String {
    s.replace('\'', "''")
}

/// Quotes inside the SQL Backup command, itself inside a string.
fn nested(s: &str) -> String {
    s.replace('\'', "''''")
}

/// SQL Compare joins the folder and the file with a backslash; a Linux folder keeps its slashes.
fn combine(folder: &str, file: &str) -> String {
    if folder.is_empty() || folder.ends_with('\\') || folder.ends_with('/') {
        format!("{folder}{file}")
    } else if folder.starts_with('/') {
        format!("{folder}/{file}")
    } else {
        format!("{folder}\\{file}")
    }
}

fn with_extension(file: &str, ext: &str) -> String {
    let has = std::path::Path::new(file).extension().is_some_and(|e| format!(".{}", e.to_string_lossy()).eq_ignore_ascii_case(ext));
    if has { file.to_string() } else { format!("{file}{ext}") }
}

impl Plan<'_> {
    fn database(&self) -> &str {
        if self.database.is_empty() { "<enter your database name here>" } else { self.database }
    }

    /// `<database>.<yyyy-MM-dd HH mm ss>Z.bak` for native backups, `<AUTO>.sqb` (SQL Backup's own
    /// naming) for SQL Backup, or the given file with its extension.
    pub fn file(&self) -> String {
        let o = self.options;
        if o.sqb() {
            return if o.name_automatically || o.file.is_empty() { "<AUTO>.sqb".into() } else { with_extension(&o.file, ".sqb") };
        }
        if o.name_automatically || o.file.is_empty() {
            format!("{}.{}Z.bak", if self.database.is_empty() { "BACKUP" } else { self.database }, self.utc)
        } else {
            with_extension(&o.file, ".bak")
        }
    }

    fn path(&self) -> String {
        combine(&self.options.folder, &self.file())
    }

    /// The batch that backs the database up.
    pub fn sql(&self) -> String {
        let o = self.options;
        let db = bracket(self.database());
        if !o.sqb() {
            let mut s = format!("-- Backs up the target database using native SQL Server backup\r\nBACKUP DATABASE [{db}]");
            if self.caps.managed_instance {
                s.push_str(&format!(" TO URL='{}' WITH COPY_ONLY", sql_text(&o.folder)));
                return s;
            }
            s.push_str(&format!(" TO DISK='{}'", sql_text(&self.path())));
            let mut with = Vec::new();
            if o.overwrite {
                with.push("INIT");
            }
            if o.differential {
                with.push("DIFFERENTIAL");
            }
            if !with.is_empty() {
                s.push_str(&format!(" WITH {}", with.join(",")));
            }
            return s;
        }
        let mut with = vec![format!("COMPRESSION={}", o.compression_level)];
        if o.encryption != "none" && !o.encryption.is_empty() {
            with.push(format!("KEYSIZE={}", if o.encryption == "aes128" { 128 } else { 256 }));
            with.push(format!("PASSWORD=''{}''", nested(o.password.as_deref().unwrap_or_default())));
        }
        if o.thread_count > 1 {
            with.push(format!("{}={}", if self.caps.sqb_version >= 5 { "THREADCOUNT" } else { "THREADS" }, o.thread_count));
        }
        if o.overwrite {
            with.push("INIT".into());
        }
        if o.differential {
            with.push("DIFFERENTIAL".into());
        }
        format!(
            "-- >Backs up the target database using Red Gate SQL Backup\r\nDECLARE @sqlErrorCode int, @exitCode int\r\nEXECUTE master..sqlbackup N'-SQL \"BACKUP DATABASE [{db}] TO DISK=''{}'' WITH {}\"', @exitCode OUTPUT, @sqlErrorCode OUTPUT\r\nIF @exitCode >= 500 OR @sqlErrorCode <> 0 BEGIN\r\n\tRAISERROR ('Backup failed\r\nSQL Backup error: %d\r\nSQL Server error: %d\r\n\r\nFor more information see http://www.red-gate.com/sqlbackup/latesthelp/errorcodes or the Microsoft SQL Server documentation.', 16, 1, @exitCode, @sqlErrorCode)\r\n\tSET NOEXEC ON\r\nEND\r\n",
            self.path(),
            with.join(", ")
        )
    }

    /// The commented restore command at the end of the script.
    pub fn restore_comment(&self) -> String {
        let o = self.options;
        let db = bracket(self.database());
        let mut s = String::new();
        if !o.sqb() {
            s.push_str("--Native Backup Restore command start\r\n--To Restore the database uncomment the following line\r\n");
            let with = if o.differential {
                s.push_str(&format!("--RESTORE DATABASE [{db}] FROM DISK='<enter your file name for full backup.bak>' WITH NORECOVERY\r\n--GO\r\n"));
                "RECOVERY"
            } else {
                "REPLACE"
            };
            if self.caps.managed_instance {
                s.push_str(&format!("--RESTORE DATABASE [{db}] FROM URL='{}'\r\n", sql_text(&o.folder)));
            } else {
                s.push_str(&format!("--RESTORE DATABASE [{db}] FROM DISK='{}' WITH {with}\r\n", sql_text(&self.path())));
            }
            s.push_str("--Native Backup Restore command finish\r\n");
            return s;
        }
        let cmd = |sql: String| format!("--EXECUTE master..sqlbackup N'-SQL \"{sql}\"'\r\n");
        let mut with = vec![if o.differential { "RECOVERY".to_string() } else { "REPLACE".to_string() }];
        if o.encryption != "none" && !o.encryption.is_empty() {
            with.push(format!("PASSWORD=''{}''", nested(o.password.as_deref().unwrap_or_default())));
        }
        let with = with.join(", ");
        let v6 = self.caps.sqb_version >= 6;
        let all = nested(&combine(&o.folder, "*.sqb"));
        s.push_str("--SQL Backup Restore command start\r\n--To Restore the database uncomment the following line\r\n");
        if o.name_automatically || o.file.is_empty() {
            if !o.differential && v6 {
                s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''{all}'' LATEST_FULL WITH {with}")));
            } else if !o.differential {
                s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''{}'' WITH {with}", nested(&combine(&o.folder, "<enter matching wild card here*.sqb>")))));
            } else if v6 {
                s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''<enter your folder for full backup>'' LATEST_FULL WITH NORECOVERY")));
                s.push_str("-- GO\r\n");
                s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''{all}'' LATEST_DIFF WITH {with}")));
            } else {
                s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''<enter your file for full backup.sqb>'' WITH NORECOVERY")));
                s.push_str("--GO\r\n");
                s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''<enter your file for differential backup.sqb>'' WITH {with}")));
            }
        } else {
            if o.differential {
                let full = if v6 { "<enter your folder for full backup>'' LATEST_FULL" } else { "<enter your file for full backup.sqb>''" };
                s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''{full} WITH NORECOVERY")));
                s.push_str("--GO\r\n");
            }
            s.push_str(&cmd(format!("RESTORE DATABASE [{db}] FROM DISK=''{}'' WITH {with}", nested(&self.path()))));
            s.push_str("--GO\r\n");
        }
        s.push_str("--SQL Backup Restore command finish\r\n");
        s
    }

    /// Puts the backup first (after the comment header) and the restore command last.
    pub fn add_to(&self, script: &mut DeploymentScript) {
        let batch = self.sql();
        let at = if script.sql.starts_with("/*") { script.sql.find("*/").map(|i| i + 2).unwrap_or(0) } else { 0 };
        let at = if script.sql[at..].starts_with("\r\n") { at + 2 } else { at };
        script.sql.insert_str(at, &format!("{}\r\nGO\r\n", batch.trim_end_matches(['\r', '\n'])));
        script.batches.insert(0, batch);
        script.sql.push_str(&self.restore_comment());
        // With a backup taken first, running without transactions is no longer worth a warning.
        let no_transactions = super::warnings::no_transactions().message;
        script.warnings.retain(|w| w.message != no_transactions);
    }
}

/// `yyyy-MM-dd HH mm ss` in UTC, the time automatic native backup names carry.
pub fn utc_stamp() -> String {
    let n = crate::commands::console::utc(std::time::SystemTime::now());
    format!("{:04}-{:02}-{:02} {:02} {:02} {:02}", n.year, n.month, n.day, n.hour, n.minute, n.second)
}

/// Asks the server of `spec` what it offers for backups.
pub fn capabilities(spec: &ConnectionSpec) -> Result<Capabilities, String> {
    let rt = db::runtime()?;
    rt.block_on(async {
        let mut c = db::connect_async(spec).await?;
        let mut caps = Capabilities::default();
        // The folder SQL Server backs up to by default (as SQL Compare reads it).
        let folder = db::query(
            &mut c,
            "DECLARE @d nvarchar(4000); BEGIN TRY EXEC master.dbo.xp_instance_regread N'HKEY_LOCAL_MACHINE', N'Software\\Microsoft\\MSSQLServer\\MSSQLServer', N'BackupDirectory', @d OUTPUT END TRY BEGIN CATCH END CATCH; \
             SELECT COALESCE(@d, CAST(SERVERPROPERTY('InstanceDefaultBackupPath') AS nvarchar(4000)), N''), CAST(SERVERPROPERTY('EngineEdition') AS int)",
        )
        .await?;
        if let Some(r) = folder.first() {
            caps.native_folder = r.get::<&str, _>(0).unwrap_or_default().to_string();
            caps.managed_instance = r.get::<i32, _>(1) == Some(8);
        }
        // SQL Backup is installed when master has its extended procedure.
        let installed = db::query(&mut c, "SELECT COUNT(*) FROM master.dbo.sysobjects WHERE name = 'sqbutility' AND OBJECTPROPERTY(id, N'IsExtendedProc') = 1").await?;
        if installed.first().and_then(|r| r.get::<i32, _>(0)).unwrap_or(0) > 0 {
            let r = db::query(
                &mut c,
                "DECLARE @LicenseType int, @Version int, @SerialNumber nvarchar(64), @DateInstalled datetime, @DateFirstUsed datetime, @RevokeErrorsv4 int, @Warningsv5 nvarchar(4000), @Settings nvarchar(4000); \
                 BEGIN TRY EXECUTE master..sqbutility 1021, @LicenseType OUTPUT, @Version OUTPUT, @SerialNumber OUTPUT, @DateInstalled OUTPUT, @DateFirstUsed OUTPUT, @RevokeErrorsv4 OUTPUT, @Warningsv5 OUTPUT END TRY BEGIN CATCH END CATCH; \
                 BEGIN TRY EXECUTE master.dbo.sqbutility 1008, @Settings OUTPUT END TRY BEGIN CATCH END CATCH; \
                 SELECT COALESCE(@Version, 1), @Settings",
            )
            .await?;
            if let Some(r) = r.first() {
                caps.sqb_version = r.get::<i32, _>(0).unwrap_or(1).max(1);
                let settings = r.get::<&str, _>(1).unwrap_or_default();
                caps.sqb_folder = settings.split("\r\n").filter_map(|l| l.split_once('=')).find(|(k, _)| *k == "BackupFolder").map(|(_, v)| v.to_string()).unwrap_or_else(|| caps.native_folder.clone());
            }
        }
        Ok(caps)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> DeploymentScript {
        DeploymentScript { sql: "/*\r\nheader\r\n*/\r\nSET NUMERIC_ROUNDABORT OFF\r\nGO\r\n".into(), warnings: Vec::new(), batches: vec!["SET NUMERIC_ROUNDABORT OFF".into()] }
    }

    #[test]
    fn native_backup_in_the_script() {
        let caps = Capabilities::default();
        let o = BackupOptions { enabled: true, folder: "C:\\Backups".into(), overwrite: true, differential: true, ..Default::default() };
        let p = Plan { options: &o, database: "Sales", utc: "2026-11-07 10 15 00".into(), caps: &caps };
        assert_eq!(p.file(), "Sales.2026-11-07 10 15 00Z.bak");
        assert_eq!(p.sql(), "-- Backs up the target database using native SQL Server backup\r\nBACKUP DATABASE [Sales] TO DISK='C:\\Backups\\Sales.2026-11-07 10 15 00Z.bak' WITH INIT,DIFFERENTIAL");
        let mut s = script();
        p.add_to(&mut s);
        assert!(s.sql.starts_with("/*\r\nheader\r\n*/\r\n-- Backs up"), "{}", s.sql);
        assert!(s.sql.contains("WITH INIT,DIFFERENTIAL\r\nGO\r\nSET NUMERIC_ROUNDABORT OFF"));
        assert!(s.sql.ends_with("--RESTORE DATABASE [Sales] FROM DISK='C:\\Backups\\Sales.2026-11-07 10 15 00Z.bak' WITH RECOVERY\r\n--Native Backup Restore command finish\r\n"));
        assert_eq!(s.batches.len(), 2);
        let named = BackupOptions { name_automatically: false, file: "mine".into(), ..Default::default() };
        let p = Plan { options: &named, database: "O'Brien", utc: String::new(), caps: &caps };
        assert_eq!(p.sql(), "-- Backs up the target database using native SQL Server backup\r\nBACKUP DATABASE [O'Brien] TO DISK='mine.bak'");
    }

    #[test]
    fn kept_in_projects() {
        use crate::project::{Project, ProjectKind, export_redgate, import_redgate};
        let mut p = Project::new(ProjectKind::Schema);
        p.backup = BackupOptions {
            enabled: true,
            provider: "sqb".into(),
            differential: true,
            folder: "D:\\b".into(),
            name_automatically: false,
            file: "x.sqb".into(),
            compression_level: 2,
            encryption: "aes128".into(),
            thread_count: 3,
            password: Some("secret".into()),
            ..Default::default()
        };
        let bytes = export_redgate(&p, "11/07/2026 10:00:00");
        let text = String::from_utf16_lossy(&bytes[2..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>());
        assert!(text.contains("<BackupProvider>Sqb</BackupProvider>") && !text.contains("secret"), "{text}");
        let back = import_redgate(&bytes, ProjectKind::Schema).unwrap();
        assert_eq!(back.backup, BackupOptions { password: None, ..p.backup.clone() });
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"backup\"") && !json.contains("secret"));
        assert!(!serde_json::to_string(&Project::new(ProjectKind::Schema)).unwrap().contains("backup"));
    }

    #[test]
    fn sql_backup_command() {
        let caps = Capabilities { sqb_version: 10, ..Default::default() };
        let o = BackupOptions {
            enabled: true,
            provider: "sqb".into(),
            folder: "D:\\SQB".into(),
            compression_level: 3,
            encryption: "aes256".into(),
            password: Some("p'w".into()),
            thread_count: 4,
            overwrite: true,
            ..Default::default()
        };
        let p = Plan { options: &o, database: "Sales", utc: String::new(), caps: &caps };
        assert_eq!(
            p.sql(),
            "-- >Backs up the target database using Red Gate SQL Backup\r\nDECLARE @sqlErrorCode int, @exitCode int\r\nEXECUTE master..sqlbackup N'-SQL \"BACKUP DATABASE [Sales] TO DISK=''D:\\SQB\\<AUTO>.sqb'' WITH COMPRESSION=3, KEYSIZE=256, PASSWORD=''p''''w'', THREADCOUNT=4, INIT\"', @exitCode OUTPUT, @sqlErrorCode OUTPUT\r\nIF @exitCode >= 500 OR @sqlErrorCode <> 0 BEGIN\r\n\tRAISERROR ('Backup failed\r\nSQL Backup error: %d\r\nSQL Server error: %d\r\n\r\nFor more information see http://www.red-gate.com/sqlbackup/latesthelp/errorcodes or the Microsoft SQL Server documentation.', 16, 1, @exitCode, @sqlErrorCode)\r\n\tSET NOEXEC ON\r\nEND\r\n"
        );
        assert_eq!(
            p.restore_comment(),
            "--SQL Backup Restore command start\r\n--To Restore the database uncomment the following line\r\n--EXECUTE master..sqlbackup N'-SQL \"RESTORE DATABASE [Sales] FROM DISK=''D:\\SQB\\*.sqb'' LATEST_FULL WITH REPLACE, PASSWORD=''p''''w''\"'\r\n--SQL Backup Restore command finish\r\n"
        );
        let old = Capabilities { sqb_version: 4, ..Default::default() };
        let p = Plan { options: &BackupOptions { provider: "sqb".into(), thread_count: 2, differential: true, ..Default::default() }, database: "S", utc: String::new(), caps: &old };
        assert!(p.sql().contains("WITH COMPRESSION=1, THREADS=2, DIFFERENTIAL\"'"));
    }
}
