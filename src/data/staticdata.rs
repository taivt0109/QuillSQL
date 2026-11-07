//! Static data for `quill schema /include:StaticData`: the rows of the tables that have a data
//! script in database 1's scripts folder are compared with database 2 and deployed with the
//! schema, as SQL Compare does.
//!
//! The data options start from SQL Data Compare's defaults without the comment header and
//! transactions (the schema script has its own) and without timestamp columns; tables the
//! schema deployment creates get all their rows inserted. The data script goes inside the
//! schema script's transaction, just before its COMMIT.

use super::compare::{self, DataComparison, Memory, RowSource};
use super::mapping::DataMappings;
use super::options::{DataOpt, DataOptions};
use super::store::{RowKind, Session};
use super::sync;
use crate::catalog::{Database, ObjectName};
use crate::db::ConnectionSpec;
use crate::schema::deploy::DeploymentScript;
use crate::sources::datafolder::DataFolder;

/// Where database 2's rows come from.
pub enum Target<'a> {
    Live(&'a ConnectionSpec),
    Folder(&'a DataFolder),
}

pub struct StaticData {
    pub result: DataComparison,
    pub options: DataOptions,
    /// The results of the tables only in database 1 live here.
    _created: Option<Session>,
}

/// The data options static data starts from, before `/datacompareoptions`.
pub fn default_options() -> DataOptions {
    DataOptions::default_set().without(DataOpt::OutputCommentHeader).with(DataOpt::DontUseTransactions).without(DataOpt::IncludeTimestamps)
}

fn has_file(folder: &DataFolder, n: &ObjectName) -> bool {
    folder.files.iter().any(|(f, _)| f.schema.eq_ignore_ascii_case(&n.schema) && f.name.eq_ignore_ascii_case(&n.name))
}

/// Compares the data of the folder's tables that have a data script with database 2.
pub fn compare(folder: &DataFolder, target: Target, db2: &Database, options: &DataOptions) -> Result<StaticData, String> {
    let db1 = &folder.folder.database;
    let options = options.clone().without(DataOpt::IncludeViews);
    let side2 = match &target {
        Target::Live(spec) => RowSource::Live(spec),
        Target::Folder(f) => RowSource::Memory(&f.memory),
    };
    let none = DataMappings::default();
    let mut result = compare::compare_sources(RowSource::Memory(&folder.memory), side2, db1, db2, &options, &none, |p| !p.is_view && has_file(folder, &p.source), 4, |_, _| {})?;
    // Tables the schema deployment creates: every row is added.
    let mut created = None;
    let new: Vec<ObjectName> = result.only_in_1.iter().filter(|n| has_file(folder, n)).cloned().collect();
    if !new.is_empty() {
        let mut shape = db1.clone();
        shape.tables.retain(|t| new.contains(&t.object_name()));
        let empty = Memory::new();
        let more = compare::compare_sources(RowSource::Memory(&folder.memory), RowSource::Memory(&empty), &shape, &shape, &options, &none, |p| !p.is_view, 4, |_, _| {})?;
        result.tables.extend(more.tables);
        result.tables.sort_by(|a, b| a.pair.source.cmp(&b.pair.source));
        created = Some(more.session);
    }
    Ok(StaticData { result, options, _created: created })
}

impl StaticData {
    pub fn identical(&self) -> bool {
        self.result.tables.iter().all(|t| t.error.is_some() || t.identical())
    }

    /// The summary rows ("Table data") with SQL Compare's static-data widths: type 16, name
    /// `name_width`, records 8.
    pub fn summary_rows(&self, name_width: usize) -> Vec<String> {
        let identical = self.options.has(DataOpt::IncludeIdenticalRecords);
        let mut out = Vec::new();
        for t in self.result.tables.iter().filter(|t| t.error.is_none()) {
            let marks = [(RowKind::Identical, "==  ==  "), (RowKind::Different, "<>  <>  "), (RowKind::OnlyIn1, ">>      "), (RowKind::OnlyIn2, "    <<  ")];
            for (kind, mark) in marks {
                let n = t.count(kind);
                if n == 0 || (kind == RowKind::Identical && !identical) {
                    continue;
                }
                let name = t.pair.source.to_string();
                let shown: String = if name.chars().count() >= name_width { name.chars().take(name_width - 1).collect() } else { name };
                out.push(format!("{:<16}{shown:<name_width$}{n:>6}  {mark}", "Table data"));
            }
        }
        out
    }

    /// The data script, to go inside the schema script's transaction (or, with `transaction`,
    /// in one of its own).
    pub fn script(&self, db2: &Database, source: &str, target: &str, isolation: &str, transaction: bool) -> Result<DeploymentScript, String> {
        let mut settings = sync::SyncSettings::new(source, target, &db2.name);
        settings.transaction_isolation_level = isolation.to_ascii_uppercase();
        let options = if transaction { self.options.clone().without(DataOpt::DontUseTransactions) } else { self.options.clone() };
        sync::generate(&self.result, db2, &options, &settings)
    }
}

/// The schema script with the data script inside its transaction (before its COMMIT), or the
/// data script alone (with a transaction of its own) when the schema has nothing to deploy.
pub fn merge(schema: DeploymentScript, data: impl FnOnce(bool) -> Result<DeploymentScript, String>) -> Result<DeploymentScript, String> {
    if schema.is_empty() {
        let d = data(true)?;
        let sql = if d.is_empty() { schema.sql } else { d.sql };
        let mut warnings = schema.warnings;
        warnings.extend(d.warnings);
        return Ok(DeploymentScript { sql, warnings, batches: d.batches });
    }
    let d = data(false)?;
    if d.is_empty() {
        return Ok(schema);
    }
    let mut out = schema;
    let at = out.batches.iter().position(|b| b.trim() == "COMMIT TRANSACTION").unwrap_or(out.batches.len());
    out.batches.splice(at..at, d.batches);
    let marker = "COMMIT TRANSACTION\r\nGO\r\n";
    let body = if d.sql.ends_with("GO\r\n") { d.sql.clone() } else { format!("{}GO\r\n", d.sql) };
    match out.sql.find(marker) {
        Some(i) => out.sql.insert_str(i, &body),
        None => out.sql.push_str(&body),
    }
    out.warnings.extend(d.warnings);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_goes_before_the_commit() {
        let script = |sql: &str, batches: &[&str]| DeploymentScript { sql: sql.into(), warnings: vec![], batches: batches.iter().map(|b| b.to_string()).collect() };
        let schema = script("BEGIN TRANSACTION\r\nGO\r\nALTER TABLE x\r\nGO\r\nCOMMIT TRANSACTION\r\nGO\r\n", &["BEGIN TRANSACTION", "ALTER TABLE x", "COMMIT TRANSACTION"]);
        let merged = merge(schema, |_| Ok(script("INSERT y\r\nGO\r\n", &["INSERT y"]))).unwrap();
        assert_eq!(merged.batches, ["BEGIN TRANSACTION", "ALTER TABLE x", "INSERT y", "COMMIT TRANSACTION"]);
        assert_eq!(merged.sql, "BEGIN TRANSACTION\r\nGO\r\nALTER TABLE x\r\nGO\r\nINSERT y\r\nGO\r\nCOMMIT TRANSACTION\r\nGO\r\n");
        let alone = merge(script("-- nothing\r\n", &[]), |own| {
            assert!(own, "the data script brings its own transaction");
            Ok(script("BEGIN TRANSACTION\r\nINSERT y\r\nCOMMIT TRANSACTION\r\nGO\r\n", &["BEGIN TRANSACTION\r\nINSERT y\r\nCOMMIT TRANSACTION"]))
        })
        .unwrap();
        assert_eq!(alone.batches.len(), 1);
    }
}
