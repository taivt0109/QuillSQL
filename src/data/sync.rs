//! The data deployment script: makes the rows of database 2 match database 1, in the layout
//! SQL Data Compare writes.
//!
//! Inside one transaction (unless split by size): triggers, foreign keys and (if asked)
//! indexes of the deployed tables are disabled or dropped, rows are deleted and updated from
//! child tables up to parents, inserted from parents down to children, then everything is
//! restored. DDL triggers and identity reseeds come after the commit.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::compare::{self, DataComparison, SideKind, TableResult};
use super::mapping::{self, ColumnPair, TablePair};
use super::options::{DataOpt, DataOptions};
use super::store::{self, RowKind, StoredRow};
use super::value::{Literal, Value, sql_string};
use crate::catalog::{Database, ForeignKey, Index, IndexKind, ObjectName, Trigger};
use crate::db::{ConnectionSpec, quote};
use crate::schema::ddl;
use crate::schema::deploy::{DeploymentScript, Severity, Warning};

/// Which stored rows of one kind are deployed: all of them (`included`) or none, with the rows
/// in `toggled` (positions within the kind, as `data_rows` numbers them) the other way round.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RowSelection {
    pub included: bool,
    pub toggled: BTreeSet<u64>,
}

impl Default for RowSelection {
    fn default() -> Self {
        RowSelection { included: true, toggled: BTreeSet::new() }
    }
}

impl RowSelection {
    pub fn selected(&self, row: u64) -> bool {
        self.included != self.toggled.contains(&row)
    }

    pub fn count(&self, total: u64) -> u64 {
        let toggled = self.toggled.iter().filter(|&&r| r < total).count() as u64;
        if self.included { total - toggled } else { toggled }
    }
}

pub struct SyncSettings {
    /// `server.database` of each side, for the header.
    pub source: String,
    pub target: String,
    pub target_database: String,
    pub transaction_isolation_level: String,
    /// Row kinds to deploy: Different → UPDATE, OnlyIn1 → INSERT, OnlyIn2 → DELETE.
    pub kinds: [bool; 4],
    /// Tables (source names) to deploy; `None` = every compared table.
    pub tables: Option<Vec<ObjectName>>,
    /// Row-level choices per table (source name), indexed by `RowKind`.
    pub selections: HashMap<ObjectName, [RowSelection; 4]>,
    /// Commit and start a new transaction after this many bytes of row statements.
    pub max_transaction_bytes: Option<u64>,
    /// Database 1, to read values that were compared by hash.
    pub source_connection: Option<ConnectionSpec>,
}

impl SyncSettings {
    pub fn new(source: &str, target: &str, target_database: &str) -> SyncSettings {
        SyncSettings {
            source: source.into(),
            target: target.into(),
            target_database: target_database.into(),
            transaction_isolation_level: "SERIALIZABLE".into(),
            kinds: [false, true, true, true],
            tables: None,
            selections: HashMap::new(),
            max_transaction_bytes: None,
            source_connection: None,
        }
    }
}

/// The execution blocks Vendor's executor sends without a GO, at most.
const EXEC_CHUNK: usize = 32_000;

/// A table to deploy and what to do with it.
struct Plan<'a> {
    t: &'a TableResult,
    delete: bool,
    update: bool,
    add: bool,
    rows: Option<&'a [RowSelection; 4]>,
}

impl Plan<'_> {
    fn synced(&self) -> bool {
        self.delete || self.update || self.add
    }

    fn selected(&self, kind: RowKind, row: u64) -> bool {
        self.rows.is_none_or(|r| r[kind as usize].selected(row))
    }
}

fn rows_word(n: u64) -> &'static str {
    if n == 1 { "row" } else { "rows" }
}

/// The target table or view's triggers, indexes and foreign keys.
struct Target<'a> {
    triggers: &'a [Trigger],
    indexes: &'a [Index],
    foreign_keys: &'a [ForeignKey],
    is_table: bool,
    memory_optimized: bool,
    edge: bool,
}

fn target_of<'a>(db: &'a Database, name: &ObjectName) -> Target<'a> {
    if let Some(t) = db.table(&name.schema, &name.name) {
        return Target {
            triggers: &t.triggers,
            indexes: &t.indexes,
            foreign_keys: &t.foreign_keys,
            is_table: true,
            memory_optimized: t.memory_optimized,
            edge: t.graph.as_deref() == Some("EDGE"),
        };
    }
    let v = db.views.iter().find(|v| v.schema == name.schema && v.name == name.name);
    Target {
        triggers: v.map(|v| v.triggers.as_slice()).unwrap_or_default(),
        indexes: v.map(|v| v.indexes.as_slice()).unwrap_or_default(),
        foreign_keys: &[],
        is_table: false,
        memory_optimized: false,
        edge: false,
    }
}

