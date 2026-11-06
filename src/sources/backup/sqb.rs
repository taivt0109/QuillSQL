//! SQL Backup (.sqb) files.

use std::path::Path;
use std::sync::Arc;

use super::BackupError;
use super::mtf::Medium;

pub fn is_sqb(_path: &Path) -> bool {
    false
}

pub fn open(path: &Path, _passwords: &[String]) -> Result<Vec<Arc<dyn Medium>>, BackupError> {
    Err(BackupError::Reader(format!("{}: SQL Backup files are not supported yet", path.display())))
}
