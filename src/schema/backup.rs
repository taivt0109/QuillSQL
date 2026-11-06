//! "Back up target before deployment": a native SQL Server backup of the target database, run
//! just before the deployment script, with the options of SQL Compare's backup page.

use serde::Deserialize;

use crate::db::{self, ConnectionSpec};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackupOptions {
    /// A folder on the SQL Server machine; empty = the instance's default backup folder.
    pub folder: String,
    /// The file name without the folder; empty (or `name_automatically`) = the automatic name.
    pub file: String,
    pub name_automatically: bool,
    /// Overwrite the backup sets already in the file (WITH INIT) rather than add to them.
    pub overwrite: bool,
    pub differential: bool,
}

/// `FULL_<server>_<database>_<yyyymmdd>_<hhmmss>.bak`, or `DIFF_…`; characters a file name
/// cannot hold become `_`.
pub fn automatic_name(server: &str, database: &str, differential: bool, stamp: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | ',') { '_' } else { c }).collect::<String>();
    format!("{}_{}_{}_{stamp}.bak", if differential { "DIFF" } else { "FULL" }, clean(server), clean(database))
}

/// The statement for a backup to `path`.
pub fn statement(database: &str, path: &str, o: &BackupOptions) -> String {
    let mut with = vec![if o.overwrite { "INIT" } else { "NOINIT" }.to_string()];
    if o.differential {
        with.insert(0, "DIFFERENTIAL".into());
    }
    with.push(format!("NAME = {}", db::literal(&format!("{database} - backup before deployment"))));
    format!("BACKUP DATABASE {} TO DISK = {} WITH {}", db::quote(database), db::literal(path), with.join(", "))
}

/// The instance's default backup folder.
pub async fn default_folder(conn: &mut db::Connection) -> Result<String, String> {
    let rows = db::query(conn, "SELECT CAST(SERVERPROPERTY('InstanceDefaultBackupPath') AS nvarchar(4000))").await?;
    Ok(rows.first().and_then(|r| r.get::<&str, _>(0)).unwrap_or_default().to_string())
}

/// Backs the database of `spec` up; returns the file written.
pub fn run(spec: &ConnectionSpec, o: &BackupOptions, stamp: &str, progress: &dyn Fn(&str)) -> Result<String, String> {
    let rt = db::runtime()?;
    rt.block_on(async {
        let mut c = db::connect_async(spec).await?;
        let database = db::query(&mut c, "SELECT DB_NAME()").await?.first().and_then(|r| r.get::<&str, _>(0).map(str::to_string)).ok_or("no database")?;
        let folder = if o.folder.trim().is_empty() { default_folder(&mut c).await? } else { o.folder.trim().to_string() };
        if folder.is_empty() {
            return Err("Folder cannot be empty".to_string());
        }
        let file = if o.name_automatically || o.file.trim().is_empty() { automatic_name(&spec.server, &database, o.differential, stamp) } else { o.file.trim().to_string() };
        let sep = if folder.contains('\\') || !folder.starts_with('/') { '\\' } else { '/' };
        let path = format!("{}{sep}{file}", folder.trim_end_matches(['\\', '/']));
        progress(&format!("Backing up {database} to {path}"));
        db::execute(&mut c, &statement(&database, &path, o)).await.map_err(|e| format!("The backup of {database} failed, so nothing was deployed: {e}"))?;
        Ok(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_statements() {
        assert_eq!(automatic_name("srv\\inst,1433", "Sales", false, "20261107_101500"), "FULL_srv_inst_1433_Sales_20261107_101500.bak");
        let o = BackupOptions { differential: true, ..Default::default() };
        assert_eq!(
            statement("Sales", "C:\\b\\x.bak", &o),
            "BACKUP DATABASE [Sales] TO DISK = N'C:\\b\\x.bak' WITH DIFFERENTIAL, NOINIT, NAME = N'Sales - backup before deployment'"
        );
        assert!(statement("a]b", "p", &BackupOptions { overwrite: true, ..Default::default() }).starts_with("BACKUP DATABASE [a]]b] TO DISK = N'p' WITH INIT"));
    }
}
