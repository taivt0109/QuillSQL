//! The data deployment script: makes the rows of database 2 match database 1.
//!
//! Inside one transaction: foreign keys (and, if asked, triggers) of the touched tables are
//! disabled, rows are deleted and updated from child tables up to parents, inserted from
//! parents down to children, then the constraints are enabled and checked again.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use super::compare::{DataComparison, TableResult};
use super::options::{DataOpt, DataOptions};
use super::store::{self, RowKind, StoredRow};
use super::value::Value;
use crate::catalog::{DataType, Database, ObjectName};
use crate::db::{literal, quote};
use crate::schema::deploy::{DeploymentScript, Severity, Warning};

pub struct SyncSettings {
    pub source: String,
    pub target: String,
    pub target_database: String,
    pub transaction_isolation_level: String,
    /// Row kinds to deploy: Different → UPDATE, OnlyIn1 → INSERT, OnlyIn2 → DELETE.
    pub kinds: [bool; 4],
    /// Tables (source names) to deploy; `None` = every compared table.
    pub tables: Option<Vec<ObjectName>>,
}

/// Statements per batch; the transaction spans batches.
const BATCH: usize = 1000;
const PAGE: usize = 10_000;

pub fn generate(result: &DataComparison, target: &Database, opts: &DataOptions, settings: &SyncSettings) -> Result<DeploymentScript, String> {
    let mut w = Writer::default();
    let mut warnings = Vec::new();
    let transactions = !opts.has(DataOpt::DontUseTransactions);
    let comments = !opts.has(DataOpt::DontOutputComments);

    let tables: Vec<&TableResult> = result
        .tables
        .iter()
        .filter(|t| t.error.is_none() && !t.pair.is_view)
        .filter(|t| settings.tables.as_ref().is_none_or(|only| only.contains(&t.pair.source)))
        .filter(|t| {
            (settings.kinds[RowKind::Different as usize] && t.count(RowKind::Different) > 0)
                || (settings.kinds[RowKind::OnlyIn1 as usize] && t.count(RowKind::OnlyIn1) > 0)
                || (settings.kinds[RowKind::OnlyIn2 as usize] && t.count(RowKind::OnlyIn2) > 0)
        })
        .collect();
    for t in result.tables.iter().filter(|t| t.error.is_some()) {
        warnings.push(Warning {
            severity: Severity::High,
            object: t.pair.source.to_string(),
            message: format!("not compared, so not deployed: {}", t.error.as_deref().unwrap_or_default()),
        });
    }
    for t in &tables {
        if t.by_checksum {
            continue;
        }
        if t.count(RowKind::OnlyIn2) > 0 && settings.kinds[RowKind::OnlyIn2 as usize] {
            warnings.push(Warning {
                severity: Severity::High,
                object: t.pair.target.to_string(),
                message: format!("{} row(s) are deleted", t.count(RowKind::OnlyIn2)),
            });
        }
    }
    if opts.has(DataOpt::DropConstraintsAndIndexes) {
        warnings.push(Warning { severity: Severity::Medium, object: String::new(), message: "dropping indexes and keys during data deployment is not supported yet; they are kept".into() });
    }
    if tables.is_empty() {
        return Ok(DeploymentScript { sql: String::new(), warnings, batches: Vec::new() });
    }

    // Parents before children for inserts; the reverse for deletes and updates.
    let order = dependency_order(&tables, target);

    if opts.has(DataOpt::OutputCommentHeader) {
        w.raw(&format!(
            "/*\r\nRun this script on:\r\n\r\n{}    -  This database will be modified\r\n\r\nto synchronize it with:\r\n\r\n{}\r\n\r\nYou are recommended to back up your database before running this script\r\n\r\nScript created by QuillSQL {}\r\n\r\n*/\r\n",
            settings.target,
            settings.source,
            env!("CARGO_PKG_VERSION")
        ));
    }
    w.batch("SET NUMERIC_ROUNDABORT OFF");
    w.batch("SET ANSI_PADDING, ANSI_WARNINGS, CONCAT_NULL_YIELDS_NULL, ARITHABORT, QUOTED_IDENTIFIER, ANSI_NULLS, NOCOUNT ON");
    w.batch("SET DATEFORMAT YMD");
    if opts.has(DataOpt::AddDatabaseUseStatement) && !settings.target_database.is_empty() {
        w.batch(&format!("USE {}", quote(&settings.target_database)));
    }
    if transactions {
        w.batch("SET XACT_ABORT ON");
        w.batch(&format!("SET TRANSACTION ISOLATION LEVEL {}", settings.transaction_isolation_level));
        w.statement("BEGIN TRANSACTION");
    }
    let print = |w: &mut Writer, text: &str| {
        if comments {
            w.statement(&format!("PRINT({})", literal(text)));
        }
    };

    // Foreign keys touching the changed tables, and their triggers.
    let changed: HashSet<ObjectName> = tables.iter().map(|t| t.pair.target.clone()).collect();
    let mut fks: Vec<(ObjectName, String)> = Vec::new();
    if opts.has(DataOpt::DisableKeys) {
        for t in &target.tables {
            for fk in t.foreign_keys.iter().filter(|fk| !fk.disabled) {
                if changed.contains(&t.object_name()) || changed.contains(&fk.referenced_table) {
                    fks.push((t.object_name(), fk.name.clone()));
                }
            }
        }
    }
    let mut triggers: Vec<(ObjectName, String)> = Vec::new();
    if opts.has(DataOpt::DisableTriggers) {
        for t in target.tables.iter().filter(|t| changed.contains(&t.object_name())) {
            for tr in t.triggers.iter().filter(|tr| !tr.disabled) {
                triggers.push((t.object_name(), tr.name.clone()));
            }
        }
    }
    for (table, fk) in &fks {
        print(&mut w, &format!("Drop constraints from {table}"));
        w.statement(&format!("ALTER TABLE {} NOCHECK CONSTRAINT {}", table.quoted(), quote(fk)));
    }
    for (table, tr) in &triggers {
        w.statement(&format!("DISABLE TRIGGER {} ON {}", crate::schema::ddl::qname(&table.schema, tr), table.quoted()));
    }

    let identity_tables: HashMap<ObjectName, bool> = target.tables.iter().map(|t| (t.object_name(), t.columns.iter().any(|c| c.identity.is_some()))).collect();

    // Deletes and updates, children first.
    for &i in order.iter().rev() {
        let t = tables[i];
        let p = &t.pair;
        let name = p.target.quoted();
        if settings.kinds[RowKind::OnlyIn2 as usize] && t.count(RowKind::OnlyIn2) > 0 {
            print(&mut w, &format!("Delete {} row(s) from {}", t.count(RowKind::OnlyIn2), p.target));
            for_each_row(t, RowKind::OnlyIn2, |row| {
                let (where_, single) = key_where(t, &row.target);
                if single {
                    w.statement("SET ROWCOUNT 1");
                }
                w.statement(&format!("DELETE FROM {name} WHERE {where_}"));
                if single {
                    w.statement("SET ROWCOUNT 0");
                }
            })?;
        }
        if settings.kinds[RowKind::Different as usize] && t.count(RowKind::Different) > 0 {
            print(&mut w, &format!("Update {} row(s) in {}", t.count(RowKind::Different), p.target));
            let identity_cols: Vec<usize> = (0..p.columns.len()).filter(|&c| p.columns[c].identity).collect();
            let mut reinserted = Vec::new();
            for_each_row(t, RowKind::Different, |row| {
                // An identity value cannot be updated: the row is deleted and inserted again.
                if identity_cols.iter().any(|&c| row.differs.get(c).copied().unwrap_or(false)) {
                    reinserted.push(row.clone());
                    return;
                }
                let sets: Vec<String> = (0..p.columns.len())
                    .filter(|&c| row.differs.get(c).copied().unwrap_or(false) && !p.columns[c].read_only && !p.key.contains(&c))
                    .map(|c| format!("{}={}", quote(&p.columns[c].target), value_sql(&row.source[c], &p.columns[c].target_type, &p.columns[c].source_type)))
                    .collect();
                if sets.is_empty() {
                    return;
                }
                let (where_, single) = key_where(t, &row.target);
                if single {
                    w.statement("SET ROWCOUNT 1");
                }
                w.statement(&format!("UPDATE {name} SET {} WHERE {where_}", sets.join(", ")));
                if single {
                    w.statement("SET ROWCOUNT 0");
                }
            })?;
            for row in &reinserted {
                let (where_, _) = key_where(t, &row.target);
                w.statement(&format!("DELETE FROM {name} WHERE {where_}"));
            }
            if !reinserted.is_empty() {
                insert_rows(&mut w, t, reinserted.iter(), true);
            }
        }
    }
    // Inserts, parents first.
    for &i in &order {
        let t = tables[i];
        let p = &t.pair;
        if settings.kinds[RowKind::OnlyIn1 as usize] && t.count(RowKind::OnlyIn1) > 0 {
            print(&mut w, &format!("Add {} row(s) to {}", t.count(RowKind::OnlyIn1), p.target));
            let has_identity = identity_tables.get(&p.target).copied().unwrap_or(false);
            let mut rows = Vec::new();
            for_each_row(t, RowKind::OnlyIn1, |row| rows.push(row))?;
            insert_rows(&mut w, t, rows.iter(), has_identity);
        }
    }

    for (table, tr) in &triggers {
        w.statement(&format!("ENABLE TRIGGER {} ON {}", crate::schema::ddl::qname(&table.schema, tr), table.quoted()));
    }
    for (table, fk) in &fks {
        print(&mut w, &format!("Add constraints to {table}"));
        let check = if opts.has(DataOpt::SkipFkChecks) { "" } else { "WITH CHECK " };
        w.statement(&format!("ALTER TABLE {} {check}CHECK CONSTRAINT {}", table.quoted(), quote(fk)));
    }
    if transactions {
        w.statement("COMMIT TRANSACTION");
    }
    w.flush();
    if opts.has(DataOpt::ReseedIdentity) {
        for t in &tables {
            if identity_tables.get(&t.pair.target).copied().unwrap_or(false) {
                w.batch(&format!("DBCC CHECKIDENT({})", literal(&t.pair.target.quoted())));
            }
        }
    }
    if !transactions {
        warnings.push(Warning {
            severity: Severity::High,
            object: String::new(),
            message: "the script does not use a transaction; a failure leaves the data partly changed".into(),
        });
    }
    Ok(DeploymentScript { sql: w.text, warnings, batches: w.batches })
}