pub fn generate(result: &DataComparison, target: &Database, opts: &DataOptions, settings: &SyncSettings) -> Result<DeploymentScript, String> {
    let mut warnings = Vec::new();
    let transactions = !opts.has(DataOpt::DontUseTransactions);
    let mut w = Writer::new(!opts.has(DataOpt::DontOutputComments), settings.max_transaction_bytes, transactions);

    for t in result.tables.iter().filter(|t| t.error.is_some()) {
        warnings.push(Warning {
            severity: Severity::High,
            object: t.pair.source.to_string(),
            message: format!("not compared, so not deployed: {}", t.error.as_deref().unwrap_or_default()),
        });
    }
    // Every compared table takes part in the ordering; only those with changes are deployed.
    let plans: Vec<Plan> = result
        .tables
        .iter()
        .filter(|t| t.error.is_none())
        .map(|t| {
            let chosen = settings.tables.as_ref().is_none_or(|only| only.contains(&t.pair.source)) && !t.identical();
            let rows = settings.selections.get(&t.pair.source);
            let wanted = |k: RowKind| {
                chosen && settings.kinds[k as usize] && t.count(k) > 0 && rows.is_none_or(|r| r[k as usize].count(t.offsets[k as usize].len() as u64) > 0)
            };
            Plan { t, delete: wanted(RowKind::OnlyIn2), update: wanted(RowKind::Different), add: wanted(RowKind::OnlyIn1), rows }
        })
        .collect();
    if !plans.iter().any(Plan::synced) {
        return Ok(DeploymentScript { sql: String::new(), warnings, batches: Vec::new() });
    }
    // Row-level choices change the messages of every table, as in SQL Data Compare.
    let by_row = plans.iter().any(|p| p.rows.is_some_and(|r| r.iter().any(|k| !k.toggled.is_empty() || !k.included)));
    let (order, extra_fks) = dependency_order(&plans, target, opts, &mut warnings);
    if result.kinds[1] == SideKind::Backup {
        warnings.push(Warning {
            severity: Severity::High,
            object: String::new(),
            message: "When the target data source is a backup, the SQL script does not take into account dependencies or foreign keys. If the script references multiple tables you are not recommended to run it against a live database.".into(),
        });
    }
    if result.old_server
        && let Some(first) = result.tables.first()
    {
        warnings.push(Warning {
            severity: Severity::Low,
            object: first.pair.target.to_string(),
            message: "A future version of SQL Compare and SQL Data Compare will no longer support SQL Server 2008 and SQL Server 2008R2.".into(),
        });
    }
    // Masked values read without UNMASK would be deployed as their masks.
    if result.masked_reads {
        for &i in &order {
            let p = &plans[i];
            if p.update || p.add {
                for c in p.t.pair.columns.iter().filter(|c| c.masked) {
                    warnings.push(Warning {
                        severity: Severity::High,
                        object: p.t.pair.target.to_string(),
                        message: format!(
                            "Column {} of table {} has a data mask applied and the current user does not have UNMASK permission. The deployment may contain invalid data.",
                            c.source, p.t.pair.source
                        ),
                    });
                }
            }
        }
    }

    // Header: its wording depends on which side is a scripts folder.
    if opts.has(DataOpt::OutputCommentHeader) && !settings.source.is_empty() && !settings.target.is_empty() {
        let (dst, src) = (&settings.target, &settings.source);
        let made = format!("Script created by QuillSQL {} at {}", env!("CARGO_PKG_VERSION"), now_text());
        let advice = "You are recommended to back up your database before running this script";
        w.sql(&match (result.kinds[0] == SideKind::Folder, result.kinds[1] == SideKind::Folder) {
            (true, true) => format!(
                "/*\r\nRun this script on a database with the same schema as:\r\n\r\n{dst} \u{2013} the database with this schema will be modified\r\n\r\nto synchronize its data with:\r\n\r\n{src} \u{2013} this scripts folder will not be modified\r\n\r\n{advice}\r\n\r\n{made}\r\n\r\n*/\r\n\t\t"
            ),
            (false, true) => format!(
                "/*\r\nRun this script on a database with the same schema as:\r\n\r\n{dst} \u{2013} the database with this schema will be modified\r\n\r\nto synchronize its data with:\r\n\r\n{src} \u{2013} this database will not be modified\r\n\r\n{advice}\r\n\r\n{made}\r\n\r\n*/\r\n\t\t\t"
            ),
            (true, false) => format!(
                "/*\r\nRun this script on:\r\n\r\n{dst} \u{2013} this database will be modified\r\n\r\nto synchronize its data with:\r\n\r\n{src} \u{2013} this scripts folder will not be modified\r\n\r\n{advice}\r\n\r\n{made}\r\n\r\n*/\r\n\t\t"
            ),
            (false, false) => format!("/*\r\nRun this script on:\r\n\r\n{dst}    -  This database will be modified\r\n\r\nto synchronize it with:\r\n\r\n{src}\r\n\r\n{advice}\r\n\r\n{made}\r\n\r\n*/\r\n\t\t"),
        });
    }
    if opts.has(DataOpt::AddDatabaseUseStatement) && !settings.target_database.is_empty() {
        w.sql(&format!("USE {}", quote(&settings.target_database)));
    }
    w.sql("");
    for s in ["SET NUMERIC_ROUNDABORT OFF", "SET ANSI_PADDING, ANSI_WARNINGS, CONCAT_NULL_YIELDS_NULL, ARITHABORT, QUOTED_IDENTIFIER, ANSI_NULLS, NOCOUNT ON", "SET DATEFORMAT YMD", "SET XACT_ABORT ON"] {
        w.sql(s);
        w.go();
    }
    if transactions {
        w.sql(&format!("SET TRANSACTION ISOLATION LEVEL {}", settings.transaction_isolation_level));
        w.go();
    }
    let ddl_triggers = if opts.has(DataOpt::DisableDdlTriggers) { result.ddl_triggers.as_slice() } else { &[] };
    if !ddl_triggers.is_empty() {
        w.print("Disable all DDL triggers for database");
        w.go();
        w.sql("DISABLE TRIGGER ALL ON DATABASE");
        w.go();
    }
    if transactions {
        w.sql("");
        w.sql("BEGIN TRANSACTION");
    }

    let tables: Vec<&Plan> = order.iter().map(|&i| &plans[i]).collect();
    let fk_mode = opts.has(DataOpt::DisableKeys) || opts.has(DataOpt::DropConstraintsAndIndexes);
    let drop_indexes = opts.has(DataOpt::DropConstraintsAndIndexes);
    let key_index = |p: &Plan| p.t.pair.target_key_name.clone().or_else(|| p.t.pair.key_name.clone());
    // Indexes dropped for DropConstraintsAndIndexes: (clustered pass, nonclustered pass).
    let droppable = |p: &Plan, clustered: bool| -> Vec<&Index> {
        let tg = target_of(target, &p.t.pair.target);
        if !drop_indexes || !tg.is_table || tg.memory_optimized {
            return Vec::new();
        }
        let key = key_index(p);
        let table = target.table(&p.t.pair.target.schema, &p.t.pair.target.name);
        // The unique index on a FILESTREAM table's ROWGUIDCOL cannot be dropped while it has data.
        let filestream_key = |i: &Index| {
            i.columns.len() == 1
                && (i.primary_key || i.unique_constraint)
                && table.is_some_and(|t| {
                    t.columns.iter().any(|c| c.filestream) && t.columns.iter().any(|c| c.name.eq_ignore_ascii_case(&i.columns[0].name) && c.rowguidcol && !c.nullable)
                })
        };
        tg.indexes
            .iter()
            .filter(|i| {
                !matches!(i.kind, IndexKind::Xml | IndexKind::ClusteredColumnstore | IndexKind::NonclusteredColumnstore | IndexKind::Heap)
                    && !i.disabled
                    && Some(&i.name) != key.as_ref()
                    && (i.kind == IndexKind::Clustered) == clustered
                    && !filestream_key(i)
            })
            .collect()
    };
    // XML indexes hang on the clustered index: when one is dropped, every deployed table's
    // primary XML indexes go first (their secondary ones with them) and come back after it.
    let dropping_clustered = tables.iter().any(|p| !droppable(p, true).is_empty());
    let xml_indexes = |p: &Plan, primary: bool| -> Vec<&Index> {
        let tg = target_of(target, &p.t.pair.target);
        if !dropping_clustered || !tg.is_table {
            return Vec::new();
        }
        tg.indexes.iter().filter(|i| i.kind == IndexKind::Xml && i.xml.as_ref().is_none_or(|x| x.using_index.is_none()) == primary).collect()
    };

    // Disable: DML triggers, foreign keys, indexes, columnstore indexes.
    if opts.has(DataOpt::DisableTriggers) {
        for p in &tables {
            let tg = target_of(target, &p.t.pair.target);
            for (n, tr) in tg.triggers.iter().filter(|tr| !tr.disabled).enumerate() {
                if n == 0 {
                    w.print(&format!("Disable DML triggers on {}", p.t.pair.target));
                }
                w.sql(&trigger_sql(&p.t.pair.target, tr, tg.is_table, "DISABLE"));
            }
        }
    }
    if fk_mode {
        for (i, p) in tables.iter().enumerate() {
            let table = &p.t.pair.target;
            let tg = target_of(target, table);
            if tg.is_table {
                for (n, fk) in tg.foreign_keys.iter().enumerate() {
                    if n == 0 {
                        w.print(&format!("Drop constraints from {table}"));
                    }
                    w.sql(&fk_off(table, fk, drop_indexes));
                }
            }
            for (other, fk) in extra_fks[order[i]].iter().filter(|(_, fk)| !fk.disabled) {
                w.print(&format!("Drop constraint {} from {other}", quote(&fk.name)));
                w.sql(&fk_off(other, fk, drop_indexes));
            }
        }
    }
    for clustered in [false, true] {
        if clustered {
            for p in &tables {
                for ix in xml_indexes(p, true) {
                    w.sql(&ddl::drop_index(&p.t.pair.target.quoted(), ix));
                }
            }
        }
        for p in &tables {
            for (n, ix) in droppable(p, clustered).into_iter().enumerate() {
                if n == 0 {
                    w.print(&format!("Drop unused indexes from {}", p.t.pair.target));
                }
                w.sql(&ddl::drop_index(&p.t.pair.target.quoted(), ix));
            }
        }
    }
    for p in &tables {
        for ix in target_of(target, &p.t.pair.target).indexes.iter().filter(|i| i.kind == IndexKind::NonclusteredColumnstore && !i.disabled) {
            w.go();
            w.sql(&format!("ALTER INDEX {} ON {} DISABLE", quote(&ix.name), p.t.pair.target));
            w.go();
        }
    }

    // Rows: deletes and updates children first, inserts parents first.
    let fetch = Fetch { spec: settings.source_connection.as_ref() };
    for p in &tables {
        if p.delete {
            write_deletes(&mut w, p, target, by_row, &mut warnings)?;
        }
    }
    for p in &tables {
        for c in &p.t.pair.columns {
            if let Some(check) = mapping::type_check(&c.source_type, &c.target_type, None, None)
                && check.has("TypeConversion")
            {
                warnings.push(Warning {
                    severity: Severity::Medium,
                    object: p.t.pair.target.to_string(),
                    message: format!(
                        "Conversion from field of type '{}' to field of type '{}' might not be possible.\n\nSource Field: {}.{}\nTarget Field: {}.{}",
                        c.source_type.sql(),
                        c.target_type.sql(),
                        p.t.pair.source,
                        quote(&c.source),
                        p.t.pair.target,
                        quote(&c.target)
                    ),
                });
            }
        }
    }
    for p in &tables {
        if p.update {
            write_updates(&mut w, p, target, opts, by_row, &fetch, &mut warnings)?;
        }
    }
    for p in tables.iter().rev() {
        if p.add {
            write_inserts(&mut w, p, target, opts, by_row, &fetch, &mut warnings)?;
        }
    }

    // Restore: clustered indexes, nonclustered indexes, foreign keys, triggers, columnstore.
    for clustered in [true, false] {
        for p in &tables {
            let table = p.t.pair.target.quoted();
            for (n, ix) in droppable(p, clustered).into_iter().enumerate() {
                if n == 0 {
                    w.print(&format!("Add indexes to {}", p.t.pair.target));
                }
                for s in ddl::create_index(&table, ix) {
                    w.sql(&s);
                }
            }
        }
        if clustered {
            for primary in [true, false] {
                for p in &tables {
                    for ix in xml_indexes(p, primary) {
                        for s in ddl::create_index(&p.t.pair.target.quoted(), ix) {
                            w.sql(&s);
                        }
                    }
                }
            }
        }
    }
    // A dropped foreign key gets its extended properties back with it.
    let fk_xprops = |w: &mut Writer, table: &ObjectName, fk: &ForeignKey| {
        if !drop_indexes {
            return;
        }
        for x in target.extended_properties.iter().filter(|x| {
            x.class == "TABLE"
                && x.schema.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&table.schema))
                && x.object.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&table.name))
                && x.minor_name.as_deref() == Some(fk.name.as_str())
        }) {
            w.sql(&ddl::add_xprop(x, &ddl::xprop_levels(x, str::to_string)));
        }
    };
    if fk_mode {
        for (i, p) in tables.iter().enumerate() {
            let table = &p.t.pair.target;
            let tg = target_of(target, table);
            if tg.is_table {
                for (n, fk) in tg.foreign_keys.iter().enumerate() {
                    if n == 0 {
                        w.print(&format!("Add constraints to {table}"));
                    }
                    for s in fk_on(table, fk, drop_indexes, opts) {
                        w.sql(&s);
                    }
                    fk_xprops(&mut w, table, fk);
                }
            }
            for (other, fk) in extra_fks[order[i]].iter().filter(|(_, fk)| !fk.disabled) {
                for s in fk_on(other, fk, drop_indexes, opts) {
                    w.sql(&s);
                }
                fk_xprops(&mut w, other, fk);
            }
        }
    }
    if opts.has(DataOpt::DisableTriggers) {
        for p in &tables {
            let tg = target_of(target, &p.t.pair.target);
            for (n, tr) in tg.triggers.iter().filter(|tr| !tr.disabled).enumerate() {
                if n == 0 {
                    w.print(&format!("Add DML triggers to {}", p.t.pair.target));
                }
                w.sql(&trigger_sql(&p.t.pair.target, tr, tg.is_table, "ENABLE"));
            }
        }
    }
    for p in &tables {
        for ix in target_of(target, &p.t.pair.target).indexes.iter().filter(|i| i.kind == IndexKind::NonclusteredColumnstore && !i.disabled) {
            w.sql(&format!("ALTER INDEX {} ON {} REBUILD", quote(&ix.name), p.t.pair.target));
        }
    }

    // Trailer.
    if transactions {
        w.sql("");
        w.sql("COMMIT TRANSACTION");
        w.go();
    }
    for name in ddl_triggers {
        w.print(&format!("Enable DDL trigger {}", quote(name)));
        w.go();
        w.sql(&format!("ENABLE TRIGGER {} ON DATABASE", quote(name)));
        w.go();
    }
    if opts.has(DataOpt::ReseedIdentity) {
        // Only the deployed tables are reseeded.
        for p in &tables {
            let t = p.t;
            let tg = target.table(&t.pair.target.schema, &t.pair.target.name);
            let increment = tg
                .and_then(|tg| tg.columns.iter().find_map(|c| c.identity.as_ref()))
                .and_then(|i| i.increment.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            if let (Some(src), Some(dst)) = t.identity_current
                && src != dst
                && increment != 0.0
                && t.pair.columns.iter().any(|c| c.identity && c.source_identity)
            {
                let name = t.pair.target.to_string();
                w.print(&format!("Reseed identity on {name}"));
                w.sql(&format!("DBCC CHECKIDENT(N'{name}', RESEED, {src})"));
                w.sql(&format!("DBCC CHECKIDENT(N'{name}', RESEED)"));
                w.go();
            }
        }
        w.go();
    }
    w.finish();
    if w.split {
        warnings.push(Warning {
            severity: Severity::Medium,
            object: String::new(),
            message: "The resulting script contains multiple transactions and cannot be rolled back in entirety".into(),
        });
    }
    let mut seen = HashSet::new();
    warnings.retain(|x| seen.insert((x.object.clone(), x.message.clone())));
    Ok(DeploymentScript { sql: w.text, warnings, batches: w.batches })
}

