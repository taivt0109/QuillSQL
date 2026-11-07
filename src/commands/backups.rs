//! Command-line side of backups as a data source: `/backup1|2` (files separated by `;`),
//! `/backuppasswords1|2` (one per file, separated by `,`) and `/backupset1|2`.

use std::path::PathBuf;

use super::common::Source;
use super::console;
use crate::args::{ArgError, Args, exit};
use crate::catalog::Database;
use crate::sources::backup::{self, Backup, BackupError};

/// The backup files of one side and the set to read from them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupSpec {
    pub files: Vec<PathBuf>,
    pub passwords: Vec<String>,
    /// The backup set's name; the latest set when `None`.
    pub set: Option<String>,
}

impl BackupSpec {
    pub fn describe(&self) -> String {
        self.files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(";")
    }
}

/// `/backupN`, `/backuppasswordsN` and `/backupsetN`; `None` when side N is something else.
pub fn source(args: &Args, n: u8) -> Result<Option<Source>, ArgError> {
    let key = |name: &str| format!("{name}{n}");
    let Some(list) = args.get(&key("backup")) else { return Ok(None) };
    let files: Vec<PathBuf> = list.split(';').map(str::trim).filter(|f| !f.is_empty()).map(PathBuf::from).collect();
    if files.is_empty() {
        return Err(ArgError::new(exit::USAGE, format!("You must specify at least one backup file when you use the /backup{n} switch")));
    }
    let passwords: Vec<String> = match args.get(&key("backuppasswords")) {
        Some(p) => p.split(',').map(str::to_string).collect(),
        None => Vec::new(),
    };
    if !passwords.is_empty() && passwords.len() != files.len() {
        return Err(ArgError::new(exit::USAGE, "The number of passwords specified should equal the number of backup files supplied."));
    }
    let set = args.get(&key("backupset")).map(str::to_string);
    Ok(Some(Source::Backup(BackupSpec { files, passwords, set })))
}

/// The reader's errors as the command line reports them.
pub fn error(e: BackupError) -> ArgError {
    match e {
        BackupError::Io(m) => ArgError::new(exit::IO_ERROR, m),
        BackupError::Password(m) => {
            ArgError::new(exit::DATA_ERROR, format!("No passwords were supplied or they were not correct for the encrypted backup files - '{m}.'"))
        }
        BackupError::Reader(m) if m == "No backup sets found." || m.starts_with("The specified backup set") => ArgError::new(exit::DATA_ERROR, m),
        BackupError::Reader(m) => ArgError::new(exit::DATA_ERROR, format!("The backup reader reported the following error: '{m}'")),
    }
}

pub fn open(spec: &BackupSpec) -> Result<Backup, ArgError> {
    console::verbose(&format!("Reading backup files {}...", spec.describe()));
    backup::open(&spec.files, &spec.passwords, spec.set.as_deref()).map_err(error)
}

/// Reads the database model of the backup set.
pub fn load(spec: &BackupSpec) -> Result<Database, ArgError> {
    open(spec)?.read_database().map_err(error)
}
