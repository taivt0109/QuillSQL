//! SQL Server backups as a comparison source, read directly from the backup files: native
//! backups (compressed or not, striped over several files, a differential over its full
//! backup) and .sqb backup files. The pages of the backup set are read like the database's own
//! files; the system base tables give the catalog and the tables give the data.

mod aes;
mod binxml;
pub mod catalog;
pub mod collation;
pub mod compressed;
mod events;
mod inflate;
mod lzma;
pub mod mtf;
pub mod pages;
pub mod record;
pub mod rows;
mod sha256;
pub mod sqb;
pub mod tables;
mod text;
pub mod values;
mod xpress;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mtf::{BackupType, Medium, SetPart};
use pages::{PageId, PageStore};

/// What can go wrong reading backups, with the reader's messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupError {
    /// A file is missing or cannot be read.
    Io(String),
    /// Encrypted .sqb backup files and no matching password.
    Password(String),
    /// Anything the reader reports about the backup itself.
    Reader(String),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Io(m) | BackupError::Password(m) | BackupError::Reader(m) => f.write_str(m),
        }
    }
}

/// A backup set across the files given: the stripes of one backup grouped together.
#[derive(Clone, Debug)]
pub struct BackupSet {
    pub name: String,
    pub database: String,
    pub server: String,
    pub backup_type: BackupType,
    pub start: Option<chrono::NaiveDateTime>,
    pub finish: Option<chrono::NaiveDateTime>,
    pub first_lsn: String,
    pub last_lsn: String,
    pub checkpoint_lsn: String,
    pub differential_base_lsn: String,
    pub binding_id: String,
    pub differential_base_guid: String,
    pub family: String,
    pub sequence: u32,
    pub size: u64,
    /// Some stripes are missing, or it is a differential whose full backup is not among the files.
    pub incomplete: bool,
    /// The database is encrypted (TDE), which the reader refuses.
    pub encrypted: bool,
    /// The parts, one per stripe file.
    parts: Vec<SetPart>,
    set_guid: [u8; 16],
    raw_differential_base: [u8; 16],
    position: u32,
}

impl BackupSet {
    /// Full and differential backups that are complete and not encrypted can be compared.
    pub fn can_handle(&self) -> bool {
        matches!(self.backup_type, BackupType::Full | BackupType::Differential) && !self.encrypted
    }

    pub fn files(&self) -> Vec<PathBuf> {
        self.parts.iter().map(|p| p.file.clone()).collect()
    }
}

pub fn guid_text(g: &[u8; 16]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}",
        u32::from_le_bytes(g[..4].try_into().unwrap()),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10..].iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// A file (or a stream of a .sqb backup file) and its readable bytes.
type Opened = (PathBuf, Arc<dyn Medium>);

/// The readable form of every file: native files directly (decompressed when compressed),
/// .sqb backup files through their decoder.
fn open_media(files: &[PathBuf], passwords: &[String]) -> Result<Vec<Opened>, BackupError> {
    let mut out = Vec::new();
    for f in files {
        if !f.exists() {
            return Err(BackupError::Io(format!("Specified file '{}' does not exist", f.display())));
        }
        if sqb::is_sqb(f) {
            for (i, m) in sqb::open(f, passwords)?.into_iter().enumerate() {
                out.push((f.join(format!(".Stream {i}")), m));
            }
            continue;
        }
        let raw: Arc<dyn Medium> = Arc::new(mtf::RawFile::open(f).map_err(|e| BackupError::Io(format!("{}: {e}", f.display())))?);
        let medium: Arc<dyn Medium> = if mtf::Compressed::is_compressed(&*raw) {
            Arc::new(mtf::Compressed::open(raw).map_err(|_| BackupError::Reader("Cannot open backup - possible file corruption".into()))?)
        } else {
            raw
        };
        out.push((f.clone(), medium));
    }
    Ok(out)
}

fn not_a_backup(f: &Path) -> BackupError {
    BackupError::Reader(format!("The file '{}' does not appear to be a Microsoft SQL Server or .sqb backup file", f.display()))
}