/// The High warning SQL Data Compare raises for rows deployed by a non-unique comparison key.
fn repeated_warning(table: &ObjectName, text: String) -> Warning {
    Warning { severity: Severity::High, object: table.to_string(), message: text }
}

fn trigger_sql(table: &ObjectName, tr: &Trigger, is_table: bool, verb: &str) -> String {
    if is_table {
        format!("ALTER TABLE {table} {verb} TRIGGER {}", quote(&tr.name))
    } else {
        // ALTER TABLE cannot name a view.
        format!("{verb} TRIGGER {} ON {table}", ddl::qname(&table.schema, &tr.name))
    }
}

fn fk_off(table: &ObjectName, fk: &ForeignKey, drop: bool) -> String {
    if drop {
        format!("ALTER TABLE {table} DROP CONSTRAINT {}", quote(&fk.name))
    } else {
        format!("ALTER TABLE {table} NOCHECK CONSTRAINT {}", quote(&fk.name))
    }
}

fn fk_on(table: &ObjectName, fk: &ForeignKey, dropped: bool, opts: &DataOptions) -> Vec<String> {
    if dropped {
        return ddl::add_foreign_key(&table.quoted(), fk, false);
    }
    if fk.disabled {
        Vec::new()
    } else if fk.not_trusted || opts.has(DataOpt::SkipFkChecks) {
        vec![format!("ALTER TABLE {table} CHECK CONSTRAINT {}", quote(&fk.name))]
    } else {
        vec![format!("ALTER TABLE {table} WITH CHECK CHECK CONSTRAINT {}", quote(&fk.name))]
    }
}

/// `M/d/yyyy h:mm:ss tt` (UTC).
fn now_text() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let Some(t) = chrono::DateTime::from_timestamp(secs, 0) else { return String::new() };
    let t = t.naive_utc();
    use chrono::{Datelike, Timelike};
    let h = t.hour();
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    format!("{}/{}/{} {h12}:{:02}:{:02} {} UTC", t.month(), t.day(), t.year(), t.minute(), t.second(), if h < 12 { "AM" } else { "PM" })
}

/// Reads full values of large columns from database 1 when a row is written.
struct Fetch<'a> {
    spec: Option<&'a ConnectionSpec>,
}

impl Fetch<'_> {
    fn complete(&self, pair: &TablePair, values: &mut [Value]) -> Result<(), String> {
        for c in 0..values.len() {
            if matches!(values[c], Value::Large(..)) {
                let spec = self.spec.ok_or_else(|| format!("{}: a large value of {} needs a connection to database 1", pair.source, pair.columns[c].source))?;
                values[c] = compare::fetch_value(spec, pair, c, values)?;
            }
        }
        Ok(())
    }
}

fn for_each_row(t: &TableResult, kind: RowKind, mut f: impl FnMut(u64, StoredRow) -> Result<(), String>) -> Result<(), String> {
    const PAGE: usize = 10_000;
    let offsets = &t.offsets[kind as usize];
    let mut start = 0;
    while start < offsets.len() {
        for (i, row) in store::read_page(&t.path, offsets, start, PAGE).map_err(|e| e.to_string())?.into_iter().enumerate() {
            f((start + i) as u64, row)?;
        }
        start += PAGE;
    }
    Ok(())
}

fn literal_ctx<'a>(c: &'a ColumnPair, clr_as_text: bool, target_table: &'a str) -> Literal<'a> {
    Literal {
        source: &c.source_type,
        target: &c.target_type,
        source_collation: c.source_collation.as_deref(),
        target_collation: c.target_collation.as_deref(),
        clr_as_text,
        // Node and edge ids name their table; $from_id and $to_id point at other tables.
        graph_table: c.graph.as_deref().filter(|g| matches!(*g, "$node_id" | "$edge_id")).map(|_| target_table),
    }
}

