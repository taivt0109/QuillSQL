//! `quill compare-serve`: the engine behind the Compare windows (SSMS and QuillCompare.exe).
//! One JSON request per stdin line, `{"id":n,"op":"...",...}`; one reply per stdout line,
//! `{"id":n,"result":...}` or `{"id":n,"error":"..."}`. Long operations first send
//! `{"id":n,"progress":"..."}` lines. Comparison results stay here between requests.
//!
//! - `options` describes every schema and data option (for the Options tabs).
//! - `databases` {source} lists databases; `test_connection` {source} returns the server version.
//! - `project_load` {path} / `project_save` {path, project}.
//! - `schema_compare` {project} → session with differences;
//!   `schema_object_sql` {session, index}; `schema_script` {session, selected:[index], project};
//!   `schema_dependencies` {session, selected}; `schema_switch` {session}; `schema_report`
//!   {session, path, format, selected, includeIdentical};
//!   `deploy` {session} runs the last script.
//! - `data_tables` {project} previews table pairs and keys; `data_compare` {project} → session;
//!   `data_rows` {session, table, kind, start, count, sort}; `data_find`; `data_value`;
//!   `data_script` {session, kinds, tables, selections, isolationLevel, maxTransactionSizeMB}
//!   (see `selections`); `data_export` {session, folder, kinds, tables} (CSV);
//!   `data_report` {session, path, kinds} (HTML);
//!   `data_session_save` {session, path} / `data_session_load` {path} → session.
//! - `snapshot` {source, path} writes a schema snapshot.
//!
//! - `backup_sets` {source} lists the backup sets of a backup side.
//!
//! A side is a live database, a snapshot (QuillSQL JSON or SQL Compare `.snp`), backup files
//! (`type: "backup"`, `path` with the files separated by `;`, optional `backupSet` name,
//! `password` with the files' passwords separated by `,`), or a scripts
//! folder (`type: "scripts"`, `path`, optional source-control `revision`). Deploying to a
//! scripts folder rewrites its files; `make_scripts` {project | source, path} writes one.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::catalog::{self, Database, ObjectName};
use crate::data::compare::DataComparison;
use crate::data::store::{self, RowKind};
use crate::data::sync::RowSelection;
use crate::data::{DataOpt, DataOptions};
use crate::project::{Project, SourceSpec};
use crate::schema::compare::{Comparison, DiffKind, ObjectType};
use crate::schema::deploy::{self, DeploymentScript, ScriptSettings};
use crate::schema::{self, Opt, Options, ddl};

struct SchemaSession {
    db1: Database,
    db2: Database,
    comparison: Comparison,
    project: Project,
    script: Option<DeploymentScript>,
    /// The differences chosen for the last script, for deploying to a scripts folder.
    selected: Vec<usize>,
}

struct DataSession {
    source_name: String,
    db2: Database,
    result: DataComparison,
    project: Project,
    script: Option<DeploymentScript>,
    /// Row orders built for the All rows tab and for sorting, by (table, kind, sort).
    views: Views,
    /// Scripts-folder sides read without a server.
    folders: [Option<crate::sources::datafolder::DataFolder>; 2],
    /// The row kinds of the last script, for deploying to a scripts folder.
    kinds: [bool; 4],
    /// The tables (`None` = all) and row choices of the last script, for the same.
    tables: Option<Vec<ObjectName>>,
    selections: HashMap<ObjectName, [RowSelection; 4]>,
}

impl DataSession {
    /// The database holding side `n` (1 or 2).
    fn spec(&self, n: u8) -> Result<crate::db::ConnectionSpec, String> {
        if n == 1 { &self.project.source } else { &self.project.target }.connection(n)
    }
}

#[derive(Default)]
struct State {
    next: u64,
    schema: HashMap<u64, SchemaSession>,
    data: HashMap<u64, DataSession>,
    /// Sides read lately, as SQL Compare keeps its registrations: the mapping tabs and rename
    /// detection reuse them, Refresh and a comparison read again.
    sides: Vec<(String, Database, Vec<String>)>,
}

/// Progress lines of a request; both sides may report at once.
type Progress<'a> = dyn Fn(&str) + Sync + 'a;

/// A side and what its reader warned about.
type Loaded = (Database, Vec<String>);

/// Reads the project's two sides at the same time, as SQL Compare does; the target is None for
/// an empty database. With `fresh` both are read again, else a side read lately is reused.
fn load_sides(state: &mut State, project: &Project, decrypt: bool, fresh: bool, progress: &Progress) -> Result<(Loaded, Option<Loaded>), String> {
    let key = |s: &SourceSpec| format!("{decrypt}|{}", serde_json::to_string(s).unwrap_or_default());
    let cached = |s: &SourceSpec| (!fresh).then(|| state.sides.iter().find(|c| c.0 == key(s)).map(|c| (c.1.clone(), c.2.clone()))).flatten();
    let (c1, c2) = (cached(&project.source), if project.target.is_empty_database() { None } else { cached(&project.target) });
    let (r1, r2) = std::thread::scope(|scope| {
        let t2 = (c2.is_none() && !project.target.is_empty_database()).then(|| scope.spawn(|| load_side_with_warnings(&project.target, 2, decrypt, progress)));
        let r1 = match c1 {
            Some(c) => Ok(c),
            None => load_side_with_warnings(&project.source, 1, decrypt, progress),
        };
        let r2 = match t2 {
            Some(t) => t.join().unwrap_or_else(|_| Err("reading the target failed".into())).map(Some),
            None => Ok(c2),
        };
        (r1, r2)
    });
    let (r1, r2) = (r1?, r2?);
    for (side, loaded) in [(&project.source, Some(&r1)), (&project.target, r2.as_ref())] {
        let Some((db, warnings)) = loaded else { continue };
        let k = key(side);
        state.sides.retain(|c| c.0 != k);
        state.sides.push((k, db.clone(), warnings.clone()));
    }
    // Two pairs at most, like SQL Compare's two registrations per side.
    let excess = state.sides.len().saturating_sub(4);
    state.sides.drain(..excess);
    Ok((r1, r2))
}

pub fn run() -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut state = State::default();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                writeln!(stdout.lock(), "{}", json!({"id": null, "error": format!("bad request: {e}")})).ok();
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let progress = |text: &str| {
            let mut out = stdout.lock();
            let _ = writeln!(out, "{}", json!({"id": id, "progress": text}));
            let _ = out.flush();
        };
        // Microsoft Entra device-code sign-in shows its code as a progress line of this request.
        let prompt_id = id.clone();
        crate::entra::set_prompt(Some(std::sync::Arc::new(move |text: &str, details: &Value| {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", json!({"id": prompt_id, "progress": text, "deviceCode": details}));
            let _ = out.flush();
        })));
        let reply = match handle(&mut state, &request, &progress) {
            Ok(result) => json!({"id": id, "result": result}),
            Err(e) => json!({"id": id, "error": e}),
        };
        let mut out = stdout.lock();
        writeln!(out, "{reply}").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn field<T: serde::de::DeserializeOwned>(request: &Value, name: &str) -> Result<T, String> {
    let v = request.get(name).cloned().ok_or_else(|| format!("'{name}' is missing"))?;
    serde_json::from_value(v).map_err(|e| format!("'{name}': {e}"))
}

fn session(request: &Value) -> Result<u64, String> {
    request.get("session").and_then(Value::as_u64).ok_or_else(|| "'session' is missing".to_string())
}

/// The files and passwords of a backup side.
fn backup_files(side: &SourceSpec) -> (Vec<std::path::PathBuf>, Vec<String>) {
    let files = side.path.split(';').map(str::trim).filter(|f| !f.is_empty()).map(Into::into).collect();
    let passwords = side.password.as_deref().filter(|p| !p.is_empty()).map(|p| p.split(',').map(str::to_string).collect()).unwrap_or_default();
    (files, passwords)
}

fn open_backup(side: &SourceSpec, progress: &Progress) -> Result<crate::sources::backup::Backup, String> {
    progress(&format!("Reading backup {}", side.path));
    let (files, passwords) = backup_files(side);
    let set = (!side.backup_set.is_empty()).then_some(side.backup_set.as_str());
    crate::sources::backup::open(&files, &passwords, set).map_err(|e| e.to_string())
}

/// `decrypt` follows DecryptPost2kEncryptedObjects; data compare never needs module text.
fn load_side(side: &SourceSpec, n: u8, decrypt: bool, progress: &Progress) -> Result<Database, String> {
    load_side_with_warnings(side, n, decrypt, progress).map(|(db, _)| db)
}

