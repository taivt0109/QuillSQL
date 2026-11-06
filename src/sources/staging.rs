//! Data compare with a scripts folder as one side. The folder's tables and the INSERT scripts
//! of its `Data` folder are loaded into a temporary database on the other side's server, so
//! the data engine compares two live databases; deploying to the folder runs the script there
//! and writes the changed tables' data scripts back.

use std::path::{Path, PathBuf};

use super::folder::{self, Folder};
use crate::catalog::{Database, ObjectName};
use crate::db::{self, ConnectionSpec};
use crate::schema::{self, DiffKind, Mappings, ObjectType, Options, deploy};

/// A temporary database holding a scripts folder; dropped with the value.
pub struct Staging {
    pub spec: ConnectionSpec,
    pub folder: Folder,
    /// The data files read, by table.
    pub files: Vec<(ObjectName, PathBuf)>,
    _checkout: Option<super::scm::Checkout>,
}

impl Drop for Staging {
    fn drop(&mut self) {
        let mut master = self.spec.clone();
        master.database = "master".into();
        let name = db::quote(&self.spec.database);
        if let Ok(rt) = db::runtime() {
            let _ = rt.block_on(async {
                let mut c = db::connect_async(&master).await?;
                db::execute(&mut c, &format!("IF DB_ID({}) IS NOT NULL BEGIN ALTER DATABASE {name} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {name}; END", db::literal(&self.spec.database))).await
            });
        }
    }
}