struct Media {
    files: Vec<Opened>,
    sets: Vec<BackupSet>,
}

fn scan(files: &[PathBuf], passwords: &[String]) -> Result<Media, BackupError> {
    let media = open_media(files, passwords)?;
    let mut parts: Vec<SetPart> = Vec::new();
    for (path, m) in &media {
        let found = mtf::read_sets(&**m, path).map_err(|_| not_a_backup(path))?;
        parts.extend(found);
    }
    // Stripes of one backup share the set GUID; each file holds one of them.
    let mut sets: Vec<BackupSet> = Vec::new();
    for (position, p) in parts.into_iter().enumerate() {
        if let Some(s) = sets.iter_mut().find(|s| s.set_guid == p.set_guid && s.first_lsn == p.first_lsn.to_string() && s.backup_type == p.backup_type) {
            if !s.parts.iter().any(|q| q.sequence == p.sequence) {
                s.parts.push(p);
            }
            continue;
        }
        sets.push(BackupSet {
            name: p.name.clone(),
            database: p.database.clone(),
            server: p.server.clone(),
            backup_type: p.backup_type,
            start: p.start,
            finish: p.finish,
            first_lsn: p.first_lsn.to_string(),
            last_lsn: p.last_lsn.to_string(),
            checkpoint_lsn: p.checkpoint_lsn.to_string(),
            differential_base_lsn: p.differential_base_lsn.to_string(),
            binding_id: guid_text(&p.binding_id),
            differential_base_guid: guid_text(&p.differential_base_guid),
            family: guid_text(&p.family),
            sequence: p.sequence,
            size: p.size,
            incomplete: false,
            encrypted: false,
            set_guid: p.set_guid,
            raw_differential_base: p.differential_base_guid,
            position: position as u32,
            parts: vec![p],
        });
    }
    for s in &mut sets {
        s.parts.sort_by_key(|p| p.sequence);
        let devices = s.parts[0].devices as usize;
        s.incomplete = s.parts.len() < devices || !matches!(s.backup_type, BackupType::Full | BackupType::Differential);
    }
    let fulls: Vec<([u8; 16], bool)> = sets.iter().filter(|s| s.backup_type == BackupType::Full).map(|s| (s.set_guid, s.incomplete)).collect();
    for s in &mut sets {
        if s.backup_type == BackupType::Differential && !fulls.iter().any(|(g, inc)| *g == s.raw_differential_base && !inc) {
            s.incomplete = true;
        }
    }
    let mut out = Media { files: media, sets };
    for i in 0..out.sets.len() {
        if !out.sets[i].incomplete {
            out.sets[i].encrypted = out.store_for(i).ok().and_then(|st| boot(&st).ok()).is_some_and(|b| b.encrypted);
        }
    }
    // Complete sets first, then by position; a differential comes after its full backup.
    out.sets.sort_by(|a, b| a.incomplete.cmp(&b.incomplete).then(a.position.cmp(&b.position)));
    Ok(out)
}

impl Media {
    fn medium(&self, part: &SetPart) -> Arc<dyn Medium> {
        self.files.iter().find(|(p, _)| *p == part.file).map(|(_, m)| m.clone()).expect("part of a scanned file")
    }

    /// The pages of set `i`: its full backup's extents, then a differential's on top.
    fn store_for(&self, i: usize) -> Result<PageStore, BackupError> {
        let set = &self.sets[i];
        let mut layers: Vec<&BackupSet> = Vec::new();
        if set.backup_type == BackupType::Differential {
            let full = self
                .sets
                .iter()
                .find(|s| s.backup_type == BackupType::Full && s.set_guid == set.raw_differential_base)
                .ok_or_else(|| BackupError::Reader("No full backup specified".into()))?;
            layers.push(full);
        }
        layers.push(set);
        let mut parts: Vec<(Arc<dyn Medium>, &SetPart)> = Vec::new();
        for l in layers {
            if l.parts.len() < l.parts[0].devices as usize {
                return Err(BackupError::Reader("One or more backups are missing from the specified set of striped backups".into()));
            }
            for p in &l.parts {
                let m = self.medium(p);
                if p.extents.iter().any(|&(off, len)| off + len > m.len()) {
                    return Err(BackupError::Reader(format!("The backup file {} is truncated or damaged", p.file.display())));
                }
                parts.push((m, p));
            }
        }
        Ok(PageStore::build(&parts))
    }
}