/// A side and, for a scripts folder, what its parser skipped.
fn load_side_with_warnings(side: &SourceSpec, n: u8, decrypt: bool, progress: &Progress) -> Result<(Database, Vec<String>), String> {
    if side.is_scripts() {
        let checkout = match side.revision.as_str() {
            "" => None,
            r => {
                progress(&format!("Getting revision {r} of '{}'", side.path));
                Some(crate::sources::scm::checkout(&side.path, r)?)
            }
        };
        progress(&format!("Reading database scripts from '{}'", side.path));
        let dir = checkout.as_ref().map(|c| c.dir.clone()).unwrap_or_else(|| std::path::PathBuf::from(&side.path));
        let mut f = crate::sources::folder::read(&dir)?;
        if checkout.is_some() {
            f.database.name = std::path::Path::new(&side.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(f.database.name);
        }
        let warnings = f.warnings.iter().cloned().chain(f.messages.iter().map(|m| m.text())).collect();
        return Ok((f.database, warnings));
    }
    if side.is_backup() {
        return Ok((open_backup(side, progress)?.read_database().map_err(|e| e.to_string())?, Vec::new()));
    }
    if side.is_snapshot() {
        let bytes = std::fs::read(&side.path).map_err(|e| format!("{}: {e}", side.path))?;
        if crate::sources::snp::is_snp(&bytes) {
            return Ok((crate::sources::snp::read(&bytes).map_err(|e| format!("{}: {e}", side.path))?, Vec::new()));
        }
        let db = serde_json::from_slice(crate::project::without_bom(&bytes)).map_err(|e| format!("{} is not a snapshot: {e}", side.path))?;
        return Ok((db, Vec::new()));
    }
    progress(&format!("Reading {}", side.describe()));
    Ok((catalog::read_database_with(&side.connection(n)?, decrypt)?, Vec::new()))
}

fn schema_options(p: &Project) -> Result<Options, String> {
    let o = Options::parse(&p.options.join(","))?;
    o.validate()?;
    Ok(o)
}

fn data_options(p: &Project) -> Result<DataOptions, String> {
    DataOptions::parse(&p.options.join(","))
}

fn handle(state: &mut State, request: &Value, progress: &Progress) -> Result<Value, String> {
    let op = request.get("op").and_then(Value::as_str).unwrap_or_default();
    match op {
        "options" => Ok(option_definitions()),
        "backup_sets" => {
            let side: SourceSpec = field(request, "source")?;
            let (files, passwords) = backup_files(&side);
            let sets = crate::sources::backup::list_sets(&files, &passwords).map_err(|e| e.to_string())?;
            let time = |t: Option<chrono::NaiveDateTime>| t.map(|t| t.to_string());
            Ok(json!(sets.iter().map(|s| json!({
                "name": s.name, "database": s.database, "server": s.server, "type": format!("{:?}", s.backup_type),
                "start": time(s.start), "finish": time(s.finish), "firstLsn": s.first_lsn, "lastLsn": s.last_lsn,
                "files": s.files(), "incomplete": s.incomplete, "encrypted": s.encrypted, "canHandle": s.can_handle() && !s.incomplete,
            })).collect::<Vec<_>>()))
        }
        "databases" => {
            let side: SourceSpec = field(request, "source")?;
            let mut spec = side.connection(1)?;
            spec.database = "master".into();
            let rt = crate::db::runtime()?;
            rt.block_on(async {
                let mut c = crate::db::connect_async(&spec).await?;
                let rows = crate::db::query(&mut c, "SELECT name FROM sys.databases WHERE state = 0 ORDER BY name").await?;
                Ok(json!(rows.iter().filter_map(|r| r.get::<&str, _>(0).map(str::to_string)).collect::<Vec<_>>()))
            })
        }
        "test_connection" => {
            let side: SourceSpec = field(request, "source")?;
            let spec = side.connection(1)?;
            let rt = crate::db::runtime()?;
            rt.block_on(async {
                let mut c = crate::db::connect_async(&spec).await?;
                let rows = crate::db::query(&mut c, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)), DB_NAME()").await?;
                let r = rows.first().ok_or("no reply")?;
                Ok(json!({"version": r.get::<&str, _>(0), "database": r.get::<&str, _>(1)}))
            })
        }
        // Runs one statement on the source's server (Create database).
        "execute" => {
            let side: SourceSpec = field(request, "source")?;
            let sql: String = field(request, "sql")?;
            let spec = side.connection(1)?;
            let rt = crate::db::runtime()?;
            rt.block_on(async {
                let mut c = crate::db::connect_async(&spec).await?;
                crate::db::execute(&mut c, &sql).await
            })?;
            Ok(json!(true))
        }
        "project_load" => {
            let path: String = field(request, "path")?;
            serde_json::to_value(Project::load(std::path::Path::new(&path))?).map_err(|e| e.to_string())
        }
        "project_save" => {
            let path: String = field(request, "path")?;
            let project: Project = field(request, "project")?;
            project.save(std::path::Path::new(&path))?;
            Ok(json!(true))
        }
        "snapshot" => {
            let side: SourceSpec = field(request, "source")?;
            let path: String = field(request, "path")?;
            let db = load_side(&side, 1, true, progress)?;
            // SQL Compare's format, or QuillSQL's JSON model for a .json path.
            let bytes = if path.to_ascii_lowercase().ends_with(".json") {
                serde_json::to_vec_pretty(&db).map_err(|e| e.to_string())?
            } else {
                crate::sources::snp_write::write(&db)
            };
            std::fs::write(&path, bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"tables": db.tables.len()}))
        }
        // Writes a side as a scripts folder: the project's source with its options, or a source.
        "make_scripts" => {
            let path: String = field(request, "path")?;
            let (side, opts) = match field::<Project>(request, "project") {
                Ok(p) => (p.source.clone(), schema_options(&p)?),
                Err(_) => (field::<SourceSpec>(request, "source")?, Options::default_set()),
            };
            let db = load_side(&side, 1, opts.has(Opt::DecryptPost2kEncryptedObjects), progress)?;
            progress(&format!("Writing the scripts folder '{path}'"));
            let files = crate::sources::folder::write_all(&db, std::path::Path::new(&path), &opts)?;
            Ok(json!({"files": files}))
        }
        "schema_compare" => {
            let project: Project = field(request, "project")?;
            let mut opts = schema_options(&project)?;
            // Folders with an SSDT database project are compared with SsdtCompatibleOptions.
            if [&project.source, &project.target].iter().any(|s| s.is_scripts() && crate::sources::sqlproj::find(std::path::Path::new(&s.path)).is_some()) {
                opts = Options::parse("SsdtCompatibleOptions")?.iter().fold(opts, |o, x| o.with(x));
            }
            let decrypt = opts.has(Opt::DecryptPost2kEncryptedObjects);
            let ((mut db1, mut warnings), loaded2) = load_sides(state, &project, decrypt, true, progress)?;
            folder_defaults(request, "source", &mut db1);
            // An empty database (`/empty2`): every object is created; only a script can be made.
            let db2 = if let Some((mut db2, w2)) = loaded2 {
                folder_defaults(request, "target", &mut db2);
                warnings.extend(w2);
                db2
            } else {
                Database { name: "Empty".into(), server: db1.server.clone(), collation: db1.collation.clone(), ..Database::default() }
            };
            progress("Comparing");
            let comparison = compare_schemas(&db1, &db2, &opts, &project)?;
            state.next += 1;
            let id = state.next;
            let s = SchemaSession { db1, db2, comparison, project, script: None, selected: Vec::new() };
            let mut reply = schema_reply(id, &s);
            reply["warnings"] = json!(warnings);
            add_modified(&mut reply, &s);
            state.schema.insert(id, s);
            Ok(reply)
        }
        // Switch deployment direction: the target becomes the source, compared again from what
        // was read (the same session, a new set of differences).
        "schema_switch" => {
            let id = session(request)?;
            let s = state.schema.get_mut(&id).ok_or("the comparison is no longer open")?;
            if s.project.target.is_empty_database() {
                return Err("an empty database cannot be the source".into());
            }
            std::mem::swap(&mut s.db1, &mut s.db2);
            let p = &mut s.project;
            std::mem::swap(&mut p.source, &mut p.target);
            for m in &mut p.schema_mappings.schemas {
                *m = (m.1.clone(), m.0.clone());
            }
            for t in &mut p.schema_mappings.tables {
                std::mem::swap(&mut t.source, &mut t.target);
                for c in &mut t.columns {
                    *c = (c.1.clone(), c.0.clone());
                }
            }
            let opts = schema_options(&s.project)?;
            s.comparison = compare_schemas(&s.db1, &s.db2, &opts, &s.project)?;
            s.script = None;
            let mut reply = schema_reply(id, s);
            reply["project"] = serde_json::to_value(&s.project).map_err(|e| e.to_string())?;
            Ok(reply)
        }
        // Objects the selected ones need that differ too: what "Include dependencies" adds.
        "schema_dependencies" => {
            let s = state.schema.get(&session(request)?).ok_or("the comparison is no longer open")?;
            let selected: Vec<usize> = field(request, "selected")?;
            Ok(json!(dependencies(s, &selected)))
        }
        // A comparison report: {path, format: Html|Classic|Excel|Xml, selected (only these
        // differences, else all), includeIdentical}.
        "schema_report" => {
            let s = state.schema.get(&session(request)?).ok_or("the comparison is no longer open")?;
            let path: String = field(request, "path")?;
            let format = request.get("format").and_then(Value::as_str).unwrap_or("Html");
            let selected: Option<Vec<usize>> = request.get("selected").and_then(|v| serde_json::from_value(v.clone()).ok());
            let include_identical = request.get("includeIdentical").and_then(Value::as_bool).unwrap_or(false);
            let bytes = schema_report(s, format, selected.as_deref(), include_identical);
            std::fs::write(&path, bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!(true))
        }
        // Filters: `filter_load` {path | xml} → the rules; `filter_save` {path, filter} writes an
        // .scpf; `schema_filter` {session, filter} checks the rules and makes them the project's
        // filter → {xml, filtered: [index]} (null filter: none).
        "filter_load" => {
            let model = match field::<String>(request, "path") {
                Ok(path) => schema::filter_edit::FilterModel::load(std::path::Path::new(&path))?,
                Err(_) => schema::filter_edit::FilterModel::from_xml(&field::<String>(request, "xml")?)?,
            };
            Ok(json!({"filter": model, "types": schema::filter_edit::types()}))
        }
        "filter_save" => {
            let path: String = field(request, "path")?;
            let model: schema::filter_edit::FilterModel = field(request, "filter")?;
            model.filter()?;
            std::fs::write(&path, model.scpf()).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!(true))
        }
        "schema_filter" => {
            let s = state.schema.get_mut(&session(request)?).ok_or("the comparison is no longer open")?;
            let model: Option<schema::filter_edit::FilterModel> = field(request, "filter").ok();
            let (xml, filter) = match model.filter(|m| !m.is_trivial()) {
                Some(m) => (Some(m.project_xml()), Some(m.filter()?)),
                None => (None, None),
            };
            s.project.filter = xml.clone();
            Ok(json!({"xml": xml, "filtered": filtered(filter.as_ref(), &s.comparison)}))
        }
        "schema_object_sql" => {
            let s = state.schema.get(&session(request)?).ok_or("the comparison is no longer open")?;
            let index: usize = field(request, "index")?;
            let d = s.comparison.differences.get(index).ok_or("no such difference")?;
            let target = d.target_name.clone().unwrap_or_else(|| d.name.clone());
            let source = if d.kind == DiffKind::OnlyIn2 { None } else { object_sql(&s.db1, d.object_type, &d.name) };
            let target = if d.kind == DiffKind::OnlyIn1 { None } else { object_sql(&s.db2, d.object_type, &target) };
            Ok(json!({"source": source, "target": target}))
        }
        "schema_script" => {
            let id = session(request)?;
            let s = state.schema.get_mut(&id).ok_or("the comparison is no longer open")?;
            if let Ok(p) = field::<Project>(request, "project") {
                s.project.options = p.options;
            }
            let selected: Vec<usize> = field(request, "selected")?;
            let mut comparison = s.comparison.clone();
            for (i, d) in comparison.differences.iter_mut().enumerate() {
                d.selected = selected.contains(&i);
            }
            let opts = schema_options(&s.project)?;
            let settings = ScriptSettings {
                transaction_isolation_level: request.get("isolationLevel").and_then(Value::as_str).unwrap_or("SERIALIZABLE").to_ascii_uppercase(),
                source: s.project.source.describe(),
                target: s.project.target.describe(),
                target_database: s.db2.name.clone(),
                target_major_version: if s.db2.server.is_azure_sql_database() { 99 } else { s.db2.server.major_version },
            };
            let mut script = deploy::generate(&comparison, &s.db1, &s.db2, &opts, &s.project.schema_mappings, &settings)?;
            if s.project.source.is_scripts() && !opts.has(Opt::IgnorePrePostScripts) {
                let dir = std::path::Path::new(&s.project.source.path);
                let (info, _) = crate::sources::folder::FolderInfo::read(dir);
                let (pre, post) = crate::sources::folder::pre_post_scripts(dir, &info)?;
                if let Some(pre) = pre {
                    script.sql = format!("{}\r\nGO\r\n{}", pre.trim_end(), script.sql);
                    script.batches.splice(0..0, deploy::split_batches(&pre));
                }
                if let Some(post) = post {
                    script.sql = format!("{}\r\nGO\r\n{}\r\nGO\r\n", script.sql.trim_end(), post.trim_end());
                    script.batches.extend(deploy::split_batches(&post));
                }
            }
            backup_into(request, &s.project.target, &mut script)?;
            let mut reply = script_json(&script);
            reply["actions"] = json!(script_actions(&script.batches));
            s.script = Some(script);
            s.selected = selected;
            Ok(reply)
        }
        "data_tables" => {
            let project: Project = field(request, "project")?;
            let mut opts = data_options(&project)?;
            // Views and checksums need both sides live.
            if [&project.source, &project.target].iter().any(|s| s.is_snapshot() || s.is_backup()) {
                opts = opts.without(DataOpt::IncludeViews).without(DataOpt::UseChecksumComparison);
            }
            let fresh = request.get("refresh").and_then(Value::as_bool).unwrap_or(false);
            let ((db1, _), loaded2) = load_sides(state, &project, false, fresh, progress)?;
            let db2 = loaded2.map(|l| l.0).unwrap_or_default();
            let (mut pairs, only1, only2) = crate::data::mapping::map_tables(&db1, &db2, &opts, &project.data_mappings);
            // What the comparison will leave out with these sources.
            let kinds = [&project.source, &project.target].map(side_kind);
            for p in &mut pairs {
                if p.excluded.is_none() {
                    p.excluded = crate::data::compare::unsupported(p, kinds, &db1, &db2);
                }
            }
            let mapping_warnings = crate::data::mapping::user_mapping_warnings(&db1, &db2, &opts, &project.data_mappings);
            let tables: Vec<Value> = pairs
                .iter()
                .map(|p| {
                    let candidates: Vec<String> = db1
                        .table(&p.source.schema, &p.source.name)
                        .map(|t| t.indexes.iter().filter(|i| i.primary_key || i.unique).map(|i| i.name.clone()).collect())
                        .unwrap_or_default();
                    json!({
                        "source": p.source.to_string(), "sourceSchema": p.source.schema, "sourceName": p.source.name,
                        "target": p.target.to_string(), "isView": p.is_view, "keyName": p.key_name, "problem": p.problem,
                        "key": p.key.iter().map(|&k| p.columns[k].source.clone()).collect::<Vec<_>>(),
                        "columns": p.columns.iter().map(|c| json!({"source": c.source, "target": c.target, "type": c.source_type.sql(), "targetType": c.target_type.sql()})).collect::<Vec<_>>(),
                        "where1": p.where1, "where2": p.where2, "keyCandidates": candidates,
                        "notes": notes_json(p), "excluded": p.excluded,
                    })
                })
                .collect();
            let all_columns: HashMap<String, Vec<String>> =
                db1.tables.iter().map(|t| (t.object_name().to_string(), t.columns.iter().map(|c| c.name.clone()).collect())).collect();
            let target_columns: HashMap<String, Vec<String>> =
                db2.tables.iter().map(|t| (t.object_name().to_string(), t.columns.iter().map(|c| c.name.clone()).collect())).collect();
            Ok(json!({
                "tables": tables,
                "mappingWarnings": mapping_warnings,
                "onlyIn1": only1.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "onlyIn2": only2.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "sourceColumns": all_columns,
                "targetColumns": target_columns,
                "sourceSchemas": db1.schemas.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "targetSchemas": db2.schemas.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            }))
        }
        // The Table mapping and Owner mapping tabs: what pairs with what, what the user mapped,
        // and what is left on each side. An empty target in a mapping unmaps a default pair.
        "mapping_preview" => {
            let project: Project = field(request, "project")?;
            let fresh = request.get("refresh").and_then(Value::as_bool).unwrap_or(false);
            mapping_preview(state, &project, fresh, progress)
        }
        "data_compare" => {
            let project: Project = field(request, "project")?;
            let opts = data_options(&project)?;
            // A scripts folder is read without a server, as SQL Data Compare reads it.
            let backups = [&project.source, &project.target].map(|s| s.is_backup().then(|| open_backup(s, progress)).transpose());
            let [b1, b2] = backups;
            let backups = [b1?, b2?];
            let mut folders: [Option<crate::sources::datafolder::DataFolder>; 2] = [None, None];
            for (k, side) in [&project.source, &project.target].into_iter().enumerate() {
                if side.is_scripts() {
                    progress(&format!("Reading the data scripts of '{}'", side.path));
                    let revision = (!side.revision.is_empty()).then_some(side.revision.as_str());
                    folders[k] = Some(crate::sources::datafolder::read(std::path::Path::new(&side.path), revision)?);
                }
            }
            let mut dbs = Vec::new();
            let mut specs = Vec::new();
            for (k, side) in [&project.source, &project.target].into_iter().enumerate() {
                progress(&format!("Reading {}", side.describe()));
                if let Some(f) = &folders[k] {
                    dbs.push(f.folder.database.clone());
                    specs.push(None);
                    continue;
                }
                if let Some(b) = &backups[k] {
                    dbs.push(b.read_database().map_err(|e| e.to_string())?);
                    specs.push(None);
                    continue;
                }
                let spec = side.connection(k as u8 + 1)?;
                dbs.push(catalog::read_database_with(&spec, false)?);
                specs.push(Some(spec));
            }
            let db2 = dbs.pop().unwrap_or_default();
            let db1 = dbs.pop().unwrap_or_default();
            let rows = |k: usize| match (&folders[k], &specs[k]) {
                (Some(f), _) => crate::data::compare::RowSource::Memory(&f.memory),
                (None, None) if backups[k].is_some() => crate::data::compare::RowSource::Backup(backups[k].as_ref().unwrap()),
                (None, Some(spec)) => crate::data::compare::RowSource::Live(spec),
                (None, None) => unreachable!(),
            };
            progress("Comparing data");
            let deselected: Vec<ObjectName> = project.deselected.iter().map(|d| ObjectName::new(&d.schema, &d.name)).collect();
            let only = only_selected(request);
            let result = crate::data::compare::compare_sources(
                rows(0),
                rows(1),
                &db1,
                &db2,
                &opts,
                &project.data_mappings,
                |p| !deselected.contains(&p.source) && only.as_ref().is_none_or(|o| o.contains(&p.source)),
                4,
                |p, _| progress(&progress_text(p, opts.has(DataOpt::UseChecksumComparison))),
            )?;
            state.next += 1;
            let id = state.next;
            let reply = data_session_json(id, &db1.name, &db2.name, &result);
            state.data.insert(id, DataSession { source_name: db1.name, db2, result, project, script: None, views: HashMap::new(), folders, kinds: [false, true, true, true], tables: None, selections: HashMap::new() });
            Ok(reply)
        }
        // `kind` 0–3 is a row kind (`RowKind::ALL` order), 4 every stored row in comparison order;
        // `sort` {column, side: source|target, descending} orders the rows by one column. Each row
        // carries its kind and its position within the kind (what `selections` refer to).
        "data_rows" => {
            let s = state.data.get_mut(&session(request)?).ok_or("the comparison is no longer open")?;
            let table: usize = field(request, "table")?;
            let kind: usize = field(request, "kind")?;
            let start: usize = field(request, "start")?;
            let count: usize = field(request, "count")?;
            let view = row_view(&mut s.views, &s.result.tables, table, kind, request.get("sort"))?;
            let t = &s.result.tables[table];
            let page: Vec<(RowKind, usize)> = view.iter().skip(start).take(count).copied().collect();
            let offsets: Vec<u64> = page.iter().map(|&(k, i)| t.offsets[k as usize][i]).collect();
            let rows = store::read_page(&t.path, &offsets, 0, offsets.len()).map_err(|e| e.to_string())?;
            let rows: Vec<Value> = rows
                .iter()
                .zip(&page)
                .map(|(r, &(k, i))| {
                    json!({
                        "kind": k as usize, "row": i,
                        "source": r.source.iter().map(|v| summary(&v.display())).collect::<Vec<_>>(),
                        "target": r.target.iter().map(|v| summary(&v.display())).collect::<Vec<_>>(),
                        "differs": r.differs,
                    })
                })
                .collect();
            let counted: u64 = if kind == ALL_ROWS { t.counts.iter().sum() } else { t.counts[kind] };
            Ok(json!({"rows": rows, "total": view.len(), "counted": counted}))
        }
        // The next row (from `start`, or before it with `backward`) of a `data_rows` view in which
        // some value contains `text`, ignoring case; null when there is none.
        "data_find" => {
            let s = state.data.get_mut(&session(request)?).ok_or("the comparison is no longer open")?;
            let table: usize = field(request, "table")?;
            let kind: usize = field(request, "kind")?;
            let start: usize = field(request, "start").unwrap_or(0);
            let text: String = field(request, "text")?;
            let backward = request.get("backward").and_then(Value::as_bool).unwrap_or(false);
            let view = row_view(&mut s.views, &s.result.tables, table, kind, request.get("sort"))?;
            let t = &s.result.tables[table];
            let needle = text.to_lowercase();
            let positions: Box<dyn Iterator<Item = usize>> = if backward { Box::new((0..start.min(view.len())).rev()) } else { Box::new(start..view.len()) };
            let positions: Vec<usize> = positions.collect();
            for chunk in positions.chunks(500) {
                let offsets: Vec<u64> = chunk.iter().map(|&p| { let (k, i) = view[p]; t.offsets[k as usize][i] }).collect();
                let rows = store::read_page(&t.path, &offsets, 0, offsets.len()).map_err(|e| e.to_string())?;
                if let Some(at) = rows.iter().position(|r| r.source.iter().chain(&r.target).any(|v| v.display().to_lowercase().contains(&needle))) {
                    return Ok(json!(chunk[at]));
                }
            }
            Ok(Value::Null)
        }
        // The whole value of one cell, for the value viewer: {table, kind, row, column, side}.
        "data_value" => {
            let s = state.data.get(&session(request)?).ok_or("the comparison is no longer open")?;
            let table: usize = field(request, "table")?;
            let kind: usize = field(request, "kind")?;
            let row: usize = field(request, "row")?;
            let column: usize = field(request, "column")?;
            let target = request.get("side").and_then(Value::as_str) == Some("target");
            let t = s.result.tables.get(table).ok_or("no such table")?;
            let offset = *t.offsets.get(kind).and_then(|o| o.get(row)).ok_or("no such row")?;
            let r = store::read_page(&t.path, &[offset], 0, 1).map_err(|e| e.to_string())?.pop().ok_or("no such row")?;
            let on_target = (target && !r.target.is_empty()) || r.source.is_empty();
            let side = if on_target { &r.target } else { &r.source };
            // A value compared by hash is read again from its database.
            if let Some(crate::data::value::Value::Large(..)) = side.get(column) {
                let n = if on_target { 2 } else { 1 };
                let full = crate::data::compare::fetch_side_value(&s.spec(n)?, &t.pair, n, column, side)?;
                return Ok(json!(full.display()));
            }
            Ok(json!(side.get(column).map(|v| v.display())))
        }
        // CSV files as `quill data /export` writes them: {folder, kinds, tables}.
        "data_export" => {
            let s = state.data.get_mut(&session(request)?).ok_or("the comparison is no longer open")?;
            let folder: String = field(request, "folder")?;
            let kinds: [bool; 4] = field(request, "kinds").unwrap_or([false, true, true, true]);
            let chosen: Option<Vec<usize>> = request.get("tables").and_then(|v| serde_json::from_value(v.clone()).ok());
            let (db1, db2) = (s.source_name.clone(), s.db2.name.clone());
            // Export only the chosen tables, then put every table back in its place.
            let all = std::mem::take(&mut s.result.tables);
            let (picked, rest): (Vec<_>, Vec<_>) = all.into_iter().enumerate().partition(|(i, _)| chosen.as_ref().is_none_or(|c| c.contains(i)));
            let (picked_ix, picked): (Vec<usize>, Vec<_>) = picked.into_iter().unzip();
            s.result.tables = picked;
            let live = |n: u8| (s.folders[n as usize - 1].is_none()).then(|| s.spec(n).ok()).flatten();
            let (spec1, spec2) = (live(1), live(2));
            let written = crate::commands::data::export_csv(&s.result, kinds, std::path::Path::new(&folder), &db1, &db2, [spec1.as_ref(), spec2.as_ref()]);
            let mut back: Vec<(usize, _)> = picked_ix.into_iter().zip(std::mem::take(&mut s.result.tables)).chain(rest).collect();
            back.sort_by_key(|(i, _)| *i);
            s.result.tables = back.into_iter().map(|(_, t)| t).collect();
            written.map_err(|e| format!("{folder}: {e}"))?;
            Ok(json!(true))
        }
        // An HTML page of the results: {path, kinds}.
        "data_report" => {
            let s = state.data.get(&session(request)?).ok_or("the comparison is no longer open")?;
            let path: String = field(request, "path")?;
            let kinds: [bool; 4] = field(request, "kinds").unwrap_or([false, true, true, true]);
            let side = |p: &SourceSpec, name: &str| format!("{}.{}", p.server, if name.is_empty() { &p.database } else { name });
            let html = crate::data::report::html(
                &s.result,
                &side(&s.project.source, &s.source_name),
                &side(&s.project.target, &s.db2.name),
                &crate::commands::console::now().report_format(),
                kinds,
            )
            .map_err(|e| e.to_string())?;
            std::fs::write(&path, html).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!(true))
        }
        "data_script" => {
            let id = session(request)?;
            let s = state.data.get_mut(&id).ok_or("the comparison is no longer open")?;
            let kinds: [bool; 4] = field(request, "kinds").unwrap_or([false, true, true, true]);
            let tables: Option<Vec<usize>> = request.get("tables").and_then(|v| serde_json::from_value(v.clone()).ok());
            let opts = data_options(&s.project)?;
            let side = |p: &crate::project::SourceSpec, name: &str| format!("{}.{}", p.server, if name.is_empty() { &p.database } else { name });
            let mut settings = crate::data::sync::SyncSettings::new(&side(&s.project.source, &s.source_name), &side(&s.project.target, &s.db2.name), &s.db2.name);
            settings.transaction_isolation_level = request.get("isolationLevel").and_then(Value::as_str).unwrap_or("SERIALIZABLE").to_ascii_uppercase();
            settings.kinds = kinds;
            s.kinds = kinds;
            settings.tables = tables.map(|ix| ix.iter().filter_map(|&i| s.result.tables.get(i)).map(|t| t.pair.source.clone()).collect());
            settings.selections = selections(request, &s.result)?;
            s.tables = settings.tables.clone();
            s.selections = settings.selections.clone();
            settings.max_transaction_bytes = request.get("maxTransactionSizeMB").and_then(Value::as_u64).filter(|&m| m > 0).map(|m| m * 1_048_576);
            settings.source_connection = s.spec(1).ok();
            let mut script = crate::data::sync::generate(&s.result, &s.db2, &opts, &settings)?;
            backup_into(request, &s.project.target, &mut script)?;
            let reply = script_json(&script);
            s.script = Some(script);
            Ok(reply)
        }
        // Saves an open data comparison to a file, and opens one again as a new session.
        "data_session_save" => {
            let s = state.data.get(&session(request)?).ok_or("the comparison is no longer open")?;
            let path: String = field(request, "path")?;
            let extra = json!({"project": s.project, "db2": s.db2, "sourceName": s.source_name});
            crate::data::saved::save(&s.result, extra, std::path::Path::new(&path))?;
            Ok(json!(true))
        }
        "data_session_load" => {
            let path: String = field(request, "path")?;
            let (result, extra) = crate::data::saved::load(std::path::Path::new(&path))?;
            let project: Project = serde_json::from_value(extra["project"].clone()).map_err(|e| format!("{path}: {e}"))?;
            let db2: Database = serde_json::from_value(extra["db2"].clone()).map_err(|e| format!("{path}: {e}"))?;
            let source_name = extra["sourceName"].as_str().unwrap_or_default().to_string();
            state.next += 1;
            let id = state.next;
            let mut reply = data_session_json(id, &source_name, &db2.name, &result);
            reply["project"] = serde_json::to_value(&project).map_err(|e| e.to_string())?;
            state.data.insert(id, DataSession { source_name, db2, result, project, script: None, views: HashMap::new(), folders: [None, None], kinds: [false, true, true, true], tables: None, selections: HashMap::new() });
            Ok(reply)
        }
        "deploy" => {
            let id = session(request)?;
            // A scripts folder as the target: the selected objects' files are rewritten.
            if let Some(s) = state.schema.get(&id)
                && s.project.target.is_scripts()
            {
                if s.script.is_none() {
                    return Err("create the deployment script first".into());
                }
                if !s.project.target.revision.is_empty() {
                    return Err("the target is a source control revision; deploy to the scripts folder itself".into());
                }
                let opts = schema_options(&s.project)?;
                let mut comparison = s.comparison.clone();
                for (i, d) in comparison.differences.iter_mut().enumerate() {
                    d.selected = s.selected.contains(&i);
                }
                progress(&format!("Writing to the scripts folder '{}'", s.project.target.path));
                let target = crate::sources::folder::read(std::path::Path::new(&s.project.target.path))?;
                let changes = crate::sources::folder::sync(&s.db1, &target, &comparison.differences, &opts)?;
                let list = |v: &[std::path::PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
                return Ok(json!({"written": list(&changes.written), "deleted": list(&changes.deleted)}));
            }
            // A data comparison whose target is a scripts folder: run the script on the loaded
            // copy, then write the changed tables' data scripts.
            if let Some(s) = state.data.get_mut(&id)
                && let Some(f) = &mut s.folders[1]
            {
                if s.script.is_none() {
                    return Err("create the deployment script first".into());
                }
                // Only the tables and rows the script deploys.
                let (kinds, tables, chosen) = (s.kinds, &s.tables, &s.selections);
                let select = |p: &crate::data::mapping::TablePair, k: RowKind, i: u64| {
                    kinds[k as usize] && tables.as_ref().is_none_or(|t| t.contains(&p.source)) && chosen.get(&p.source).is_none_or(|r| r[k as usize].selected(i))
                };
                let files = f.write_back(&s.result, &select)?;
                return Ok(json!({"batches": 0, "written": files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()}));
            }
            let (script, target) = if let Some(s) = state.schema.get(&id) {
                (s.script.clone(), s.project.target.clone())
            } else if let Some(s) = state.data.get(&id) {
                (s.script.clone(), s.project.target.clone())
            } else {
                return Err("the comparison is no longer open".into());
            };
            let script = script.ok_or("create the deployment script first")?;
            if target.is_snapshot() {
                return Err("the target is a snapshot; only a script can be created".into());
            }
            if target.is_empty_database() {
                return Err("the target is an empty database; only a script can be created".into());
            }
            let spec = target.connection(2)?;
            let ran = deploy::execute(&spec, &script, |i, n| progress(&format!("Running batch {} of {n}", i + 1)))?;
            Ok(json!({"batches": ran}))
        }
        // What the backup page starts with: the instance's backup folder and the automatic name.
        "backup_defaults" => {
            let id = session(request)?;
            let target = state.schema.get(&id).map(|s| s.project.target.clone()).or_else(|| state.data.get(&id).map(|s| s.project.target.clone())).ok_or("the comparison is no longer open")?;
            let spec = target.connection(2)?;
            let caps = schema::backup::capabilities(&spec)?;
            let o = schema::backup::BackupOptions::default();
            let plan = schema::backup::Plan { options: &o, database: &spec.database, utc: schema::backup::utc_stamp(), caps: &caps };
            Ok(json!({"capabilities": caps, "database": spec.database, "file": plan.file()}))
        }        "close" => {
            let id = session(request)?;
            state.schema.remove(&id);
            state.data.remove(&id);
            Ok(json!(true))
        }
        _ if UI_OPS.contains(&op) => ui_op(state, op, request, progress),
        other => Err(format!("unknown op '{other}'")),
    }
}