fn is_inexact(c: &ColumnPair) -> bool {
    let t = &c.target_type;
    matches!(t.name.as_str(), "timestamp" | "rowversion" | "text" | "ntext" | "xml" | "image") || t.length == Some(-1)
}

/// ` WITH ( SNAPSHOT)` for memory-optimised tables inside a transaction.
fn hint(target: &Database, table: &ObjectName, transactions: bool) -> &'static str {
    if transactions && target_of(target, table).memory_optimized { " WITH ( SNAPSHOT)" } else { "" }
}

/// The WHERE condition for a row of the target: the key, or every comparable column when keys
/// repeat, then the table's filter. As SQL Data Compare writes it, a string literal of a deleted
/// row is N'' (and cast) when the source column needs it and the cast takes the target column's
/// collation and type; for an updated row the two columns swap roles.
fn where_sql(p: &TablePair, repeated: bool, values: &[Value], update: bool, warnings: &mut Vec<Warning>) -> String {
    let mut parts = Vec::new();
    let mut inexact = false;
    let cols: Vec<usize> = if repeated { (0..p.columns.len()).collect() } else { p.key.clone() };
    for c in cols {
        let col = &p.columns[c];
        if is_inexact(col) {
            inexact = true;
            continue;
        }
        let name = quote(&col.target);
        let lit = Literal { source: &col.target_type, graph_table: None, ..literal_ctx(col, false, "") };
        let part = match &values[c] {
            Value::Null => format!("{name} IS NULL"),
            v @ Value::Clr(..) if matches!(col.target_type.name.as_str(), "geometry" | "geography") => format!("{name}.STEquals({})=1", v.sql(&lit)),
            Value::Str(s) if col.graph.is_none() => {
                // (decides N'' and the cast, whose Unicode-ness skips the cast, gives collation and type)
                let (cast_by, unicode_by, cast_to, cast_collation) = if update {
                    (&col.target_type, &col.source_type, &col.source_type, col.source_collation.as_deref())
                } else {
                    (&col.source_type, &col.target_type, &col.target_type, col.target_collation.as_deref())
                };
                let by_collation = if update { col.target_collation.as_deref() } else { col.source_collation.as_deref() };
                let n = Literal { source: cast_by, source_collation: by_collation, ..literal_ctx(col, false, "") }.source_needs_n();
                if n && !matches!(unicode_by.name.as_str(), "nchar" | "nvarchar" | "ntext" | "sysname" | "xml" | "sql_variant") {
                    let size = cast_to.length.map(|l| if l <= 0 { "max".to_string() } else { l.to_string() }).unwrap_or_else(|| "max".into());
                    let coll = cast_collation.unwrap_or("DATABASE_DEFAULT");
                    format!("{name} = cast({} COLLATE {coll} as {}({size}))", sql_string(s, true), cast_to.name)
                } else {
                    format!("{name} = {}", sql_string(s, n))
                }
            }
            v => format!("{name} = {}", v.sql(&lit)),
        };
        parts.push(part);
    }
    if inexact && repeated {
        warnings.push(Warning {
            severity: Severity::High,
            object: p.target.to_string(),
            message: format!(
                "Unable to use a full row-based where clause on table or view {} with repeating comparison keys; results may not be as expected. Table or view contains text, image, or xml data types.",
                p.target
            ),
        });
    }
    let mut out = parts.join(" AND ");
    if let Some(filter) = p.where2.as_deref().map(strip_filter).filter(|f| !f.is_empty()) {
        out = if out.is_empty() { format!(" ({filter})") } else { format!("{out} AND ({filter})") };
    }
    out
}

/// A user filter without a leading WHERE and one pair of outer parentheses.
fn strip_filter(f: &str) -> String {
    let mut f = f.trim();
    if f.len() > 5 && f[..5].eq_ignore_ascii_case("where") && f[5..].starts_with(char::is_whitespace) {
        f = f[5..].trim();
    }
    if f.starts_with('(') && f.ends_with(')') {
        let inner = &f[1..f.len() - 1];
        let mut depth = 0i32;
        let balanced = inner.chars().all(|c| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            depth >= 0
        }) && depth == 0;
        if balanced {
            return inner.trim().to_string();
        }
    }
    f.to_string()
}