/// The backup sets in the files, in the order the classic compare tool lists them (the default is the last).
pub fn list_sets(files: &[PathBuf], passwords: &[String]) -> Result<Vec<BackupSet>, BackupError> {
    Ok(scan(files, passwords)?.sets)
}

/// What the boot page says about the database.
pub struct Boot {
    pub version: u16,
    pub compatibility_level: u8,
    pub collation: u32,
    pub name: String,
    pub encrypted: bool,
}

fn boot(store: &PageStore) -> Result<Boot, BackupError> {
    let p = store.page(PageId { file: 1, page: 9 }).ok_or_else(|| BackupError::Reader("Failed to locate database boot page".into()))?;
    let r = pages::slot(&p, 0).ok_or_else(|| BackupError::Reader("Failed to locate database boot page".into()))?;
    let version = mtf::u16_at(r, 4);
    if version < 539 {
        return Err(BackupError::Reader(format!("Unknown database version '{version}'")));
    }
    Ok(Boot {
        version,
        compatibility_level: r[314],
        collation: mtf::u32_at(r, 392),
        name: values::utf16(&r[52..52 + 256]).trim_end_matches(['\0', ' ']).to_string(),
        encrypted: version >= 655 && r[401] & 1 != 0,
    })
}

/// An open backup set: its pages and what the boot page says.
pub struct Backup {
    pub set: BackupSet,
    pub store: PageStore,
    pub boot: Boot,
}

/// Opens the set named `set_name` (by its backup set name), or the latest one.
pub fn open(files: &[PathBuf], passwords: &[String], set_name: Option<&str>) -> Result<Backup, BackupError> {
    let media = scan(files, passwords)?;
    if media.sets.is_empty() {
        return Err(BackupError::Reader("No backup sets found.".into()));
    }
    let i = match set_name {
        Some(n) => media
            .sets
            .iter()
            .position(|s| s.can_handle() && s.name == n)
            .ok_or_else(|| BackupError::Reader("The specified backup set in the backup file could not be located.".into()))?,
        None => media.sets.len() - 1,
    };
    let set = media.sets[i].clone();
    match set.backup_type {
        BackupType::Full | BackupType::Differential => {}
        BackupType::Log => return Err(BackupError::Reader("Transaction log backups are not supported".into())),
        BackupType::File | BackupType::FileDifferential => return Err(BackupError::Reader("File and filegroup backups are not supported".into())),
        BackupType::Other(_) => return Err(BackupError::Reader("Unsupported backup type (type unknown)".into())),
    }
    let store = media.store_for(i)?;
    let boot = boot(&store)?;
    if boot.encrypted {
        return Err(BackupError::Reader("Encrypted backups are not supported".into()));
    }
    Ok(Backup { set, store, boot })
}

impl Backup {
    pub fn catalog(&self) -> Result<tables::Catalog<'_>, BackupError> {
        tables::Catalog::open(&self.store, self.boot.collation).map_err(BackupError::Reader)
    }

    /// The database model, as a live read of the database would give it.
    pub fn read_database(&self) -> Result<crate::catalog::Database, BackupError> {
        let cat = self.catalog()?;
        let info = catalog::DatabaseInfo {
            name: if self.set.database.is_empty() { self.boot.name.clone() } else { self.set.database.clone() },
            collation: self.boot.collation,
            compatibility_level: self.boot.compatibility_level as u32,
            version: self.boot.version,
            server: self.set.server.clone(),
        };
        catalog::read(&cat, &info).map_err(BackupError::Reader)
    }
}