/// "Back up target before deployment": the request's `backup` settings (with the SQL Backup
/// password) put the backup first in the script, as SQL Compare does.
fn backup_into(request: &Value, target: &SourceSpec, script: &mut DeploymentScript) -> Result<(), String> {
    let Some(b) = request.get("backup").filter(|b| !b.is_null()) else { return Ok(()) };
    let o: schema::backup::BackupOptions = serde_json::from_value(b.clone()).map_err(|e| format!("'backup': {e}"))?;
    if !o.enabled {
        return Ok(());
    }
    if target.is_snapshot() || target.is_empty_database() || target.is_scripts() {
        return Err("only a live database can be backed up before deployment".into());
    }
    let spec = target.connection(2)?;
    let caps = schema::backup::capabilities(&spec)?;
    if o.sqb() && caps.sqb_version == 0 {
        return Err(format!("SQL Backup is not installed on {}", spec.server));
    }
    schema::backup::Plan { options: &o, database: &spec.database, utc: schema::backup::utc_stamp(), caps: &caps }.add_to(script);
    Ok(())
}

fn mapping_preview(state: &mut State, project: &Project, fresh: bool, progress: &Progress) -> Result<Value, String> {
    let data = project.kind == crate::project::ProjectKind::Data;
    let (decrypt, case_sensitive) = if data {
        (false, data_options(project)?.has(DataOpt::CaseSensitiveObjectDefinition))
    } else {
        let o = schema_options(project)?;
        (o.has(Opt::DecryptPost2kEncryptedObjects), o.has(Opt::CaseSensitiveObjectDefinition))
    };
    let ((db1, _), loaded2) = load_sides(state, project, decrypt, fresh, progress)?;
    let db2 = loaded2.map(|l| l.0).unwrap_or_default();
    let key = |s: &str| if case_sensitive { s.to_string() } else { s.to_lowercase() };
    let same = |a: &str, b: &str| key(a) == key(b);

    // Owners: the user's mappings, then by name.
    let user_schemas = if data { &project.data_mappings.schemas } else { &project.schema_mappings.schemas };
    let (left, right): (Vec<&str>, Vec<&str>) = (db1.schemas.iter().map(|s| s.name.as_str()).collect(), db2.schemas.iter().map(|s| s.name.as_str()).collect());
    let mut used = vec![false; right.len()];
    let mut owners = Vec::new();
    let mut owners1 = Vec::new();
    for l in &left {
        let user = user_schemas.iter().find(|(a, _)| same(a, l));
        let want = user.map(|(_, b)| b.as_str()).unwrap_or(l);
        match right.iter().position(|r| !want.is_empty() && same(r, want)) {
            Some(j) if !used[j] => {
                used[j] = true;
                owners.push(json!({"source": l, "target": right[j], "user": user.is_some()}));
            }
            _ => owners1.push(json!(l)),
        }
    }
    let owners2: Vec<Value> = right.iter().enumerate().filter(|(j, _)| !used[*j]).map(|(_, r)| json!(r)).collect();
    let target_schema = |s: &str| user_schemas.iter().find(|(a, _)| same(a, s)).map(|(_, b)| b.clone()).unwrap_or_else(|| s.to_string());

    let columns_of = |db: &Database, n: &ObjectName| db.table(&n.schema, &n.name).map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()).unwrap_or_default();
    let mut tables = Vec::new();
    let (only1, only2): (Vec<ObjectName>, Vec<ObjectName>);
    if data {
        let opts = data_options(project)?;
        let (pairs, o1, o2) = crate::data::mapping::map_tables(&db1, &db2, &opts, &project.data_mappings);
        for p in &pairs {
            let user = project.data_mappings.tables.iter().any(|t| t.source == p.source && t.target.as_ref().is_some_and(|x| !x.name.is_empty()));
            let explicit = project.data_mappings.tables.iter().find(|t| t.source == p.source).map(|t| t.columns.clone()).unwrap_or_default();
            let cols: Vec<Value> = p.columns.iter().map(|c| json!({"source": c.source, "target": c.target, "auto": !explicit.iter().any(|(a, _)| a == &c.source) && !c.source.eq_ignore_ascii_case(&c.target)})).collect();
            tables.push(json!({"source": p.source, "target": p.target, "user": user, "isView": p.is_view, "columns": cols,
                "sourceColumns": columns_of(&db1, &p.source), "targetColumns": columns_of(&db2, &p.target)}));
        }
        (only1, only2) = (o1, o2);
    } else {
        // As schema::compare pairs tables, with the columns "Auto-map similar columns" pairs.
        let auto = schema::compare(&db1, &db2, &schema_options(project)?, &project.schema_mappings).auto_mapped_columns;
        let mut matched = vec![false; db2.tables.len()];
        let mut alone = Vec::new();
        for t1 in db1.tables.iter().filter(|t| !t.history_table) {
            let n1 = t1.object_name();
            let mapping = project.schema_mappings.tables.iter().find(|m| same(&m.source.schema, &t1.schema) && same(&m.source.name, &t1.name));
            let (schema, name) = match mapping {
                Some(m) => (m.target.schema.clone(), m.target.name.clone()),
                None => (target_schema(&t1.schema), t1.name.clone()),
            };
            let found = db2.tables.iter().position(|t| !name.is_empty() && !t.history_table && same(&t.schema, &schema) && same(&t.name, &name));
            match found {
                Some(j) => {
                    matched[j] = true;
                    let n2 = db2.tables[j].object_name();
                    let explicit = mapping.map(|m| m.columns.clone()).unwrap_or_default();
                    let mut cols: Vec<Value> = explicit.iter().map(|(a, b)| json!({"source": a, "target": b, "auto": false})).collect();
                    cols.extend(auto.iter().filter(|(t, _, _)| *t == n1).map(|(_, a, b)| json!({"source": a, "target": b, "auto": true})));
                    tables.push(json!({"source": n1, "target": n2, "user": mapping.is_some(), "columns": cols,
                        "sourceColumns": columns_of(&db1, &n1), "targetColumns": columns_of(&db2, &n2)}));
                }
                None => alone.push(n1),
            }
        }
        only1 = alone;
        only2 = db2.tables.iter().enumerate().filter(|(j, t)| !matched[*j] && !t.history_table).map(|(_, t)| t.object_name()).collect();
    }
    Ok(json!({
        "owners": {"mapped": owners, "only1": owners1, "only2": owners2},
        "tables": {"mapped": tables, "only1": only1, "only2": only2},
    }))
}

