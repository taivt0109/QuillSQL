//! SQL Backup (.sqb) files: recognized by the signature in their header so that they are
//! reported clearly; reading their compressed (and possibly encrypted) extents is not
//! implemented yet.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use super::BackupError;
use super::mtf::Medium;

/// A SQL Backup file has `SSBE` or `SSBF` at offset 508 of its first kilobyte.
pub fn is_sqb(path: &Path) -> bool {
    let mut head = [0u8; 1024];
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    matches!(&head[508..512], b"SSBE" | b"SSBF")
}

pub fn open(path: &Path, _passwords: &[String]) -> Result<Vec<Arc<dyn Medium>>, BackupError> {
    Err(BackupError::Reader(format!("The file '{}' is a SQL Backup file; reading SQL Backup files is not supported yet", path.display())))
}