fn for_each_row(t: &TableResult, kind: RowKind, mut f: impl FnMut(StoredRow)) -> Result<(), String> {
    let offsets = &t.offsets[kind as usize];
    let mut start = 0;
    while start < offsets.len() {
        for row in store::read_page(&t.path, offsets, start, PAGE).map_err(|e| e.to_string())? {
            f(row);
        }
        start += PAGE;
    }
    Ok(())
}

fn value_sql(v: &Value, target: &DataType, source: &DataType) -> String {
    match v {
        Value::Bytes(_) if target.schema.is_some() || matches!(target.name.as_str(), "hierarchyid" | "geography" | "geometry") => {
            format!("CAST({} AS {})", v.literal("varbinary"), target.sql())
        }
        // N'' follows the source type: Unicode data keeps its characters until the server converts it.
        Value::Str(_) => {
            let unicode = !matches!(source.name.as_str(), "char" | "varchar" | "text") || !matches!(target.name.as_str(), "char" | "varchar" | "text");
            v.literal(if unicode { "nvarchar" } else { "varchar" })
        }
        Value::DateTime(_) if target.name == "date" => v.literal("date"),
        other => other.literal(&target.name),
    }
}

/// `[k1] = v1 AND [k2] = v2`; with repeated keys every comparable column is matched and
/// `true` asks for SET ROWCOUNT 1.
fn key_where(t: &TableResult, values: &[Value]) -> (String, bool) {
    let p = &t.pair;
    let comparable = |c: usize| {
        let ty = &p.columns[c].target_type;
        !(matches!(ty.name.as_str(), "text" | "ntext" | "image" | "xml" | "geography" | "geometry") || ty.schema.is_some() || ty.length == Some(-1))
    };
    let cols: Vec<usize> = if t.repeated_keys { (0..p.columns.len()).filter(|&c| comparable(c)).collect() } else { p.key.clone() };
    let parts: Vec<String> = cols
        .iter()
        .map(|&c| {
            let col = quote(&p.columns[c].target);
            match &values[c] {
                Value::Null => format!("{col} IS NULL"),
                v => format!("{col} = {}", value_sql(v, &p.columns[c].target_type, &p.columns[c].target_type)),
            }
        })
        .collect();
    (parts.join(" AND "), t.repeated_keys)
}