/// The `data_rows` kind that lists every stored row.
const ALL_ROWS: usize = 4;

/// Values as the grid shows them: 256 characters at most, then `…` (`data_value` has the rest).
fn summary(text: &str) -> String {
    match text.char_indices().nth(256) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// The rows of a `data_rows` view, as (kind, position within the kind), built once and kept.
type Views = HashMap<(usize, usize, String), Vec<(RowKind, usize)>>;

fn row_view<'a>(views: &'a mut Views, tables: &[crate::data::compare::TableResult], table: usize, kind: usize, sort: Option<&Value>) -> Result<&'a Vec<(RowKind, usize)>, String> {
    let t = tables.get(table).ok_or("no such table")?;
    if kind > ALL_ROWS {
        return Err("no such row kind".into());
    }
    let sort = sort.filter(|v| !v.is_null());
    let key = (table, kind, sort.map(Value::to_string).unwrap_or_default());
    if !views.contains_key(&key) {
        let mut view: Vec<(RowKind, usize)> = if kind == ALL_ROWS {
            // Comparison order is the order the rows were written in.
            let mut all: Vec<(u64, RowKind, usize)> =
                RowKind::ALL.iter().flat_map(|&k| t.offsets[k as usize].iter().enumerate().map(move |(i, &o)| (o, k, i))).collect();
            all.sort_by_key(|x| x.0);
            all.into_iter().map(|(_, k, i)| (k, i)).collect()
        } else {
            (0..t.offsets[kind].len()).map(|i| (RowKind::ALL[kind], i)).collect()
        };
        if let Some(sort) = sort {
            let column = sort.get("column").and_then(Value::as_u64).ok_or("sort: 'column' is missing")? as usize;
            // `key`: whichever side the row has.
            let side = sort.get("side").and_then(Value::as_str).unwrap_or("source").to_string();
            let descending = sort.get("descending").and_then(Value::as_bool).unwrap_or(false);
            let mut values: HashMap<(RowKind, usize), Option<crate::data::value::Value>> = HashMap::new();
            for &k in RowKind::ALL.iter().filter(|&&k| view.iter().any(|v| v.0 == k)) {
                for (i, r) in store::read_all(&t.path, &t.offsets[k as usize]).map_err(|e| e.to_string())?.into_iter().enumerate() {
                    // A side the row is missing from sorts last.
                    let (mine, other) = if side == "target" { (r.target, r.source) } else { (r.source, r.target) };
                    let values_of = if !mine.is_empty() { mine } else if side == "key" || k == RowKind::Identical { other } else { Vec::new() };
                    values.insert((k, i), values_of.into_iter().nth(column));
                }
            }
            view.sort_by(|a, b| {
                let ord = match (&values[a], &values[b]) {
                    // Text sorts ignoring case first, as people read it.
                    (Some(crate::data::value::Value::Str(x)), Some(crate::data::value::Value::Str(y))) => x.to_lowercase().cmp(&y.to_lowercase()).then_with(|| x.cmp(y)),
                    (Some(x), Some(y)) => crate::data::value::cmp_key(x, y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                };
                if descending && values[a].is_some() && values[b].is_some() { ord.reverse() } else { ord }
            });
        }
        views.insert(key.clone(), view);
    }
    Ok(&views[&key])
}

/// What a project side is, for the data engine's source rules.
fn side_kind(s: &SourceSpec) -> crate::data::compare::SideKind {
    use crate::data::compare::SideKind;
    if s.is_scripts() {
        SideKind::Folder
    } else if s.is_backup() || s.is_snapshot() {
        SideKind::Backup
    } else {
        SideKind::Live
    }
}

fn notes_json(p: &crate::data::mapping::TablePair) -> Vec<Value> {
    p.notes.iter().map(|n| json!({"code": n.code, "severity": format!("{:?}", n.severity), "text": n.text, "notCompared": n.not_compared()})).collect()
}

/// "Comparing [t]" or "Performing checksum comparison of [t] to [u]".
fn progress_text(p: &crate::data::mapping::TablePair, checksum: bool) -> String {
    let what = if checksum { "Performing checksum comparison of" } else { "Comparing" };
    if p.source == p.target { format!("{what} {}", p.source) } else { format!("{what} {} to {}", p.source, p.target) }
}