/// The data scripts of a folder: the top level of its `Data` folder (or the files the settings
/// list), one table per file, `<schema>.<table>_Data.sql`.
pub fn data_files(f: &Folder) -> Vec<PathBuf> {
    let dir = f.path.join(f.info.prefixes.get("Data").map(String::as_str).unwrap_or("Data"));
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sql"))).collect())
        .unwrap_or_default();
    for name in &f.info.data_files {
        let p = f.path.join(name);
        if p.is_file() && !out.contains(&p) {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// The table a data script fills: from its INSERT statements, else from its file name.
fn table_of(f: &Folder, path: &Path, text: &str) -> Option<ObjectName> {
    let tokens = crate::schema::text::tokenize(text);
    let sig: Vec<&crate::schema::text::Token> = tokens.iter().filter(|t| !matches!(t.kind, crate::schema::text::Tok::Space | crate::schema::text::Tok::LineComment | crate::schema::text::Tok::BlockComment)).collect();
    let unq = |s: &str| s.trim_matches(['[', ']']).replace("]]", "]");
    for k in 0..sig.len() {
        if sig[k].text.eq_ignore_ascii_case("INSERT") {
            let mut j = k + 1;
            if sig.get(j).is_some_and(|t| t.text.eq_ignore_ascii_case("INTO")) {
                j += 1;
            }
            let a = sig.get(j)?;
            if sig.get(j + 1).is_some_and(|t| t.text == ".") {
                return Some(ObjectName::new(&unq(&a.text), &unq(&sig.get(j + 2)?.text)));
            }
            return Some(ObjectName::new(&f.info.default_schema, &unq(&a.text)));
        }
    }
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let stem = stem.strip_suffix("_Data").unwrap_or(&stem);
    let (s, n) = stem.split_once('.')?;
    Some(ObjectName::new(s, n))
}

fn run(spec: &ConnectionSpec, batches: &[String]) -> Result<(), String> {
    let rt = db::runtime()?;
    rt.block_on(async {
        let mut c = db::connect_async(spec).await?;
        for b in batches {
            db::execute(&mut c, b).await.map_err(|e| format!("{e}\n  in: {}", b.lines().next().unwrap_or_default()))?;
        }
        Ok(())
    })
}

/// Loads the scripts folder at `dir` (at `revision`, if given) into a new database on `server`.
pub fn stage(dir: &Path, revision: Option<&str>, server: &ConnectionSpec) -> Result<Staging, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let checkout = revision.map(|r| super::scm::checkout(&dir.to_string_lossy(), r)).transpose()?;
    let read_dir = checkout.as_ref().map(|c| c.dir.clone()).unwrap_or_else(|| dir.to_path_buf());
    let mut f = folder::read(&read_dir)?;
    if checkout.is_some() {
        f.database.name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(f.database.name);
    }
    let name = format!("QuillScripts_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
    let mut spec = server.clone();
    spec.database = name.clone();
    let mut master = server.clone();
    master.database = "master".into();
    let q = db::quote(&name);
    run(&master, &[format!("CREATE DATABASE {q} COLLATE {}", f.info.collation)])?;
    let staging = Staging { spec: spec.clone(), files: Vec::new(), folder: f, _checkout: checkout };
    let mut staging = staging;

    // Only what holds data: schemas, types, sequences and tables, without triggers.
    let mut model = staging.folder.database.clone();
    model.views.clear();
    model.routines.clear();
    model.synonyms.clear();
    model.ddl_triggers.clear();
    model.permissions.clear();
    model.principals.clear();
    model.extended_properties.clear();
    for s in &mut model.schemas {
        s.owner = "dbo".into();
    }
    for t in &mut model.tables {
        t.triggers.clear();
    }
    let empty = crate::catalog::read_database(&spec)?;
    let opts = Options::default_set();
    let mut c = schema::compare(&model, &empty, &opts, &Mappings::default());
    for d in &mut c.differences {
        d.selected = d.kind == DiffKind::OnlyIn1 && matches!(d.object_type, ObjectType::Schema | ObjectType::UserDefinedType | ObjectType::Sequence | ObjectType::Table);
    }
    let settings = deploy::ScriptSettings {
        transaction_isolation_level: "SERIALIZABLE".into(),
        source: model.name.clone(),
        target: name.clone(),
        target_database: name.clone(),
        target_major_version: empty.server.major_version,
    };
    let script = deploy::generate(&c, &model, &empty, &opts, &Mappings::default(), &settings)?;
    deploy::execute(&spec, &script, |_, _| {})?;

    // Rows go in in any order: constraints are not checked while loading.
    let nocheck: Vec<String> = model.tables.iter().filter(|t| !t.foreign_keys.is_empty() || !t.check_constraints.is_empty()).map(|t| format!("ALTER TABLE {} NOCHECK CONSTRAINT ALL", t.object_name().quoted())).collect();
    run(&spec, &nocheck)?;
    for path in data_files(&staging.folder) {
        let text = folder::decode(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?, &staging.folder.info.encoding);
        run(&spec, &deploy::split_batches(&text)).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(t) = table_of(&staging.folder, &path, &text) {
            staging.files.push((t, path));
        }
    }
    let check: Vec<String> = model.tables.iter().filter(|t| !t.foreign_keys.is_empty() || !t.check_constraints.is_empty()).map(|t| format!("ALTER TABLE {} WITH CHECK CHECK CONSTRAINT ALL", t.object_name().quoted())).collect();
    // Data that breaks a constraint is still compared; only the trust flag is lost.
    let _ = run(&spec, &check);
    Ok(staging)
}

impl Staging {
    /// After deploying to the staging database: writes the data scripts of `tables` again
    /// (deleting a script whose table is now empty). Returns the files written.
    pub fn write_back(&self, db: &Database, tables: &[ObjectName]) -> Result<Vec<PathBuf>, String> {
        let opts = crate::data::DataOptions::default_set();
        let (pairs, _, _) = crate::data::mapping::map_tables(db, db, &opts, &crate::data::mapping::DataMappings::default());
        let data_dir = self.folder.path.join(self.folder.info.prefixes.get("Data").map(String::as_str).unwrap_or("Data"));
        let mut written = Vec::new();
        for t in tables {
            let Some(p) = pairs.iter().find(|p| p.source.schema.eq_ignore_ascii_case(&t.schema) && p.source.name.eq_ignore_ascii_case(&t.name)) else { continue };
            let text = crate::data::compare::script_rows(&self.spec, db, p)?;
            let path = self.files.iter().find(|(n, _)| n.schema.eq_ignore_ascii_case(&t.schema) && n.name.eq_ignore_ascii_case(&t.name)).map(|(_, p)| p.clone()).unwrap_or_else(|| {
                let name: String = format!("{}.{}_Data.sql", t.schema, t.name).chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
                data_dir.join(name)
            });
            if text.is_empty() {
                if path.exists() {
                    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                continue;
            }
            std::fs::create_dir_all(&data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
            std::fs::write(&path, [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat()).map_err(|e| format!("{}: {e}", path.display()))?;
            written.push(path);
        }
        Ok(written)
    }
}