fn write_deletes(w: &mut Writer, p: &Plan, target: &Database, by_row: bool, warnings: &mut Vec<Warning>) -> Result<(), String> {
    let t = p.t;
    let name = &t.pair.target;
    let total = t.count(RowKind::OnlyIn2);
    let repeated = t.repeated_keys;
    let suffix = if repeated { " with non-unique comparison key" } else { "" };
    let prefix = format!("DELETE FROM {name}{} WHERE ", hint(target, name, w.transactions));
    let mut n = 0u64;
    for_each_row(t, RowKind::OnlyIn2, |i, row| {
        if !p.selected(RowKind::OnlyIn2, i) {
            return Ok(());
        }
        if n == 0 {
            if by_row {
                w.print(&format!("Delete {} from {name}{suffix}", rows_word(total)));
            } else {
                w.print(&format!("Delete {total} {} from {name}{suffix}", rows_word(total)));
            }
            if repeated {
                warnings.push(repeated_warning(name, format!("Delete {total} {} from {name}{suffix}", rows_word(total))));
                w.sql("SET ROWCOUNT 1");
            }
        }
        n += 1;
        w.sql(&format!("{prefix}{}", where_sql(&t.pair, repeated, &row.target, false, warnings)));
        w.end_statement();
        Ok(())
    })?;
    if n > 0 && repeated {
        w.sql("SET ROWCOUNT 0");
    }
    if by_row && n > 0 && total > 1 {
        w.print(&format!("Operation applied to {n} rows out of {total}"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_updates(w: &mut Writer, p: &Plan, target: &Database, opts: &DataOptions, by_row: bool, fetch: &Fetch, warnings: &mut Vec<Warning>) -> Result<(), String> {
    let t = p.t;
    let pair = &t.pair;
    let name = &pair.target;
    let tg = target_of(target, name);
    let total = t.count(RowKind::Different);
    let repeated = t.repeated_keys;
    let clr_as_text = !opts.has(DataOpt::TransportClrBinary);
    let target_name = name.name.clone();
    let identity = (0..pair.columns.len()).find(|&c| pair.columns[c].identity);
    let mut must_reinsert = identity.is_some_and(|c| !pair.key.contains(&c)) || tg.edge;
    let differs = |c: usize| t.column_differences.get(c).copied().unwrap_or(0) > 0;
    let candidates: Vec<usize> = (0..pair.columns.len()).filter(|&c| !pair.key.contains(&c) && differs(c)).collect();
    let all_updatable = candidates.iter().all(|&c| !pair.columns[c].read_only);
    if !candidates.iter().any(|&c| !pair.columns[c].read_only) && !tg.edge {
        warnings.push(Warning {
            severity: Severity::Low,
            object: name.to_string(),
            message: format!("Table {name} contains differences. However, these cannot be synchronized because no columns in the table can be updated."),
        });
        return Ok(());
    }
    if !p.add {
        type_warnings(pair, opts, false, warnings);
    }
    let needs_reinsert = |row: &StoredRow| tg.edge || identity.is_some_and(|c| row.source.get(c) != row.target.get(c));
    let suffix = if repeated { " with non-unique comparison key" } else { "" };
    let message = |w: &mut Writer, reinsert: bool, warnings: &mut Vec<Warning>| {
        if repeated {
            let text = if reinsert {
                format!("Delete and re-insert {total} {} in {name} due to identity row modification{suffix}", rows_word(total))
            } else {
                format!("Update {total} {} in {name}{suffix}", rows_word(total))
            };
            warnings.push(repeated_warning(name, text));
        }
        if !by_row && all_updatable {
            let text = if reinsert && tg.edge {
                format!("Delete and re-insert {total} {} in {name} due to table being a SQL Graph EDGE table{suffix}", rows_word(total))
            } else if reinsert {
                format!("Delete and re-insert {total} {} in {name} due to identity row modification{suffix}", rows_word(total))
            } else {
                format!("Update {total} {} in {name}{suffix}", rows_word(total))
            };
            w.print(&text);
        } else if reinsert {
            w.print(&format!("Delete and re-insert {} in {name} due to identity row modification{suffix}", rows_word(total)));
        } else {
            w.print(&format!("Update {} in {name}{suffix}", rows_word(total)));
        }
        if repeated {
            w.sql("SET ROWCOUNT 1");
        }
    };
    let mut header = false;
    let mut reinserted = 0u64;
    let h = hint(target, name, w.transactions);
    // Rows whose identity changes (and every edge row) are deleted first, then inserted again.
    if must_reinsert {
        let mut deleted = 0u64;
        for_each_row(t, RowKind::Different, |i, row| {
            if !p.selected(RowKind::Different, i) {
                return Ok(());
            }
            if !header {
                message(w, true, warnings);
                header = true;
            }
            if needs_reinsert(&row) {
                w.sql(&format!("DELETE FROM {name}{h} WHERE {}", where_sql(pair, repeated, &row.target, true, warnings)));
                w.end_statement();
                deleted += 1;
            }
            Ok(())
        })?;
        if deleted == 0 {
            must_reinsert = false;
        }
    }
    let mut updated = 0u64;
    for_each_row(t, RowKind::Different, |i, row| {
        if !p.selected(RowKind::Different, i) {
            return Ok(());
        }
        if !header {
            message(w, false, warnings);
            header = true;
        }
        if must_reinsert && needs_reinsert(&row) {
            return Ok(());
        }
        let mut source = row.source.clone();
        let sets: Vec<usize> = candidates.iter().copied().filter(|&c| !pair.columns[c].read_only && row.differs.get(c).copied().unwrap_or(false)).collect();
        if !sets.is_empty() {
            fetch.complete(pair, &mut source)?;
            let list: Vec<String> =
                sets.iter().map(|&c| format!("{}={}", quote(&pair.columns[c].target), source[c].sql(&literal_ctx(&pair.columns[c], clr_as_text, &target_name)))).collect();
            w.sql(&format!("UPDATE {name}{h} SET {} WHERE {}", list.join(", "), where_sql(pair, repeated, &row.target, true, warnings)));
            updated += 1;
        }
        w.end_statement();
        Ok(())
    })?;
    if must_reinsert {
        let writable: Vec<usize> = (0..pair.columns.len()).filter(|&c| !pair.columns[c].read_only).collect();
        let cols = writable.iter().map(|&c| quote(&pair.columns[c].target)).collect::<Vec<_>>().join(",");
        if identity.is_some() && tg.is_table {
            w.sql(&format!("SET IDENTITY_INSERT {name} ON"));
        }
        for_each_row(t, RowKind::Different, |i, row| {
            if !p.selected(RowKind::Different, i) || !needs_reinsert(&row) {
                return Ok(());
            }
            let mut source = row.source;
            fetch.complete(pair, &mut source)?;
            let values: Vec<String> = writable.iter().map(|&c| source[c].sql(&literal_ctx(&pair.columns[c], clr_as_text, &target_name))).collect();
            w.sql(&format!("INSERT INTO {name} ({cols}) VALUES ({})", values.join(",")));
            w.end_statement();
            reinserted += 1;
            Ok(())
        })?;
        if identity.is_some() && tg.is_table {
            w.sql(&format!("SET IDENTITY_INSERT {name} OFF"));
        }
    }
    if repeated {
        w.sql("SET ROWCOUNT 0");
    }
    let n = if must_reinsert { reinserted.max(updated) } else { updated };
    if n > 0 && (by_row || !all_updatable) && total > 1 {
        w.print(&format!("Operation applied to {n} rows out of {total}"));
    }
    Ok(())
}

/// The target-side type warnings of a table's column pairs.
fn type_warnings(pair: &TablePair, opts: &DataOptions, force_collation: bool, warnings: &mut Vec<Warning>) {
    let o = if force_collation { opts.clone().without(DataOpt::ForceBinaryCollation) } else { opts.clone() };
    for c in &pair.columns {
        if let Some(check) = mapping::type_check(&c.source_type, &c.target_type, c.source_collation.as_deref(), c.target_collation.as_deref()) {
            let f1 = format!("{}.{}", pair.source, quote(&c.source));
            let f2 = format!("{}.{}", pair.target, quote(&c.target));
            for n in mapping::type_notes(&check, &f1, &f2, &pair.target, true, &o) {
                warnings.push(Warning { severity: n.severity, object: pair.target.to_string(), message: n.text });
            }
        }
    }
}

fn write_inserts(w: &mut Writer, p: &Plan, target: &Database, opts: &DataOptions, by_row: bool, fetch: &Fetch, warnings: &mut Vec<Warning>) -> Result<(), String> {
    let t = p.t;
    let pair = &t.pair;
    let name = &pair.target;
    let tg = target_of(target, name);
    let total = t.count(RowKind::OnlyIn1);
    let repeated = t.repeated_keys;
    let clr_as_text = !opts.has(DataOpt::TransportClrBinary);
    let target_name = name.name.clone();
    let writable: Vec<usize> = (0..pair.columns.len()).filter(|&c| !pair.columns[c].read_only).collect();
    let has_identity = tg.is_table && pair.columns.iter().any(|c| c.identity);
    let prefix = format!("INSERT INTO {name} ({}) VALUES (", writable.iter().map(|&c| quote(&pair.columns[c].target)).collect::<Vec<_>>().join(", "));
    type_warnings(pair, opts, true, warnings);
    let suffix = if repeated { " with non-unique comparison key" } else { "" };
    let mut n = 0u64;
    for_each_row(t, RowKind::OnlyIn1, |i, row| {
        if !p.selected(RowKind::OnlyIn1, i) {
            return Ok(());
        }
        if n == 0 {
            if by_row {
                w.print(&format!("Add {} to {name}{suffix}", rows_word(total)));
            } else {
                w.print(&format!("Add {total} {} to {name}{suffix}", rows_word(total)));
            }
            if has_identity {
                w.sql(&format!("SET IDENTITY_INSERT {name} ON"));
            }
            if repeated {
                warnings.push(repeated_warning(name, format!("Add {total} {} to {name}{suffix}", rows_word(total))));
                w.sql("SET ROWCOUNT 1");
            }
        }
        n += 1;
        let mut source = row.source;
        fetch.complete(pair, &mut source)?;
        let values: Vec<String> = writable.iter().map(|&c| source[c].sql(&literal_ctx(&pair.columns[c], clr_as_text, &target_name))).collect();
        w.sql(&format!("{prefix}{})", values.join(", ")));
        w.end_statement();
        Ok(())
    })?;
    if n > 0 {
        if has_identity {
            w.sql(&format!("SET IDENTITY_INSERT {name} OFF"));
        }
        if repeated {
            w.sql("SET ROWCOUNT 0");
        }
    }
    if by_row && n > 0 && total > 1 {
        w.print(&format!("Operation applied to {n} rows out of {total}"));
    }
    Ok(())
}

/// The order tables are deployed in (indexes into `plans`, deployed tables only): a table
/// that depends on others comes before them, so deletes run children first and inserts (in
/// reverse) parents first. Also returns, per plan, the foreign keys of other tables that must
/// be disabled with it, and adds the circular-reference and undeployed-key warnings.
fn dependency_order(plans: &[Plan], target: &Database, opts: &DataOptions, warnings: &mut Vec<Warning>) -> (Vec<usize>, Vec<Vec<(ObjectName, ForeignKey)>>) {
    let names: Vec<&ObjectName> = plans.iter().map(|p| &p.t.pair.target).collect();
    let mut extra: Vec<Vec<(ObjectName, ForeignKey)>> = vec![Vec::new(); plans.len()];
    // "x depends on y" over the target database's objects.
    let mut direct: HashSet<(ObjectName, ObjectName)> = HashSet::new();
    for d in &target.dependencies {
        if d.from != d.to {
            direct.insert((d.from.clone(), d.to.clone()));
        }
    }
    for t in &target.tables {
        for fk in &t.foreign_keys {
            if fk.referenced_table != t.object_name() {
                direct.insert((t.object_name(), fk.referenced_table.clone()));
            }
        }
    }
    let graph = |n: &ObjectName| target.table(&n.schema, &n.name).and_then(|t| t.graph.clone());
    for e in names.iter().filter(|n| graph(n).as_deref() == Some("EDGE")) {
        for node in names.iter().filter(|n| graph(n).as_deref() == Some("NODE")) {
            direct.insert(((*e).clone(), (*node).clone()));
        }
    }
    // Close over the compared tables: T → parent → child gives T → child; reaching T again is a cycle.
    let mut deps = direct.clone();
    let mut edges: Vec<(ObjectName, ObjectName)> = direct.iter().cloned().collect();
    edges.sort();
    let mut circular: BTreeSet<ObjectName> = BTreeSet::new();
    let mut i = 0;
    while i < edges.len() {
        let (parent, child) = edges[i].clone();
        for &t in &names {
            if deps.contains(&(t.clone(), child.clone())) || !deps.contains(&(t.clone(), parent.clone())) {
                continue;
            }
            if child == *t {
                if circular.insert(t.clone()) {
                    warnings.push(Warning { severity: Severity::High, object: t.to_string(), message: format!("Circular dependency for the table or view {t}") });
                }
            } else {
                deps.insert((t.clone(), child.clone()));
                edges.push((t.clone(), child.clone()));
            }
        }
        i += 1;
    }
    let is_table = |n: &ObjectName| target.table(&n.schema, &n.name).is_some();
    let synced: Vec<bool> = plans.iter().map(Plan::synced).collect();
    let n = synced.iter().filter(|s| **s).count();
    let mut out: Vec<Option<usize>> = vec![None; n];
    let mut slot = n;
    let mut remaining: Vec<usize> = (0..plans.len()).collect();
    let mut removed: HashSet<(usize, usize)> = HashSet::new();
    let skip = opts.has(DataOpt::SkipFkChecks);
    let severity = if skip { Severity::Medium } else { Severity::High };
    for pass in 0.. {
        let snapshot = std::mem::take(&mut remaining);
        let mut placed = Vec::new();
        for &x in &snapshot {
            let blocked = snapshot.iter().any(|&y| y != x && synced[y] && is_table(names[y]) && deps.contains(&(names[x].clone(), names[y].clone())));
            if blocked {
                remaining.push(x);
            } else {
                if synced[x] {
                    slot -= 1;
                    out[slot] = Some(x);
                }
                placed.push(x);
            }
        }
        if remaining.is_empty() {
            break;
        }
        for &x in placed.iter().filter(|&&x| synced[x]) {
            let (mut to, mut from) = (false, false);
            for u in (0..plans.len()).filter(|&u| u != x) {
                if !synced[u] {
                    if direct.contains(&(names[x].clone(), names[u].clone())) {
                        to = true;
                    }
                    if !removed.contains(&(u, x)) && direct.contains(&(names[u].clone(), names[x].clone())) && is_table(names[u]) {
                        from = true;
                    }
                }
                removed.insert((u, x));
            }
            let t = names[x];
            if to {
                let text = if skip {
                    format!("The table {t} has foreign keys to other undeployed tables; the database may be left in an inconsistent state.")
                } else {
                    format!("The table {t} has foreign keys to other undeployed tables; the deployment script may fail when the keys are reinstated. To skip these integrity checks and leave the database without referential integrity, use \"Skip integrity checks for foreign key constraints\" under Deployment Behavior.")
                };
                warnings.push(Warning { severity, object: t.to_string(), message: text });
            }
            if from {
                let text = if skip {
                    format!("Undeployed tables have foreign keys to table {t}; the database may be left in an inconsistent state.")
                } else {
                    format!("Undeployed tables have foreign keys to table {t}; the deployment script may fail when the keys are reinstated.  To skip these integrity checks and leave the database without referential integrity, use \"Skip integrity checks for foreign key constraints\" under Deployment Behavior.")
                };
                warnings.push(Warning { severity, object: t.to_string(), message: text });
            }
        }
        if pass + 1 > plans.len() {
            for &x in &remaining {
                if synced[x] && slot > 0 {
                    slot -= 1;
                    out[slot] = Some(x);
                }
            }
            break;
        }
    }
    // Foreign keys of tables that are not deployed and reference a deployed one go off with it.
    let deployed: HashSet<&ObjectName> = (0..plans.len()).filter(|&i| synced[i]).map(|i| names[i]).collect();
    for x in (0..plans.len()).filter(|&i| synced[i]) {
        for t in target.tables.iter().filter(|t| !deployed.contains(&t.object_name())) {
            for fk in t.foreign_keys.iter().filter(|fk| fk.referenced_table == *names[x]) {
                if !extra[x].iter().any(|(o, f)| *o == t.object_name() && f.name == fk.name) {
                    extra[x].push((t.object_name(), fk.clone()));
                }
            }
        }
    }
    (out.into_iter().flatten().collect(), extra)
}

/// Builds the script the way SQL Data Compare's execution block does: each statement on its
/// own line(s), `GO` only when something was written since the last one, PRINT comments
/// preceded by a blank line.
struct Writer {
    text: String,
    batches: Vec<String>,
    current: String,
    written: bool,
    comments: bool,
    transactions: bool,
    max_bytes: Option<u64>,
    since_split: u64,
    split: bool,
}

impl Writer {
    fn new(comments: bool, max_bytes: Option<u64>, transactions: bool) -> Writer {
        Writer { text: String::new(), batches: Vec::new(), current: String::new(), written: false, comments, transactions, max_bytes, since_split: 0, split: false }
    }

    /// One statement (or nothing, for ""), with a line break added when it lacks one.
    fn sql(&mut self, s: &str) {
        self.written = true;
        if s.is_empty() {
            return;
        }
        let mut s = s.replace("\r\n", "\n").replace('\n', "\r\n");
        if !s.ends_with('\n') {
            s.push_str("\r\n");
        }
        if self.current.len() + s.len() > EXEC_CHUNK && !self.current.trim().is_empty() {
            self.batches.push(std::mem::take(&mut self.current));
        }
        self.since_split += s.len() as u64;
        self.text.push_str(&s);
        self.current.push_str(&s);
    }

    fn print(&mut self, message: &str) {
        if self.comments {
            self.sql(&format!("\r\nPRINT({})", sql_string(message, true)));
        }
    }

    fn go(&mut self) {
        if !self.written {
            return;
        }
        self.text.push_str("GO\r\n");
        if !self.current.trim().is_empty() {
            self.batches.push(std::mem::take(&mut self.current));
        }
        self.current.clear();
        self.written = false;
    }

    /// After each row statement: start a new transaction when this one has grown too large.
    fn end_statement(&mut self) {
        let Some(max) = self.max_bytes else { return };
        if self.since_split <= max {
            return;
        }
        if self.transactions {
            self.split = true;
            self.sql("COMMIT TRANSACTION");
            self.go();
            self.sql("BEGIN TRANSACTION");
        } else {
            self.go();
        }
        self.since_split = 0;
    }

    fn finish(&mut self) {
        if !self.current.trim().is_empty() {
            self.batches.push(std::mem::take(&mut self.current));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Column, DataType, Identity, IndexColumn, Table};
    use crate::data::mapping::{DataMappings, map_tables};
    use crate::data::store::{Session, TableWriter};

    fn col(name: &str, t: &str) -> Column {
        Column { name: name.into(), data_type: DataType { name: t.into(), ..Default::default() }, nullable: true, ..Default::default() }
    }

    fn table(name: &str, cols: Vec<Column>, fks: Vec<ForeignKey>) -> Table {
        Table {
            schema: "dbo".into(),
            name: name.into(),
            indexes: vec![Index {
                name: format!("PK_{name}"),
                primary_key: true,
                unique: true,
                kind: IndexKind::Clustered,
                columns: vec![IndexColumn { name: cols[0].name.clone(), descending: false }],
                ..Default::default()
            }],
            columns: cols,
            foreign_keys: fks,
            ..Default::default()
        }
    }

    /// The worked example: Orders references Customers; one delete, one update, two inserts.
    fn example() -> (DataComparison, Database) {
        let mut cid = col("CustomerID", "int");
        cid.identity = Some(Identity { seed: "1".into(), increment: "1".into(), not_for_replication: false });
        let mut email = col("Email", "varchar");
        email.data_type.length = Some(100);
        email.collation = Some("Latin1_General_CI_AS".into());
        let mut oid = col("OrderID", "int");
        oid.identity = cid.identity.clone();
        let customers = table("Customers", vec![cid, col("Name", "nvarchar"), email], vec![]);
        let fk = ForeignKey {
            name: "FK_Orders_Customers".into(),
            columns: vec!["CustomerID".into()],
            referenced_table: ObjectName::new("dbo", "Customers"),
            referenced_columns: vec!["CustomerID".into()],
            on_delete: "NO_ACTION".into(),
            on_update: "NO_ACTION".into(),
            ..Default::default()
        };
        let orders = table("Orders", vec![oid, col("CustomerID", "int"), col("Amount", "money"), col("OrderDate", "datetime")], vec![fk]);
        let db = Database { tables: vec![customers, orders], ..Default::default() };
        let (pairs, _, _) = map_tables(&db, &db, &DataOptions::default_set(), &DataMappings::default());
        let session = Session::create().unwrap();
        let rows = |i: usize, rows: Vec<StoredRow>| {
            let path = session.table_path(i);
            let mut w = TableWriter::create(&path, false).unwrap();
            for r in &rows {
                w.write(r).unwrap();
            }
            (path, w.finish().unwrap())
        };
        let s = |v: &str| Value::Str(v.into());
        let (p0, o0) = rows(0, vec![
            StoredRow { kind: RowKind::Different, source: vec![Value::Int(1), s("Alice O'Neil"), s("a@x")], target: vec![Value::Int(1), s("Alice"), s("a@x")], differs: vec![false, true, false] },
            StoredRow { kind: RowKind::OnlyIn1, source: vec![Value::Int(5), s("Bob"), s("bob@example.com")], target: vec![], differs: vec![] },
        ]);
        let (p1, o1) = rows(1, vec![
            StoredRow { kind: RowKind::OnlyIn2, source: vec![], target: vec![Value::Int(3), Value::Int(1), Value::Decimal(10000, 4), Value::Null], differs: vec![] },
            StoredRow {
                kind: RowKind::OnlyIn1,
                source: vec![Value::Int(10), Value::Int(5), Value::Decimal(999500, 4), Value::DateTime(739_920 * 86_400_000_000_000 + 9 * 3_600_000_000_000 + 30 * 60_000_000_000)],
                target: vec![],
                differs: vec![],
            },
        ]);
        let result = |pair: TablePair, path, offsets, counts, diffs| TableResult {
            pair,
            counts,
            offsets,
            path,
            error: None,
            by_checksum: false,
            repeated_keys: false,
            identity_current: (None, None),
            column_differences: diffs,
        };
        let mut pairs = pairs.into_iter();
        let tables = vec![
            result(pairs.next().unwrap(), p0, o0, [0, 1, 1, 0], vec![0, 1, 0]),
            result(pairs.next().unwrap(), p1, o1, [0, 0, 1, 1], vec![0, 0, 0, 0]),
        ];
        (DataComparison { session, tables, only_in_1: vec![], only_in_2: vec![], ddl_triggers: vec![], excluded: vec![], kinds: Default::default(), masked_reads: false, old_server: false }, db)
    }

    #[test]
    fn script_layout() {
        let (result, db) = example();
        let settings = SyncSettings::new("DEV.Shop", "PROD.Shop", "Shop");
        let script = generate(&result, &db, &DataOptions::default_set(), &settings).unwrap();
        let header_end = script.sql.find("*/\r\n\t\t\r\n").expect("header ends with two tabs") + 8;
        assert!(script.sql.starts_with("/*\r\nRun this script on:\r\n\r\nPROD.Shop    -  This database will be modified\r\n\r\nto synchronize it with:\r\n\r\nDEV.Shop\r\n"));
        let body = &script.sql[header_end..];
        let expected = "SET NUMERIC_ROUNDABORT OFF\r\nGO\r\n\
SET ANSI_PADDING, ANSI_WARNINGS, CONCAT_NULL_YIELDS_NULL, ARITHABORT, QUOTED_IDENTIFIER, ANSI_NULLS, NOCOUNT ON\r\nGO\r\n\
SET DATEFORMAT YMD\r\nGO\r\nSET XACT_ABORT ON\r\nGO\r\nSET TRANSACTION ISOLATION LEVEL SERIALIZABLE\r\nGO\r\nBEGIN TRANSACTION\r\n\
\r\nPRINT(N'Drop constraints from [dbo].[Orders]')\r\n\
ALTER TABLE [dbo].[Orders] NOCHECK CONSTRAINT [FK_Orders_Customers]\r\n\
\r\nPRINT(N'Delete 1 row from [dbo].[Orders]')\r\n\
DELETE FROM [dbo].[Orders] WHERE [OrderID] = 3\r\n\
\r\nPRINT(N'Update 1 row in [dbo].[Customers]')\r\n\
UPDATE [dbo].[Customers] SET [Name]=N'Alice O''Neil' WHERE [CustomerID] = 1\r\n\
\r\nPRINT(N'Add 1 row to [dbo].[Customers]')\r\n\
SET IDENTITY_INSERT [dbo].[Customers] ON\r\n\
INSERT INTO [dbo].[Customers] ([CustomerID], [Name], [Email]) VALUES (5, N'Bob', 'bob@example.com')\r\n\
SET IDENTITY_INSERT [dbo].[Customers] OFF\r\n\
\r\nPRINT(N'Add 1 row to [dbo].[Orders]')\r\n\
SET IDENTITY_INSERT [dbo].[Orders] ON\r\n\
INSERT INTO [dbo].[Orders] ([OrderID], [CustomerID], [Amount], [OrderDate]) VALUES (10, 5, 99.9500, '2026-11-01 09:30:00.000')\r\n\
SET IDENTITY_INSERT [dbo].[Orders] OFF\r\n\
\r\nPRINT(N'Add constraints to [dbo].[Orders]')\r\n\
ALTER TABLE [dbo].[Orders] WITH CHECK CHECK CONSTRAINT [FK_Orders_Customers]\r\n\
COMMIT TRANSACTION\r\nGO\r\n";
        assert_eq!(body, expected);
        assert!(script.warnings.is_empty(), "{:?}", script.warnings);

        // No comments; drop instead of NOCHECK; tiny transactions.
        let opts = DataOptions::default_set().with(DataOpt::DontOutputComments).with(DataOpt::DropConstraintsAndIndexes);
        let split = SyncSettings { max_transaction_bytes: Some(1), ..SyncSettings::new("a.b", "c.d", "d") };
        let script = generate(&result, &db, &opts, &split).unwrap();
        assert!(!script.sql.contains("PRINT"));
        assert!(script.sql.contains("ALTER TABLE [dbo].[Orders] DROP CONSTRAINT [FK_Orders_Customers]\r\n"));
        assert!(script.sql.contains("ALTER TABLE [dbo].[Orders] ADD CONSTRAINT [FK_Orders_Customers] FOREIGN KEY ([CustomerID]) REFERENCES [dbo].[Customers] ([CustomerID])"));
        assert!(script.sql.contains("DELETE FROM [dbo].[Orders] WHERE [OrderID] = 3\r\nCOMMIT TRANSACTION\r\nGO\r\nBEGIN TRANSACTION\r\n"));
        assert!(script.warnings.iter().any(|w| w.message == "The resulting script contains multiple transactions and cannot be rolled back in entirety"));

        // Row-level selection: only the insert into Orders; the messages carry no counts up front.
        let mut sel = [RowSelection::default(), RowSelection::default(), RowSelection::default(), RowSelection::default()];
        sel[RowKind::OnlyIn2 as usize].included = false;
        let mut only = SyncSettings::new("a.b", "c.d", "d");
        only.selections.insert(ObjectName::new("dbo", "Orders"), sel);
        only.tables = Some(vec![ObjectName::new("dbo", "Orders")]);
        let script = generate(&result, &db, &DataOptions::default_set(), &only).unwrap();
        assert!(!script.sql.contains("DELETE"));
        assert!(script.sql.contains("PRINT(N'Add row to [dbo].[Orders]')"), "{}", script.sql);
        assert!(!script.sql.contains("[dbo].[Customers] ("));
    }

    #[test]
    fn headers_and_warnings() {
        let (mut result, db) = example();
        let settings = SyncSettings::new("DEV.Shop", "PROD.Shop", "Shop");
        result.kinds = [SideKind::Folder, SideKind::Live];
        let script = generate(&result, &db, &DataOptions::default_set(), &settings).unwrap();
        assert!(script.sql.starts_with("/*\r\nRun this script on:\r\n\r\nPROD.Shop \u{2013} this database will be modified\r\n\r\nto synchronize its data with:\r\n\r\nDEV.Shop \u{2013} this scripts folder will not be modified\r\n"));
        result.kinds = [SideKind::Live, SideKind::Folder];
        let script = generate(&result, &db, &DataOptions::default_set(), &settings).unwrap();
        assert!(script.sql.contains("DEV.Shop \u{2013} this database will not be modified\r\n"));
        assert!(script.sql.contains("*/\r\n\t\t\t"));

        result.kinds = [SideKind::Live, SideKind::Backup];
        result.old_server = true;
        result.masked_reads = true;
        result.tables[0].pair.columns[1].masked = true;
        result.tables[0].repeated_keys = true;
        let script = generate(&result, &db, &DataOptions::default_set(), &settings).unwrap();
        let has = |sev: Severity, text: &str| script.warnings.iter().any(|w| w.severity == sev && w.message == text);
        assert!(has(Severity::High, "When the target data source is a backup, the SQL script does not take into account dependencies or foreign keys. If the script references multiple tables you are not recommended to run it against a live database."));
        assert!(has(Severity::Low, "A future version of SQL Compare and SQL Data Compare will no longer support SQL Server 2008 and SQL Server 2008R2."));
        assert!(has(Severity::High, "Column Name of table [dbo].[Customers] has a data mask applied and the current user does not have UNMASK permission. The deployment may contain invalid data."));
        assert!(has(Severity::High, "Update 1 row in [dbo].[Customers] with non-unique comparison key"), "{:?}", script.warnings);
        assert!(has(Severity::High, "Add 1 row to [dbo].[Customers] with non-unique comparison key"));

        // Only deployed tables are reseeded.
        let (mut result, db) = example();
        for t in &mut result.tables {
            t.identity_current = (Some(10), Some(3));
        }
        let mut only = SyncSettings::new("a.b", "c.d", "d");
        only.tables = Some(vec![ObjectName::new("dbo", "Customers")]);
        let script = generate(&result, &db, &DataOptions::default_set().with(DataOpt::ReseedIdentity), &only).unwrap();
        assert!(script.sql.contains("DBCC CHECKIDENT(N'[dbo].[Customers]', RESEED, 10)"));
        assert!(!script.sql.contains("[dbo].[Orders]', RESEED"), "{}", script.sql);
    }

    #[test]
    fn drop_constraints_and_indexes() {
        use crate::catalog::{ExtendedProperty, XmlIndex, XmlIndexKind};
        let (result, mut db) = example();
        let c = &mut db.tables[0];
        c.indexes[0].kind = IndexKind::Nonclustered;
        let mut g = col("G", "uniqueidentifier");
        g.rowguidcol = true;
        g.nullable = false;
        let mut f = col("F", "varbinary");
        f.filestream = true;
        c.columns.extend([g, f, col("X", "xml")]);
        let ix = |name: &str, kind: IndexKind, column: &str| Index { name: name.into(), kind, columns: vec![IndexColumn { name: column.into(), descending: false }], ..Default::default() };
        c.indexes.push(Index { unique_constraint: true, unique: true, ..ix("UQ_G", IndexKind::Nonclustered, "G") });
        c.indexes.push(ix("CX_Name", IndexKind::Clustered, "Name"));
        c.indexes.push(Index { xml: Some(XmlIndex::default()), ..ix("PX", IndexKind::Xml, "X") });
        c.indexes.push(Index {
            xml: Some(XmlIndex { kind: XmlIndexKind::Secondary, secondary_type: Some("PATH".into()), using_index: Some("PX".into()), ..Default::default() }),
            ..ix("SX", IndexKind::Xml, "X")
        });
        db.extended_properties.push(ExtendedProperty {
            class: "TABLE".into(),
            schema: Some("dbo".into()),
            object: Some("Orders".into()),
            minor_type: Some("CONSTRAINT".into()),
            minor_name: Some("FK_Orders_Customers".into()),
            name: "MS_Description".into(),
            value: Some("to customers".into()),
        });
        let opts = DataOptions::default_set().with(DataOpt::DropConstraintsAndIndexes);
        let sql = generate(&result, &db, &opts, &SyncSettings::new("a.b", "c.d", "d")).unwrap().sql;
        assert!(!sql.contains("UQ_G"), "the FILESTREAM ROWGUIDCOL index stays: {sql}");
        let at = |s: &str| sql.find(s).unwrap_or_else(|| panic!("{s} missing from {sql}"));
        assert!(at("DROP INDEX [PX] ON [dbo].[Customers]") < at("DROP INDEX [CX_Name] ON [dbo].[Customers]"));
        assert!(!sql.contains("DROP INDEX [SX]"), "secondary XML indexes go with the primary one");
        let create_cx = at("CREATE CLUSTERED INDEX [CX_Name]");
        let create_px = at("CREATE PRIMARY XML INDEX [PX]");
        assert!(create_cx < create_px && create_px < at("XML INDEX [SX]"));
        assert!(at("ADD CONSTRAINT [FK_Orders_Customers]") < at("EXEC sp_addextendedproperty N'MS_Description', N'to customers', 'SCHEMA', N'dbo', 'TABLE', N'Orders', 'CONSTRAINT', N'FK_Orders_Customers'"));
    }

    #[test]
    fn cycles_are_reported() {
        let fk = |name: &str, to: &str| ForeignKey { name: name.into(), columns: vec!["R".into()], referenced_table: ObjectName::new("dbo", to), referenced_columns: vec!["Id".into()], ..Default::default() };
        let a = table("A", vec![col("Id", "int"), col("R", "int")], vec![fk("FK_A_B", "B")]);
        let b = table("B", vec![col("Id", "int"), col("R", "int")], vec![fk("FK_B_A", "A")]);
        let db = Database { tables: vec![a, b], ..Default::default() };
        let (pairs, _, _) = map_tables(&db, &db, &DataOptions::default_set(), &DataMappings::default());
        let session = Session::create().unwrap();
        let tables: Vec<TableResult> = pairs
            .into_iter()
            .enumerate()
            .map(|(i, pair)| {
                let path = session.table_path(i);
                let mut w = TableWriter::create(&path, false).unwrap();
                w.write(&StoredRow { kind: RowKind::OnlyIn1, source: vec![Value::Int(1), Value::Null], target: vec![], differs: vec![] }).unwrap();
                TableResult {
                    pair,
                    counts: [0, 0, 1, 0],
                    offsets: w.finish().unwrap(),
                    path,
                    error: None,
                    by_checksum: false,
                    repeated_keys: false,
                    identity_current: (None, None),
                    column_differences: vec![],
                }
            })
            .collect();
        let result = DataComparison { session, tables, only_in_1: vec![], only_in_2: vec![], ddl_triggers: vec![], excluded: vec![], kinds: Default::default(), masked_reads: false, old_server: false };
        let script = generate(&result, &db, &DataOptions::default_set(), &SyncSettings::new("a.b", "c.d", "d")).unwrap();
        let mut circular: Vec<&str> = script.warnings.iter().map(|w| w.message.as_str()).filter(|m| m.starts_with("Circular")).collect();
        circular.sort();
        assert_eq!(circular, ["Circular dependency for the table or view [dbo].[A]", "Circular dependency for the table or view [dbo].[B]"]);
        assert_eq!(script.sql.matches("INSERT INTO").count(), 2);
    }

    #[test]
    fn where_literals_by_side() {
        let sized = |n: &str, l: i32| crate::catalog::DataType { name: n.into(), length: Some(l), ..Default::default() };
        let c = ColumnPair {
            source: "K".into(),
            target: "K".into(),
            source_type: sized("varchar", 20),
            target_type: sized("varchar", 10),
            read_only: false,
            identity: false,
            case_insensitive: true,
            nullable: false,
            source_collation: Some("Japanese_CI_AS".into()),
            target_collation: Some("Latin1_General_CI_AS".into()),
            source_identity: false,
            graph: None,
            masked: false,
        };
        let p = TablePair {
            source: ObjectName::new("dbo", "T"),
            target: ObjectName::new("dbo", "T"),
            columns: vec![c],
            key: vec![0],
            key_name: None,
            target_key_name: None,
            where1: None,
            where2: None,
            problem: None,
            is_view: false,
            notes: vec![],
            excluded: None,
            target_missing: false,
        };
        let v = [Value::Str("a".into())];
        let mut w = Vec::new();
        assert_eq!(where_sql(&p, false, &v, false, &mut w), "[K] = cast(N'a' COLLATE Latin1_General_CI_AS as varchar(10))");
        assert_eq!(where_sql(&p, false, &v, true, &mut w), "[K] = 'a'", "an updated row: the target column decides");
    }

    #[test]
    fn filters() {
        assert_eq!(strip_filter("WHERE (a = 1)"), "a = 1");
        assert_eq!(strip_filter("(a = 1) OR (b = 2)"), "(a = 1) OR (b = 2)");
    }
}