/// The reply describing a data comparison session.
fn data_session_json(id: u64, source: &str, target: &str, result: &DataComparison) -> Value {
    let tables: Vec<Value> = result
        .tables
        .iter()
        .enumerate()
        .map(|(i, t)| {
            json!({
                "index": i, "source": t.pair.source.to_string(), "target": t.pair.target.to_string(),
                "isView": t.pair.is_view, "counts": t.counts, "error": t.error, "byChecksum": t.by_checksum,
                "repeatedKeys": t.repeated_keys,
                "columns": t.pair.columns.iter().map(|c| c.source.clone()).collect::<Vec<_>>(),
                "targetColumns": t.pair.columns.iter().map(|c| c.target.clone()).collect::<Vec<_>>(),
                "key": t.pair.key,
                "columnDifferences": t.column_differences,
                "notes": notes_json(&t.pair),
            })
        })
        .collect();
    json!({
        "session": id,
        "sourceName": source,
        "targetName": target,
        "tables": tables,
        "onlyIn1": result.only_in_1.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "onlyIn2": result.only_in_2.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "excluded": result.excluded.iter().map(|p| json!({"source": p.source.to_string(), "target": p.target.to_string(), "isView": p.is_view, "reason": p.excluded})).collect::<Vec<_>>(),
    })
}

/// `selections` of `data_script`: per table (index), the kinds deployed and row choices.
/// `{"table": 0, "kinds": [false, true, false, true], "rows": {"different": {"exclude": [3]}, "onlyIn1": {"include": [0, 1]}}}`;
/// a kind may also be given as `{"included": bool, "toggled": [..]}`.
fn selections(request: &Value, result: &DataComparison) -> Result<HashMap<ObjectName, [RowSelection; 4]>, String> {
    let mut out = HashMap::new();
    let Some(list) = request.get("selections").and_then(Value::as_array) else { return Ok(out) };
    for item in list {
        let index = item.get("table").and_then(Value::as_u64).ok_or("selections: 'table' is missing")? as usize;
        let t = result.tables.get(index).ok_or_else(|| format!("selections: no table {index}"))?;
        let mut sel: [RowSelection; 4] = Default::default();
        if let Some(kinds) = item.get("kinds") {
            let kinds: [bool; 4] = serde_json::from_value(kinds.clone()).map_err(|e| format!("selections: 'kinds': {e}"))?;
            for (k, on) in kinds.iter().enumerate() {
                sel[k].included = *on;
            }
        }
        if let Some(rows) = item.get("rows") {
            for (name, kind) in [("different", RowKind::Different), ("onlyIn1", RowKind::OnlyIn1), ("onlyIn2", RowKind::OnlyIn2)] {
                let Some(r) = rows.get(name) else { continue };
                let list = |key: &str| -> Result<Option<std::collections::BTreeSet<u64>>, String> {
                    r.get(key).map(|v| serde_json::from_value(v.clone()).map_err(|e| format!("selections: '{name}.{key}': {e}"))).transpose()
                };
                let s = &mut sel[kind as usize];
                if let Some(rows) = list("include")? {
                    *s = RowSelection { included: false, toggled: rows };
                } else if let Some(rows) = list("exclude")? {
                    *s = RowSelection { included: true, toggled: rows };
                } else {
                    *s = serde_json::from_value(r.clone()).map_err(|e| format!("selections: '{name}': {e}"))?;
                }
            }
        }
        out.insert(t.pair.source.clone(), sel);
    }
    Ok(out)
}

/// The comparison with the project's unticked objects and what its filter leaves out,
/// unselected (as `quill schema /project` does).
fn compare_schemas(db1: &Database, db2: &Database, opts: &Options, project: &Project) -> Result<Comparison, String> {
    let mut comparison = schema::compare(db1, db2, opts, &project.schema_mappings);
    for d in &mut comparison.differences {
        if project.deselected.iter().any(|x| x.schema == d.name.schema && x.name == d.name.name && (x.kind.is_empty() || x.kind == d.object_type.label())) {
            d.selected = false;
        }
    }
    if let Some(f) = project_filter(project)? {
        crate::commands::sources::apply_filter(&f, &mut comparison);
    }
    Ok(comparison)
}

fn project_filter(project: &Project) -> Result<Option<schema::filter::Filter>, String> {
    project.filter.as_deref().map(|xml| schema::filter::Filter::from_xml(xml).map_err(|e| format!("the project's filter is invalid: {e}"))).transpose()
}

/// Indexes of the differences a filter leaves out.
fn filtered(f: Option<&schema::filter::Filter>, comparison: &Comparison) -> Vec<usize> {
    let Some(f) = f else { return Vec::new() };
    comparison.differences.iter().enumerate().filter(|(_, d)| !f.includes(d)).map(|(i, _)| i).collect()
}

/// The reply describing a schema comparison session.
fn schema_reply(id: u64, s: &SchemaSession) -> Value {
    let differences: Vec<Value> = s.comparison.differences.iter().enumerate().map(|(i, d)| difference_json(i, d)).collect();
    let auto: Vec<Value> = s.comparison.auto_mapped_columns.iter().map(|(t, a, b)| json!({"table": t.to_string(), "source": a, "target": b})).collect();
    json!({
        "session": id,
        "sourceName": s.db1.name,
        "targetName": s.db2.name,
        "differences": differences,
        "autoMapped": auto,
        "filtered": filtered(project_filter(&s.project).ok().flatten().as_ref(), &s.comparison),
    })
}

/// Differences that the selected ones reference (created or changed objects need them) and
/// that are not selected: {index, referencedBy: [names]}.
fn dependencies(s: &SchemaSession, selected: &[usize]) -> Vec<Value> {
    let diffs = &s.comparison.differences;
    let mut found: Vec<(usize, Vec<String>)> = Vec::new();
    let mut queue: Vec<usize> = selected.iter().copied().filter(|&i| diffs.get(i).is_some_and(|d| d.kind != DiffKind::OnlyIn2)).collect();
    while let Some(i) = queue.pop() {
        let from = &diffs[i].name;
        for dep in s.db1.dependencies.iter().filter(|d| d.from == *from) {
            let Some(j) = diffs.iter().position(|d| d.name == dep.to && matches!(d.kind, DiffKind::OnlyIn1 | DiffKind::Different)) else { continue };
            if selected.contains(&j) {
                continue;
            }
            match found.iter_mut().find(|f| f.0 == j) {
                Some(f) => {
                    if !f.1.contains(&from.to_string()) {
                        f.1.push(from.to_string());
                    }
                }
                None => {
                    found.push((j, vec![from.to_string()]));
                    queue.push(j);
                }
            }
        }
    }
    found.sort_by_key(|f| f.0);
    found.into_iter().map(|(i, by)| json!({"index": i, "referencedBy": by})).collect()
}

fn report_side(spec: &SourceSpec, db: &Database) -> schema::report::DataSource {
    if spec.is_snapshot() {
        let created = std::fs::metadata(&spec.path).and_then(|m| m.modified()).ok().map(|t| crate::commands::console::utc(t).report_format());
        schema::report::DataSource { kind: "snapshot", server: String::new(), database: db.name.clone(), created }
    } else {
        schema::report::DataSource { kind: "live", server: spec.server.clone(), database: db.name.clone(), created: None }
    }
}

/// A report as `quill schema /report` writes it, of the chosen differences.
fn schema_report(s: &SchemaSession, format: &str, selected: Option<&[usize]>, include_identical: bool) -> Vec<u8> {
    use schema::report::{self, Item, Report};
    let ignore_ws = schema_options(&s.project).map(|o| o.has(Opt::IgnoreWhiteSpace)).unwrap_or(true);
    let mut items: Vec<Item> = s
        .comparison
        .differences
        .iter()
        .enumerate()
        .filter(|(i, d)| if d.kind == DiffKind::Equal { include_identical } else { selected.is_none_or(|sel| sel.contains(i)) })
        .map(|(_, d)| {
            let target = d.target_name.as_ref().unwrap_or(&d.name);
            let (one, two) = match d.kind {
                DiffKind::OnlyIn1 => (Some(&d.name), None),
                DiffKind::OnlyIn2 => (None, Some(&d.name)),
                _ => (Some(&d.name), Some(target)),
            };
            let left = one.and_then(|n| object_sql(&s.db1, d.object_type, n));
            let right = two.and_then(|n| object_sql(&s.db2, d.object_type, n));
            Item {
                object_type: d.object_type,
                kind: d.kind,
                full_name: if d.name.schema.is_empty() { format!("[{}]", d.name.name) } else { d.name.quoted() },
                owner1: one.map(|n| n.schema.clone()).unwrap_or_default(),
                name1: one.map(|n| n.name.clone()).unwrap_or_default(),
                owner2: two.map(|n| n.schema.clone()).unwrap_or_default(),
                name2: two.map(|n| n.name.clone()).unwrap_or_default(),
                lines: schema::linediff::compare(left.as_deref(), right.as_deref(), ignore_ws),
            }
        })
        .collect();
    report::order(&mut items);
    let r = Report {
        timestamp: crate::commands::console::now().report_format(),
        source: report_side(&s.project.source, &s.db1),
        target: report_side(&s.project.target, &s.db2),
        items,
        include_identical,
    };
    match format {
        "Classic" => report::classic(&r).into_bytes(),
        "Excel" => report::excel(&r).into_bytes(),
        "Xlsx" => report::xlsx(&r),
        "Xml" => report::xml(&r),
        _ => report::html(&r).into_bytes(),
    }
}

fn difference_json(i: usize, d: &crate::schema::Difference) -> Value {
    json!({
        "index": i,
        "type": d.object_type.label(),
        "schema": d.name.schema,
        "name": d.name.name,
        "targetName": d.target_name.as_ref().map(ToString::to_string),
        "kind": format!("{:?}", d.kind),
        "selected": d.selected,
        "trivial": d.trivial,
        "properties": d.properties,
    })
}

fn script_json(script: &DeploymentScript) -> Value {
    json!({
        "sql": script.sql,
        "batches": script.batches.len(),
        "warnings": script.warnings.iter().map(|w| json!({"severity": format!("{:?}", w.severity), "object": w.object, "message": w.message})).collect::<Vec<_>>(),
    })
}

/// The SQL of one object, for the side-by-side view and the comparison reports.
pub fn object_sql(db: &Database, ty: ObjectType, name: &ObjectName) -> Option<String> {
    let same = |s: &str, n: &str| s == name.schema && n == name.name;
    match ty {
        ObjectType::Table => db.tables.iter().find(|t| same(&t.schema, &t.name)).map(|t| {
            let table = t.object_name().quoted();
            let mut parts = vec![ddl::create_table(t, None, false)];
            let mut ixs: Vec<&catalog::Index> = t.indexes.iter().collect();
            ixs.sort_by_key(|i| (!i.primary_key, i.name.clone()));
            for ix in ixs {
                parts.extend(ddl::create_index(&table, ix));
            }
            for c in &t.check_constraints {
                parts.extend(ddl::add_check(&table, c, false));
            }
            for fk in &t.foreign_keys {
                parts.extend(ddl::add_foreign_key(&table, fk, false));
            }
            for tr in &t.triggers {
                parts.push(tr.definition.trim().to_string());
            }
            parts.extend(schema::ddl_objects::table_parts(t, false));
            parts.join("\nGO\n")
        }),
        ObjectType::View => db.views.iter().find(|v| same(&v.schema, &v.name)).map(|v| {
            let mut parts = vec![v.definition.trim().to_string()];
            for ix in &v.indexes {
                parts.extend(ddl::create_index(&name.quoted(), ix));
            }
            parts.join("\nGO\n")
        }),
        ObjectType::Function | ObjectType::StoredProcedure => db
            .routines
            .iter()
            .find(|r| same(&r.schema, &r.name))
            .map(|r| if r.encrypted { "-- The object is encrypted.".into() } else { r.definition.trim().to_string() }),
        ObjectType::UserDefinedType => db.types.iter().find(|t| same(&t.schema, &t.name)).map(ddl::create_type),
        ObjectType::Sequence => db.sequences.iter().find(|s| same(&s.schema, &s.name)).map(ddl::create_sequence),
        ObjectType::Synonym => db.synonyms.iter().find(|s| same(&s.schema, &s.name)).map(ddl::create_synonym),
        ObjectType::Role | ObjectType::User => db.principals.iter().find(|p| p.name == name.name).map(|p| {
            let mut lines = vec![format!("-- {} {}", p.kind.to_lowercase().replace('_', " "), crate::db::quote(&p.name))];
            if let Some(l) = &p.login {
                lines.push(format!("-- login: {l}"));
            }
            if let Some(s) = &p.default_schema {
                lines.push(format!("-- default schema: {s}"));
            }
            for r in &p.member_of {
                lines.push(format!("ALTER ROLE {} ADD MEMBER {}", crate::db::quote(r), crate::db::quote(&p.name)));
            }
            for g in db.permissions.iter().filter(|g| g.class == "DATABASE" && g.grantee == p.name) {
                lines.push(g.grant_sql());
            }
            lines.join("\n")
        }),
        ObjectType::Schema => db
            .schemas
            .iter()
            .find(|s| s.name == name.name)
            .map(|s| format!("CREATE SCHEMA {} AUTHORIZATION {}", crate::db::quote(&s.name), crate::db::quote(&s.owner))),
        ObjectType::ExtendedProperty => db
            .extended_properties
            .iter()
            .find(|p| p.class == "DATABASE" && p.name == name.name)
            .map(|p| ddl::add_xprop(p, "NULL, NULL, NULL, NULL, NULL, NULL")),
        other => schema::ddl_objects::object_sql(db, other, name),
    }
}

