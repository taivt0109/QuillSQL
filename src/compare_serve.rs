//! `quill compare-serve`: the engine behind the Compare windows (SSMS and QuillCompare.exe).
//! One JSON request per stdin line, `{"id":n,"op":"...",...}`; one reply per stdout line,
//! `{"id":n,"result":...}` or `{"id":n,"error":"..."}`. Long operations first send
//! `{"id":n,"progress":"..."}` lines. Comparison results stay here between requests.
//!
//! - `options` describes every schema and data option (for the Options tabs).
//! - `databases` {source} lists databases; `test_connection` {source} returns the server version.
//! - `project_load` {path} / `project_save` {path, project}.
//! - `schema_compare` {project} → session with differences; `schema_object_sql` {session, index};
//!   `schema_script` {session, selected:[index], project}; `deploy` {session} runs the last script.
//! - `data_tables` {project} previews table pairs and keys; `data_compare` {project} → session;
//!   `data_rows` {session, table, kind, start, count}; `data_script` {session, kinds, tables,
//!   selections, isolationLevel, maxTransactionSizeMB} (see `selections`);
//!   `data_session_save` {session, path} / `data_session_load` {path} → session.
//! - `snapshot` {source, path} writes a schema snapshot.
//!
//! A side is a live database, a snapshot (QuillSQL JSON or SQL Compare `.snp`), or a scripts
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
}