fn insert_rows<'a>(w: &mut Writer, t: &TableResult, rows: impl Iterator<Item = &'a StoredRow>, has_identity: bool) {
    let p = &t.pair;
    let writable: Vec<usize> = (0..p.columns.len()).filter(|&c| !p.columns[c].read_only).collect();
    let identity = has_identity && writable.iter().any(|&c| p.columns[c].identity);
    let name = p.target.quoted();
    let cols = writable.iter().map(|&c| quote(&p.columns[c].target)).collect::<Vec<_>>().join(", ");
    if identity {
        w.statement(&format!("SET IDENTITY_INSERT {name} ON"));
    }
    for row in rows {
        let values: Vec<String> =
            writable.iter().map(|&c| value_sql(&row.source[c], &p.columns[c].target_type, &p.columns[c].source_type)).collect();
        w.statement(&format!("INSERT INTO {name} ({cols}) VALUES ({})", values.join(", ")));
    }
    if identity {
        w.statement(&format!("SET IDENTITY_INSERT {name} OFF"));
    }
}

/// Indexes into `tables`, referenced tables before the tables that reference them.
fn dependency_order(tables: &[&TableResult], target: &Database) -> Vec<usize> {
    let index: HashMap<ObjectName, usize> = tables.iter().enumerate().map(|(i, t)| (t.pair.target.clone(), i)).collect();
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); tables.len()];
    for t in &target.tables {
        let Some(&from) = index.get(&t.object_name()) else { continue };
        for fk in &t.foreign_keys {
            if let Some(&to) = index.get(&fk.referenced_table)
                && to != from
            {
                deps[from].push(to);
            }
        }
    }
    let mut order = Vec::new();
    let mut state = vec![0u8; tables.len()];
    fn visit(i: usize, deps: &[Vec<usize>], state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &d in &deps[i] {
            visit(d, deps, state, order);
        }
        state[i] = 2;
        order.push(i);
    }
    for i in 0..tables.len() {
        visit(i, &deps, &mut state, &mut order);
    }
    order
}

#[derive(Default)]
struct Writer {
    text: String,
    batches: Vec<String>,
    current: String,
    count: usize,
}

impl Writer {
    fn raw(&mut self, s: &str) {
        self.text.push_str(s);
    }

    fn batch(&mut self, sql: &str) {
        self.flush();
        self.text.push_str(sql);
        self.text.push_str("\r\nGO\r\n");
        self.batches.push(sql.to_string());
    }

    fn statement(&mut self, sql: &str) {
        let _ = write!(self.current, "{}\r\n", sql.replace("\r\n", "\n").replace('\n', "\r\n"));
        self.count += 1;
        if self.count >= BATCH {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.current);
        self.text.push_str(&batch);
        self.text.push_str("GO\r\n");
        self.batches.push(batch);
        self.count = 0;
    }
}