fn option_definitions() -> Value {
    let schema: Vec<Value> = schema::options::ALL
        .iter()
        .map(|o| {
            json!({
                "name": o.name(),
                "label": o.label(),
                "group": format!("{:?}", o.group()),
                "kind": format!("{:?}", o.kind()),
                "default": o.is_default(),
                "negated": o.presented_as_negation(),
                "requires": o.requires().iter().map(|r| r.name()).collect::<Vec<_>>(),
                "description": help::schema(o.name()),
            })
        })
        .collect();
    // MissingFrom2AsInclude is a command-line and project flag, not a check box.
    let data: Vec<Value> = crate::data::options::ALL
        .iter()
        .filter(|o| **o != DataOpt::MissingFrom2AsInclude)
        .map(|o| json!({"name": o.name(), "label": o.label(), "group": format!("{:?}", o.group()), "default": o.is_default(), "description": help::data(o.name())}))
        .collect();
    let _ = (Opt::IgnoreWhiteSpace, DataOpt::IgnoreSpaces);
    json!({"schema": schema, "data": data})
}

// ---- Operations only the Compare windows use ----

#[path = "compare_help.rs"]
mod help;

const UI_OPS: &[&str] = &["format_sql", "collations", "revisions", "folder_preview"];

/// - `format_sql` {source, target, style?} formats both texts with the user's QuillSQL style
///   (`style` path, else `%USERPROFILE%\.quillsql.json`, else the defaults), for the formatted
///   difference view; a text that cannot be formatted comes back as it was.
/// - `collations` {source} → {collations, default, dataPath, logPath}, for New database.
/// - `revisions` {path} → the latest revisions of a scripts folder in git, Mercurial or
///   Subversion: [{id, author, date, message}].
/// - `folder_preview` {session} → {written, deleted}: the files that deploying the last script
///   to a scripts folder would change.
fn ui_op(state: &mut State, op: &str, request: &Value, progress: &Progress) -> Result<Value, String> {
    match op {
        "format_sql" => {
            let style = request
                .get("style")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("USERPROFILE").map(|h| std::path::Path::new(&h).join(".quillsql.json")));
            let opts = style
                .filter(|p| p.is_file())
                .and_then(|p| std::fs::read(&p).ok())
                .and_then(|b| serde_json::from_str::<Value>(&crate::project::decode_text(&b)).ok())
                .and_then(|v| crate::style::options_from_value(&v).ok())
                .map(|(o, _)| o)
                .unwrap_or_default();
            let format = |key: &str| request.get(key).and_then(Value::as_str).map(|t| crate::format(t, &opts).unwrap_or_else(|_| t.to_string()));
            Ok(json!({"source": format("source"), "target": format("target")}))
        }
        "collations" => {
            let side: SourceSpec = field(request, "source")?;
            let mut spec = side.connection(1)?;
            spec.database = "master".into();
            let rt = crate::db::runtime()?;
            rt.block_on(async {
                let mut c = crate::db::connect_async(&spec).await?;
                let names = crate::db::query(&mut c, "SELECT name FROM sys.fn_helpcollations() ORDER BY name").await?;
                let info = crate::db::query(
                    &mut c,
                    "SELECT CAST(SERVERPROPERTY('Collation') AS nvarchar(128)), CAST(SERVERPROPERTY('InstanceDefaultDataPath') AS nvarchar(260)), CAST(SERVERPROPERTY('InstanceDefaultLogPath') AS nvarchar(260))",
                )
                .await?;
                let r = info.first().ok_or("no reply")?;
                Ok(json!({
                    "collations": names.iter().filter_map(|r| r.get::<&str, _>(0).map(str::to_string)).collect::<Vec<_>>(),
                    "default": r.get::<&str, _>(0), "dataPath": r.get::<&str, _>(1), "logPath": r.get::<&str, _>(2),
                }))
            })
        }
        "revisions" => {
            let path: String = field(request, "path")?;
            progress("Reading the history");
            Ok(json!(revisions(std::path::Path::new(&path))?))
        }
        "folder_preview" => {
            let s = state.schema.get(&session(request)?).ok_or("the comparison is no longer open")?;
            if !s.project.target.is_scripts() || s.script.is_none() {
                return Ok(json!({"written": [], "deleted": []}));
            }
            let opts = schema_options(&s.project)?;
            let mut comparison = s.comparison.clone();
            for (i, d) in comparison.differences.iter_mut().enumerate() {
                d.selected = s.selected.contains(&i);
            }
            let target = crate::sources::folder::read(std::path::Path::new(&s.project.target.path))?;
            let changes = crate::sources::folder::preview(&s.db1, &target, &comparison.differences, &opts)?;
            let list = |v: &[std::path::PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
            Ok(json!({"written": list(&changes.written), "deleted": list(&changes.deleted)}))
        }
        _ => Err(format!("unknown op '{op}'")),
    }
}

/// Data projects with "Include new tables" off (`includeNewTables: false`): only the tables
/// named in `selected` are compared.
fn only_selected(request: &Value) -> Option<Vec<ObjectName>> {
    let p = request.get("project")?;
    if p.get("includeNewTables").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let list = p.get("selected").and_then(Value::as_array).cloned().unwrap_or_default();
    Some(list.iter().map(|t| ObjectName::new(t["schema"].as_str().unwrap_or("dbo"), t["name"].as_str().unwrap_or_default())).collect())
}

/// A scripts folder without database information is read as the side's `sqlVersion` and
/// `defaultCollation` (what SQL Compare asks for such a folder).
fn folder_defaults(request: &Value, key: &str, db: &mut Database) {
    let Some(side) = request.get("project").and_then(|p| p.get(key)) else { return };
    if side["type"] != "scripts" || side.get("revision").and_then(Value::as_str).is_some_and(|r| !r.is_empty()) {
        return;
    }
    let path = side["path"].as_str().unwrap_or_default();
    if crate::sources::folder::FolderInfo::read(std::path::Path::new(path)).0.file.is_some() {
        return;
    }
    if let Some(v) = side.get("sqlVersion").and_then(Value::as_str).and_then(|v| v.parse::<u32>().ok()).filter(|v| (10..=17).contains(v)) {
        db.server.major_version = v;
        db.compatibility_level = v * 10;
    }
    if let Some(c) = side.get("defaultCollation").and_then(Value::as_str).filter(|c| !c.is_empty()) {
        db.collation = c.to_string();
    }
}

/// `modified1` / `modified2` of each difference: when the object last changed on each side.
fn add_modified(reply: &mut Value, s: &SchemaSession) {
    fn modified(db: &Database, ty: ObjectType, name: &ObjectName) -> Option<String> {
        let same = |schema: &str, n: &str| schema == name.schema && n == name.name;
        match ty {
            ObjectType::Table => db.tables.iter().find(|t| same(&t.schema, &t.name)).and_then(|t| t.modified.clone()),
            ObjectType::View => db.views.iter().find(|v| same(&v.schema, &v.name)).and_then(|v| v.modified.clone()),
            ObjectType::Function | ObjectType::StoredProcedure => db.routines.iter().find(|r| same(&r.schema, &r.name)).and_then(|r| r.modified.clone()),
            _ => None,
        }
    }
    let Some(list) = reply.get_mut("differences").and_then(Value::as_array_mut) else { return };
    for (v, d) in list.iter_mut().zip(&s.comparison.differences) {
        let target = d.target_name.as_ref().unwrap_or(&d.name);
        v["modified1"] = json!(modified(&s.db1, d.object_type, &d.name));
        v["modified2"] = json!(modified(&s.db2, d.object_type, target));
    }
}

/// What the statements of a deployment script do, in the order they run: {action, object,
/// detail}, for the deployment summary. Statements that only steer the script (SET, PRINT,
/// transactions, error handling, existence checks) are left out.
fn script_actions(batches: &[String]) -> Vec<Value> {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    const KINDS: &str = r"VIEW|PROCEDURE|PROC|FUNCTION|TRIGGER|SCHEMA|TYPE|SEQUENCE|SYNONYM|ROLE|USER|ASSEMBLY|DEFAULT|RULE|XML\s+SCHEMA\s+COLLECTION|PARTITION\s+FUNCTION|PARTITION\s+SCHEME|FULLTEXT\s+CATALOG|FULLTEXT\s+INDEX\s+ON|STATISTICS|QUEUE|SERVICE|CONTRACT|MESSAGE\s+TYPE|CERTIFICATE|SYMMETRIC\s+KEY|ASYMMETRIC\s+KEY|SECURITY\s+POLICY|EVENT\s+NOTIFICATION|SEARCH\s+PROPERTY\s+LIST|EXTERNAL\s+\w+";
    let patterns = PATTERNS.get_or_init(|| {
        let n = r"((?:\[(?:[^\]]|\]\])+\]|[\w@#$]+)(?:\.(?:\[(?:[^\]]|\]\])+\]|[\w@#$]+))*)";
        let check = r"(?:WITH\s+(?:NO)?CHECK\s+)?";
        [
            (format!(r"(?i)^CREATE\s+TABLE\s+{n}"), "create table"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+{check}ADD\s+CONSTRAINT\s+{n}\s+PRIMARY\s+KEY"), "add primary key"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+{check}ADD\s+CONSTRAINT\s+{n}\s+FOREIGN\s+KEY"), "add foreign key"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+{check}ADD\s+CONSTRAINT\s+{n}\s+UNIQUE"), "add unique constraint"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+{check}ADD\s+CONSTRAINT\s+{n}\s+CHECK"), "add check constraint"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+ADD\s+CONSTRAINT\s+{n}\s+DEFAULT"), "add default"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+DROP\s+CONSTRAINT\s+{n}"), "drop constraint"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+ALTER\s+COLUMN\s+{n}"), "alter column"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+DROP\s+COLUMN\s+{n}"), "drop column"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+(?:NO)?CHECK\s+CONSTRAINT"), "check constraints"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+(?:ENABLE|DISABLE)\s+TRIGGER"), "enable or disable triggers"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}\s+ADD\b"), "add columns"),
            (format!(r"(?i)^ALTER\s+TABLE\s+{n}"), "alter table"),
            (format!(r"(?i)^CREATE\s+(?:UNIQUE\s+)?(?:CLUSTERED\s+|NONCLUSTERED\s+)?(?:COLUMNSTORE\s+|XML\s+|SPATIAL\s+|PRIMARY\s+XML\s+)?INDEX\s+{n}\s+ON\s+{n}"), "create index"),
            (format!(r"(?i)^DROP\s+INDEX\s+(?:IF\s+EXISTS\s+)?{n}\s+ON\s+{n}"), "drop index"),
            (format!(r"(?i)^ALTER\s+INDEX\s+{n}\s+ON\s+{n}"), "alter index"),
            (format!(r"(?i)^(?:CREATE\s+OR\s+ALTER|CREATE)\s+({KINDS})\s+{n}"), "create"),
            (format!(r"(?i)^ALTER\s+({KINDS}|AUTHORIZATION\s+ON)\s+{n}"), "alter"),
            (format!(r"(?i)^DROP\s+(TABLE|{KINDS})\s+(?:IF\s+EXISTS\s+)?{n}"), "drop"),
            (r"(?i)^EXEC(?:UTE)?\s+(?:sys\.)?sp_rename\s+N?'((?:[^']|'')+)'\s*,\s*N?'((?:[^']|'')+)'".to_string(), "rename"),
            (r"(?i)^EXEC(?:UTE)?\s+(?:sys\.)?sp_(?:add|update|drop)extendedproperty\b".to_string(), "extended properties"),
            (r"(?i)^EXEC(?:UTE)?\s+(?:sys\.)?sp_(?:add|drop)rolemember\s+N?'([^']+)'\s*,\s*N?'([^']+)'".to_string(), "role membership"),
            (r"(?i)^(GRANT|DENY|REVOKE)\s+(.*?)\s+ON\s+((?:\w+::)?(?:\[[^\]]+\]|[\w@#$]+)(?:\.(?:\[[^\]]+\]|[\w@#$]+))*)".to_string(), "permissions"),
            (r"(?i)^(INSERT\s+INTO|UPDATE|DELETE\s+FROM)\s+((?:\[[^\]]+\]|[\w@#$]+)(?:\.(?:\[[^\]]+\]|[\w@#$]+))*)".to_string(), "data"),
            (r"(?i)^BACKUP\s+DATABASE\s+(\[[^\]]+\]|\w+)".to_string(), "back up database"),
        ]
        .into_iter()
        .map(|(p, a)| (regex::Regex::new(&p).expect("valid pattern"), a))
        .collect()
    });
    let mut out: Vec<Value> = Vec::new();
    for batch in batches {
        for line in batch.lines() {
            let text = line.trim();
            // Existence checks (IF ...), comments and the script's own plumbing are not actions.
            if text.is_empty() || text.starts_with("--") || text.len() < 4 {
                continue;
            }
            let Some((re, action)) = patterns.iter().find(|(re, _)| re.is_match(text)) else { continue };
            let c = re.captures(text).expect("matched");
            let g = |i: usize| c.get(i).map(|m| m.as_str().to_string()).unwrap_or_default();
            let (action, object, detail) = match *action {
                "create" | "alter" | "drop" => {
                    let kind = g(1).split_whitespace().map(str::to_ascii_lowercase).collect::<Vec<_>>().join(" ");
                    let kind = match kind.as_str() {
                        "proc" => "procedure".to_string(),
                        "fulltext index on" => "full-text index".to_string(),
                        "authorization on" => "owner".to_string(),
                        _ => kind,
                    };
                    (format!("{action} {kind}"), g(2), String::new())
                }
                "create table" if g(1).contains("RG_Recovery_") => ("rebuild table".to_string(), g(1).replace("RG_Recovery_", ""), String::new()),
                "create index" | "drop index" | "alter index" => (action.to_string(), g(2), g(1)),
                "rename" | "role membership" => (action.to_string(), g(1), g(2)),
                "permissions" => (format!("{} permissions", g(1).to_ascii_lowercase()), g(3), g(2)),
                "data" => (format!("{} rows", g(1).split_whitespace().next().unwrap_or_default().to_ascii_lowercase()), g(2), String::new()),
                "extended properties" => (action.to_string(), String::new(), String::new()),
                "back up database" => (action.to_string(), g(1), String::new()),
                _ => (action.to_string(), g(1), g(2)),
            };
            // Several statements of one kind on one object (rows, columns) are one action.
            let same = out.last().is_some_and(|l| l["action"] == action && l["object"] == object && l["detail"] == detail);
            if !same {
                out.push(json!({"action": action, "object": object, "detail": detail}));
            }
        }
    }
    out
}