#[derive(Default)]
struct State {
    next: u64,
    schema: HashMap<u64, SchemaSession>,
    data: HashMap<u64, DataSession>,
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

/// `decrypt` follows DecryptPost2kEncryptedObjects; data compare never needs module text.
fn load_side(side: &SourceSpec, n: u8, decrypt: bool, progress: &dyn Fn(&str)) -> Result<Database, String> {
    load_side_with_warnings(side, n, decrypt, progress).map(|(db, _)| db)
}

/// A side and, for a scripts folder, what its parser skipped.
fn load_side_with_warnings(side: &SourceSpec, n: u8, decrypt: bool, progress: &dyn Fn(&str)) -> Result<(Database, Vec<String>), String> {
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

fn handle(state: &mut State, request: &Value, progress: &dyn Fn(&str)) -> Result<Value, String> {
    let op = request.get("op").and_then(Value::as_str).unwrap_or_default();
    match op {
        "options" => Ok(option_definitions()),
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
            std::fs::write(&path, serde_json::to_vec_pretty(&db).map_err(|e| e.to_string())?).map_err(|e| format!("{path}: {e}"))?;
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
            let opts = schema_options(&project)?;
            let decrypt = opts.has(Opt::DecryptPost2kEncryptedObjects);
            let (db1, mut warnings) = load_side_with_warnings(&project.source, 1, decrypt, progress)?;
            let (db2, w2) = load_side_with_warnings(&project.target, 2, decrypt, progress)?;
            warnings.extend(w2);
            progress("Comparing");
            let mut comparison = schema::compare(&db1, &db2, &opts, &project.schema_mappings);
            for d in &mut comparison.differences {
                if project.deselected.iter().any(|x| x.schema == d.name.schema && x.name == d.name.name && (x.kind.is_empty() || x.kind == d.object_type.label())) {
                    d.selected = false;
                }
            }
            let differences: Vec<Value> = comparison.differences.iter().enumerate().map(|(i, d)| difference_json(i, d)).collect();
            let auto: Vec<Value> = comparison
                .auto_mapped_columns
                .iter()
                .map(|(t, a, b)| json!({"table": t.to_string(), "source": a, "target": b}))
                .collect();
            state.next += 1;
            let id = state.next;
            let reply = json!({
                "session": id,
                "sourceName": db1.name,
                "targetName": db2.name,
                "differences": differences,
                "autoMapped": auto,
                "warnings": warnings,
            });
            state.schema.insert(id, SchemaSession { db1, db2, comparison, project, script: None, selected: Vec::new() });
            Ok(reply)
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
            let reply = script_json(&script);
            s.script = Some(script);
            s.selected = selected;
            Ok(reply)
        }
        "data_tables" => {
            let project: Project = field(request, "project")?;
            let mut opts = data_options(&project)?;
            // Views and checksums need both sides live.
            if project.source.is_snapshot() || project.target.is_snapshot() {
                opts = opts.without(DataOpt::IncludeViews).without(DataOpt::UseChecksumComparison);
            }
            let db1 = load_side(&project.source, 1, false, progress)?;
            let db2 = load_side(&project.target, 2, false, progress)?;
            let (pairs, only1, only2) = crate::data::mapping::map_tables(&db1, &db2, &opts, &project.data_mappings);
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
                        "notes": notes_json(p),
                    })
                })
                .collect();
            let all_columns: HashMap<String, Vec<String>> =
                db1.tables.iter().map(|t| (t.object_name().to_string(), t.columns.iter().map(|c| c.name.clone()).collect())).collect();
            let target_columns: HashMap<String, Vec<String>> =
                db2.tables.iter().map(|t| (t.object_name().to_string(), t.columns.iter().map(|c| c.name.clone()).collect())).collect();
            Ok(json!({
                "tables": tables,
                "onlyIn1": only1.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "onlyIn2": only2.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "sourceColumns": all_columns,
                "targetColumns": target_columns,
                "sourceSchemas": db1.schemas.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "targetSchemas": db2.schemas.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            }))
        }
        "data_compare" => {
            let project: Project = field(request, "project")?;
            if project.source.is_scripts() || project.target.is_scripts() {
                return Err("data compare needs live databases, not scripts folders".into());
            }
            let opts = data_options(&project)?;
            let (s1, s2) = (project.source.connection(1)?, project.target.connection(2)?);
            progress(&format!("Reading {}", project.source.describe()));
            let db1 = catalog::read_database_with(&s1, false)?;
            progress(&format!("Reading {}", project.target.describe()));
            let db2 = catalog::read_database_with(&s2, false)?;
            progress("Comparing data");
            let deselected: Vec<ObjectName> = project.deselected.iter().map(|d| ObjectName::new(&d.schema, &d.name)).collect();
            let result = crate::data::compare::compare(
                &s1,
                &s2,
                &db1,
                &db2,
                &opts,
                &project.data_mappings,
                |p| !deselected.contains(&p.source),
                4,
                |p, _| progress(&progress_text(p, opts.has(DataOpt::UseChecksumComparison))),
            )?;
            state.next += 1;
            let id = state.next;
            let reply = data_session_json(id, &db1.name, &db2.name, &result);
            state.data.insert(id, DataSession { source_name: db1.name, db2, result, project, script: None });
            Ok(reply)
        }
        "data_rows" => {
            let s = state.data.get(&session(request)?).ok_or("the comparison is no longer open")?;
            let table: usize = field(request, "table")?;
            let kind: usize = field(request, "kind")?;
            let start: usize = field(request, "start")?;
            let count: usize = field(request, "count")?;
            let t = s.result.tables.get(table).ok_or("no such table")?;
            let k = RowKind::ALL.get(kind).copied().ok_or("no such row kind")?;
            let offsets = &t.offsets[k as usize];
            let rows = store::read_page(&t.path, offsets, start, count).map_err(|e| e.to_string())?;
            let rows: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "source": r.source.iter().map(|v| v.display()).collect::<Vec<_>>(),
                        "target": r.target.iter().map(|v| v.display()).collect::<Vec<_>>(),
                        "differs": r.differs,
                    })
                })
                .collect();
            Ok(json!({"rows": rows, "total": offsets.len(), "counted": t.counts[k as usize]}))
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
            settings.tables = tables.map(|ix| ix.iter().filter_map(|&i| s.result.tables.get(i)).map(|t| t.pair.source.clone()).collect());
            settings.selections = selections(request, &s.result)?;
            settings.max_transaction_bytes = request.get("maxTransactionSizeMB").and_then(Value::as_u64).filter(|&m| m > 0).map(|m| m * 1_048_576);
            settings.source_connection = s.project.source.connection(1).ok();
            let script = crate::data::sync::generate(&s.result, &s.db2, &opts, &settings)?;
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
            state.data.insert(id, DataSession { source_name, db2, result, project, script: None });
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
            let spec = target.connection(2)?;
            let ran = deploy::execute(&spec, &script, |i, n| progress(&format!("Running batch {} of {n}", i + 1)))?;
            Ok(json!({"batches": ran}))
        }
        "close" => {
            let id = session(request)?;
            state.schema.remove(&id);
            state.data.remove(&id);
            Ok(json!(true))
        }
        other => Err(format!("unknown op '{other}'")),
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
            })
        })
        .collect();
    let data: Vec<Value> = crate::data::options::ALL
        .iter()
        .map(|o| json!({"name": o.name(), "label": o.label(), "group": format!("{:?}", o.group()), "default": o.is_default()}))
        .collect();
    let _ = (Opt::IgnoreWhiteSpace, DataOpt::IgnoreSpaces);
    json!({"schema": schema, "data": data})
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
}