/// The latest revisions of the folder `path` in git, Mercurial or Subversion.
fn revisions(path: &std::path::Path) -> Result<Vec<Value>, String> {
    use std::process::Command;
    let run = |c: &mut Command| c.output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let row = |id: &str, author: &str, date: &str, message: &str| json!({"id": id, "author": author, "date": date, "message": message});
    let fields = |text: &str, cut: bool| -> Vec<Value> {
        text.lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\u{1f}').collect();
                (f.len() == 4).then(|| row(f[0], f[1], if cut { f[2].get(..16).unwrap_or(f[2]) } else { f[2] }, f[3]))
            })
            .collect()
    };
    if let Some(text) = run(Command::new("git").arg("-C").arg(path).args(["log", "-n", "100", "--format=%h%x1f%an%x1f%ad%x1f%s", "--date=format:%Y-%m-%d %H:%M", "--", "."])) {
        let mut list = fields(&text, false);
        if let Some(tags) = run(Command::new("git").arg("-C").arg(path).args(["tag", "--sort=-creatordate"])) {
            list.extend(tags.lines().take(20).map(|t| row(t, "", "", "tag")));
        }
        return Ok(list);
    }
    if let Some(text) = run(Command::new("hg").arg("--cwd").arg(path).args(["log", "-l", "100", "--template", "{rev}\x1f{author|person}\x1f{date|isodate}\x1f{desc|firstline}\n", "."])) {
        return Ok(fields(&text, true));
    }
    if let Some(xml) = run(Command::new("svn").args(["log", "--xml", "-l", "100"]).arg(path)) {
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        let text = |n: roxmltree::Node, tag: &str| n.children().find(|c| c.has_tag_name(tag)).and_then(|c| c.text()).unwrap_or_default().to_string();
        return Ok(doc
            .descendants()
            .filter(|n| n.has_tag_name("logentry"))
            .map(|n| {
                let date = text(n, "date").replace('T', " ");
                row(n.attribute("revision").unwrap_or_default(), &text(n, "author"), date.get(..16).unwrap_or(&date), text(n, "msg").lines().next().unwrap_or_default())
            })
            .collect());
    }
    Err(format!("{} is not in a git, Mercurial or Subversion working copy", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_and_projects() {
        let mut state = State::default();
        let none = |_: &str| {};
        let o = handle(&mut state, &json!({"op": "options"}), &none).unwrap();
        assert_eq!(o["schema"].as_array().unwrap().len(), schema::options::ALL.len());
        assert_eq!(o["data"].as_array().unwrap().len(), 25);
        let dir = std::env::temp_dir().join(format!("quill-serve-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.qsc").to_string_lossy().to_string();
        let project = serde_json::to_value(Project::new(crate::project::ProjectKind::Schema)).unwrap();
        handle(&mut state, &json!({"op": "project_save", "path": path, "project": project}), &none).unwrap();
        let back = handle(&mut state, &json!({"op": "project_load", "path": path}), &none).unwrap();
        assert_eq!(back["kind"], "schema");
        assert!(handle(&mut state, &json!({"op": "nope"}), &none).is_err());
        assert!(handle(&mut state, &json!({"op": "data_rows", "session": 9}), &none).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    /// Data compare with a scripts folder (needs QUILL_TEST_SERVER, like tests/compare_live.rs).
    #[test]
    fn data_compare_with_a_scripts_folder() {
        let Ok(host) = std::env::var("QUILL_TEST_SERVER") else { return };
        let user = std::env::var("QUILL_TEST_USER").unwrap_or_default();
        let mut side = SourceSpec::database(&host, "QD_sources_Serve");
        side.trust_server_certificate = true;
        if !user.is_empty() {
            side.authentication = crate::project::Authentication::Sql;
            side.user = user;
            side.password = std::env::var("QUILL_TEST_PASSWORD").ok();
        }
        let run = |db: &str, sql: &str| {
            let mut s = side.clone();
            s.database = db.into();
            let spec = s.connection(1).unwrap();
            let rt = crate::db::runtime().unwrap();
            rt.block_on(async {
                let mut c = crate::db::connect_async(&spec).await.unwrap();
                crate::db::execute(&mut c, sql).await.unwrap();
            });
        };
        run("master", "IF DB_ID('QD_sources_Serve') IS NOT NULL BEGIN ALTER DATABASE QD_sources_Serve SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE QD_sources_Serve; END; CREATE DATABASE QD_sources_Serve;");
        run("QD_sources_Serve", "CREATE TABLE dbo.T (a int NOT NULL PRIMARY KEY, b nvarchar(10) NULL); INSERT dbo.T VALUES (1, N'x'), (2, N'y');");
        let dir = std::env::temp_dir().join(format!("quill-serve-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = catalog::read_database(&side.connection(1).unwrap()).unwrap();
        crate::sources::folder::write_all(&db, &dir, &Options::default_set()).unwrap();
        std::fs::create_dir_all(dir.join("Data")).unwrap();
        std::fs::write(dir.join("Data").join("dbo.T_Data.sql"), "INSERT INTO [dbo].[T] ([a], [b]) VALUES (1, N'x')\r\n").unwrap();

        // Projects never carry passwords; the engine takes them from the environment.
        // SAFETY: no other test reads or writes QUILL_PASSWORD1.
        unsafe { std::env::set_var("QUILL_PASSWORD1", side.password.clone().unwrap_or_default()) };
        let mut state = State::default();
        let none = |_: &str| {};
        let mut project = Project::new(crate::project::ProjectKind::Data);
        project.source = side.clone();
        project.target = SourceSpec { kind: "scripts".into(), path: dir.to_string_lossy().to_string(), ..Default::default() };
        let p = serde_json::to_value(&project).unwrap();
        let c = handle(&mut state, &json!({"op": "data_compare", "project": p}), &none).unwrap();
        assert_eq!(c["targetName"], dir.file_name().unwrap().to_string_lossy().to_string(), "{c}");
        let id = c["session"].as_u64().unwrap();
        handle(&mut state, &json!({"op": "data_script", "session": id}), &none).unwrap();
        let d = handle(&mut state, &json!({"op": "deploy", "session": id}), &none).unwrap();
        assert_eq!(d["written"].as_array().unwrap().len(), 1, "{d}");
        let text = std::fs::read_to_string(dir.join("Data").join("dbo.T_Data.sql")).unwrap();
        assert!(text.contains("N'y'"), "{text}");
        handle(&mut state, &json!({"op": "close", "session": id}), &none).unwrap();
        run("master", "ALTER DATABASE QD_sources_Serve SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE QD_sources_Serve;");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn schema_session_ops() {
        use crate::catalog::{Column, DataType, Dependency, Routine, Table};
        let table = |name: &str, cols: &[&str]| Table {
            schema: "dbo".into(),
            name: name.into(),
            columns: cols.iter().map(|c| Column { name: (*c).into(), data_type: DataType { name: "int".into(), ..Default::default() }, ..Default::default() }).collect(),
            ..Default::default()
        };
        let mut db1 = Database { name: "One".into(), ..Default::default() };
        db1.tables = vec![table("Customer", &["Id", "Name"]), table("Orders", &["Id", "Total"])];
        db1.routines.push(Routine { schema: "dbo".into(), name: "Report".into(), definition: "CREATE PROCEDURE dbo.Report AS SELECT Id FROM dbo.Orders".into(), ..Default::default() });
        db1.dependencies.push(Dependency { from: ObjectName::new("dbo", "Report"), to: ObjectName::new("dbo", "Orders"), strong: false });
        let mut db2 = Database { name: "Two".into(), ..Default::default() };
        db2.tables = vec![table("Customers", &["Id", "Name"]), table("Orders", &["Id"])];
        let dir = std::env::temp_dir().join(format!("quill-serve-schema-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = |f: &str| dir.join(f).to_string_lossy().to_string();
        std::fs::write(path("one.json"), serde_json::to_vec(&db1).unwrap()).unwrap();
        std::fs::write(path("two.json"), serde_json::to_vec(&db2).unwrap()).unwrap();
        let mut project = Project::new(crate::project::ProjectKind::Schema);
        project.source = SourceSpec { kind: "snapshot".into(), path: path("one.json"), ..Default::default() };
        project.target = SourceSpec { kind: "snapshot".into(), path: path("two.json"), ..Default::default() };
        let project = serde_json::to_value(&project).unwrap();
        let mut state = State::default();
        let none = |_: &str| {};
        let r = handle(&mut state, &json!({"op": "schema_compare", "project": project}), &none).unwrap();
        let s = r["session"].clone();
        // No rename suggestions: SQL Compare's UI makes none; tables are mapped by hand.
        assert!(r.get("renames").is_none());
        let index = |r: &Value, name: &str| r["differences"].as_array().unwrap().iter().find(|d| d["name"] == name).unwrap()["index"].as_u64().unwrap();
        let (report, orders) = (index(&r, "Report"), index(&r, "Orders"));
        let deps = handle(&mut state, &json!({"op": "schema_dependencies", "session": s, "selected": [report]}), &none).unwrap();
        assert_eq!(deps, json!([{"index": orders, "referencedBy": ["[dbo].[Report]"]}]));
        let html = path("r.html");
        handle(&mut state, &json!({"op": "schema_report", "session": s, "path": html, "format": "Html", "selected": [orders]}), &none).unwrap();
        let text = std::fs::read_to_string(&html).unwrap();
        assert!(text.contains("Orders") && !text.contains("Report</"), "only the chosen object is reported");
        let switched = handle(&mut state, &json!({"op": "schema_switch", "session": s}), &none).unwrap();
        assert_eq!((switched["sourceName"].as_str(), switched["targetName"].as_str()), (Some("Two"), Some("One")));
        let report_now = switched["differences"].as_array().unwrap().iter().find(|d| d["name"] == "Report").unwrap();
        assert_eq!(report_now["kind"], "OnlyIn2");
        assert_eq!(switched["project"]["source"]["path"], path("two.json"));
        let qsc = path("p.qsc");
        handle(&mut state, &json!({"op": "project_save", "path": qsc, "project": project}), &none).unwrap();
        let back = handle(&mut state, &json!({"op": "project_load", "path": qsc}), &none).unwrap();
        assert_eq!(back["source"]["path"], path("one.json"));

        // A filter leaving out tables named like Cust%: the switched session, then a new comparison.
        let rules = json!({"rules": [{"type": "Table", "include": false, "expression": "@NAME LIKE 'Cust%'"}]});
        let f = handle(&mut state, &json!({"op": "schema_filter", "session": s, "filter": rules}), &none).unwrap();
        assert_eq!(f["filtered"].as_array().unwrap().len(), 2, "{f}");
        assert!(handle(&mut state, &json!({"op": "schema_filter", "session": s, "filter": {"rules": [{"type": "Table", "include": true, "expression": "@NAME ="}]}}), &none).is_err());
        let scpf = path("f.scpf");
        handle(&mut state, &json!({"op": "filter_save", "path": scpf, "filter": rules}), &none).unwrap();
        let loaded = handle(&mut state, &json!({"op": "filter_load", "path": scpf}), &none).unwrap();
        assert!(loaded["filter"]["rules"].as_array().unwrap().iter().any(|r| r["type"] == "Table" && r["include"] == false));
        let mut filtered_project = project.clone();
        filtered_project["filter"] = f["xml"].clone();
        let r = handle(&mut state, &json!({"op": "schema_compare", "project": filtered_project}), &none).unwrap();
        let customer = r["differences"].as_array().unwrap().iter().find(|d| d["name"] == "Customer").unwrap();
        assert_eq!(customer["selected"], false);
        assert!(r["filtered"].as_array().unwrap().contains(&customer["index"]));
        handle(&mut state, &json!({"op": "project_save", "path": qsc, "project": filtered_project}), &none).unwrap();
        let back = handle(&mut state, &json!({"op": "project_load", "path": qsc}), &none).unwrap();
        assert!(back["filter"].as_str().unwrap().contains("Cust%"));

        // Mapping tabs: the default pairs, then a user mapping and an unmapped default pair.
        let m = handle(&mut state, &json!({"op": "mapping_preview", "project": project}), &none).unwrap();
        assert_eq!(m["tables"]["mapped"].as_array().unwrap().len(), 1, "{m}");
        assert_eq!(m["tables"]["mapped"][0]["user"], false);
        assert_eq!(m["tables"]["only1"][0]["name"], "Customer");
        let mut remapped = project.clone();
        remapped["schemaMappings"] = json!({"tables": [
            {"source": {"schema": "dbo", "name": "Customer"}, "target": {"schema": "dbo", "name": "Customers"}},
            {"source": {"schema": "dbo", "name": "Orders"}, "target": {"schema": "", "name": ""}},
        ]});
        let m = handle(&mut state, &json!({"op": "mapping_preview", "project": remapped}), &none).unwrap();
        assert_eq!(m["tables"]["mapped"][0]["target"]["name"], "Customers");
        assert_eq!(m["tables"]["mapped"][0]["user"], true);
        assert_eq!((m["tables"]["only1"][0]["name"].as_str(), m["tables"]["only2"][0]["name"].as_str()), (Some("Orders"), Some("Orders")));
        let r = handle(&mut state, &json!({"op": "schema_compare", "project": remapped}), &none).unwrap();
        let kinds: Vec<&str> = r["differences"].as_array().unwrap().iter().filter(|d| d["name"] == "Orders").map(|d| d["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["OnlyIn1", "OnlyIn2"]);
        handle(&mut state, &json!({"op": "project_save", "path": qsc, "project": remapped}), &none).unwrap();
        let back = handle(&mut state, &json!({"op": "project_load", "path": qsc}), &none).unwrap();
        let tables = back["schemaMappings"]["tables"].as_array().unwrap();
        assert!(tables.iter().any(|t| t["source"]["name"] == "Customer" && t["target"]["name"] == "Customers"), "{back}");
        assert!(tables.iter().any(|t| t["source"]["name"] == "Orders" && t["target"]["name"] == ""), "{back}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compares_and_deploys_scripts_folders() {
        let mut state = State::default();
        let none = |_: &str| {};
        let base = std::env::temp_dir().join(format!("quill-serve-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let write = |dir: &str, file: &str, text: &str| {
            let p = base.join(dir).join(file);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("a", "Tables/dbo.T.sql", "CREATE TABLE [dbo].[T]\r\n(\r\n[a] [int] NULL,\r\n[b] [int] NULL\r\n)\r\nGO\r\n");
        write("a", "Tables/dbo.U.sql", "CREATE TABLE [dbo].[U]\r\n(\r\n[a] [int] NULL\r\n)\r\nGO\r\n");
        write("b", "Tables/dbo.T.sql", "CREATE TABLE [dbo].[T]\r\n(\r\n[a] [int] NULL\r\n)\r\nGO\r\nPRINT 1\r\nGO\r\n");
        let side = |d: &str| SourceSpec { kind: "scripts".into(), path: base.join(d).to_string_lossy().to_string(), ..Default::default() };
        let mut project = Project::new(crate::project::ProjectKind::Schema);
        project.source = side("a");
        project.target = side("b");
        let p = serde_json::to_value(&project).unwrap();
        let c = handle(&mut state, &json!({"op": "schema_compare", "project": p}), &none).unwrap();
        assert_eq!(c["differences"].as_array().unwrap().len(), 2);
        assert!(c["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("Ignored statement")));
        let id = c["session"].as_u64().unwrap();
        handle(&mut state, &json!({"op": "schema_script", "session": id, "selected": [0, 1]}), &none).unwrap();
        let d = handle(&mut state, &json!({"op": "deploy", "session": id}), &none).unwrap();
        assert_eq!(d["written"].as_array().unwrap().len(), 2, "{d}");
        assert!(base.join("b").join("Tables").join("dbo.U.sql").is_file());
        let again = handle(&mut state, &json!({"op": "schema_compare", "project": p}), &none).unwrap();
        assert!(again["differences"].as_array().unwrap().iter().all(|x| x["kind"] == "Equal"), "{again}");
        let sql = handle(&mut state, &json!({"op": "schema_object_sql", "session": again["session"], "index": 1}), &none).unwrap();
        assert!(sql["source"].as_str().unwrap().contains("[dbo].[U]"), "{sql}");

        // make_scripts from a project's source and from a plain source.
        let made = base.join("made").to_string_lossy().to_string();
        let r = handle(&mut state, &json!({"op": "make_scripts", "project": p, "path": made}), &none).unwrap();
        assert_eq!(r["files"], 2);
        assert!(base.join("made").join("QuillDatabaseInfo.xml").is_file());
        let made2 = base.join("made2").to_string_lossy().to_string();
        let src = serde_json::to_value(side("b")).unwrap();
        handle(&mut state, &json!({"op": "make_scripts", "source": src, "path": made2}), &none).unwrap();
        assert!(base.join("made2").join("Tables").join("dbo.U.sql").is_file());

        // A revision of a folder in git.
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&base).args(args).output().is_ok_and(|o| o.status.success());
        if git(&["init", "-q"]) {
            git(&["config", "user.email", "t@example.com"]);
            git(&["config", "user.name", "t"]);
            git(&["add", "a"]);
            git(&["commit", "-q", "-m", "a"]);
            write("a", "Tables/dbo.V.sql", "CREATE TABLE [dbo].[V]\r\n(\r\n[a] [int] NULL\r\n)\r\nGO\r\n");
            let mut at_head = project.clone();
            at_head.source.revision = "HEAD".into();
            at_head.target = side("a");
            let c = handle(&mut state, &json!({"op": "schema_compare", "project": at_head}), &none).unwrap();
            let only2: Vec<&Value> = c["differences"].as_array().unwrap().iter().filter(|x| x["kind"] == "OnlyIn2").collect();
            assert_eq!(only2.len(), 1, "{c}");
            assert_eq!(c["sourceName"], "a");
            let mut to_revision = at_head.clone();
            std::mem::swap(&mut to_revision.source, &mut to_revision.target);
            let c = handle(&mut state, &json!({"op": "schema_compare", "project": to_revision}), &none).unwrap();
            let id = c["session"].as_u64().unwrap();
            handle(&mut state, &json!({"op": "schema_script", "session": id, "selected": [0, 1, 2]}), &none).unwrap();
            assert!(handle(&mut state, &json!({"op": "deploy", "session": id}), &none).is_err());
        }
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn every_shown_option_has_a_description() {
        let o = option_definitions();
        for d in o["schema"].as_array().unwrap().iter().filter(|d| d["group"] != "Hidden").chain(o["data"].as_array().unwrap()) {
            assert!(!d["description"].as_str().unwrap().is_empty(), "{d}");
        }
    }

    #[test]
    fn script_actions_in_order() {
        let batches = [
            "SET NUMERIC_ROUNDABORT OFF".to_string(),
            "PRINT N'Dropping foreign keys from [dbo].[Orders]'".to_string(),
            "ALTER TABLE [dbo].[Orders] DROP CONSTRAINT [FK_Orders_Customer]".to_string(),
            "IF @@ERROR <> 0 SET NOEXEC ON".to_string(),
            "CREATE TABLE [dbo].[RG_Recovery_1_Customer]\n(\n[Id] [int] NOT NULL\n)".to_string(),
            "INSERT INTO [dbo].[RG_Recovery_1_Customer]([Id]) SELECT [Id] FROM [dbo].[Customer]".to_string(),
            "EXEC sp_rename N'[dbo].[RG_Recovery_1_Customer]', N'Customer', N'OBJECT'".to_string(),
            "CREATE NONCLUSTERED INDEX [IX_A] ON [dbo].[Customer] ([Id])".to_string(),
            "ALTER TABLE [dbo].[Orders] WITH NOCHECK ADD CONSTRAINT [FK_Orders_Customer] FOREIGN KEY ([C]) REFERENCES [dbo].[Customer] ([Id])".to_string(),
            "CREATE PROCEDURE [dbo].[Report] AS SELECT 1".to_string(),
            "ALTER VIEW dbo.V AS SELECT 1 AS a".to_string(),
            "IF OBJECT_ID(N'[dbo].[Old]', 'P') IS NOT NULL\nDROP PROCEDURE [dbo].[Old]".to_string(),
            "GRANT EXECUTE ON  [dbo].[Report] TO [app]".to_string(),
        ];
        let actions: Vec<(String, String)> = script_actions(&batches).iter().map(|a| (a["action"].as_str().unwrap().to_string(), a["object"].as_str().unwrap().to_string())).collect();
        let expected = [
            ("drop constraint", "[dbo].[Orders]"),
            ("rebuild table", "[dbo].[1_Customer]"),
            ("insert rows", "[dbo].[RG_Recovery_1_Customer]"),
            ("rename", "[dbo].[RG_Recovery_1_Customer]"),
            ("create index", "[dbo].[Customer]"),
            ("add foreign key", "[dbo].[Orders]"),
            ("create procedure", "[dbo].[Report]"),
            ("alter view", "dbo.V"),
            ("drop procedure", "[dbo].[Old]"),
            ("grant permissions", "[dbo].[Report]"),
        ];
        assert_eq!(actions, expected.map(|(a, o)| (a.to_string(), o.to_string())));
    }

    #[test]
    fn ui_ops() {
        let mut state = State::default();
        let none = |_: &str| {};
        let f = handle(&mut state, &json!({"op": "format_sql", "source": "select a,b from t", "target": null, "style": "none"}), &none).unwrap();
        assert_eq!(f["source"], "select a,
       b
from t", "{f}");
        assert!(f["target"].is_null());
        assert!(handle(&mut state, &json!({"op": "folder_preview", "session": 99}), &none).is_err());

        let request = json!({"project": {"includeNewTables": false, "selected": [{"type": "Table", "schema": "dbo", "name": "A"}]}});
        assert_eq!(only_selected(&request), Some(vec![ObjectName::new("dbo", "A")]));
        assert_eq!(only_selected(&json!({"project": {"selected": []}})), None);

        // A folder without database information read as SQL Server 2019 with a chosen collation.
        let base = std::env::temp_dir().join(format!("quill-serve-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("a").join("Tables")).unwrap();
        std::fs::write(base.join("a").join("Tables").join("dbo.T.sql"), "CREATE TABLE [dbo].[T]\r\n(\r\n[a] [int] NULL\r\n)\r\nGO\r\n").unwrap();
        let path = base.join("a").to_string_lossy().to_string();
        let mut db = Database::default();
        folder_defaults(&json!({"project": {"source": {"type": "scripts", "path": path, "sqlVersion": "15", "defaultCollation": "Latin1_General_BIN"}}}), "source", &mut db);
        assert_eq!((db.server.major_version, db.collation.as_str()), (15, "Latin1_General_BIN"));

        // Deploying to a scripts folder: the files are listed first, and only then written.
        std::fs::create_dir_all(base.join("b")).unwrap();
        let mut project = Project::new(crate::project::ProjectKind::Schema);
        project.source = SourceSpec { kind: "scripts".into(), path, ..Default::default() };
        project.target = SourceSpec { kind: "scripts".into(), path: base.join("b").to_string_lossy().to_string(), ..Default::default() };
        let c = handle(&mut state, &json!({"op": "schema_compare", "project": project}), &none).unwrap();
        let id = c["session"].as_u64().unwrap();
        assert!(c["differences"][0].get("modified1").is_some());
        let script = handle(&mut state, &json!({"op": "schema_script", "session": id, "selected": [0]}), &none).unwrap();
        assert_eq!(script["actions"][0]["action"], "create table", "{script}");
        let p = handle(&mut state, &json!({"op": "folder_preview", "session": id}), &none).unwrap();
        assert_eq!(p["written"].as_array().unwrap().len(), 1, "{p}");
        assert!(!base.join("b").join("Tables").exists(), "a preview writes nothing");

        // Revisions of a folder in git.
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&base).args(args).output().is_ok_and(|o| o.status.success());
        if git(&["init", "-q"]) {
            git(&["config", "user.email", "t@example.com"]);
            git(&["config", "user.name", "t"]);
            git(&["add", "a"]);
            git(&["commit", "-q", "-m", "first"]);
            git(&["tag", "v1"]);
            let r = handle(&mut state, &json!({"op": "revisions", "path": base.join("a")}), &none).unwrap();
            assert_eq!(r[0]["message"], "first", "{r}");
            assert!(r.as_array().unwrap().iter().any(|x| x["id"] == "v1"));
        }
        std::fs::remove_dir_all(base).ok();
    }
}
