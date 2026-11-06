//! Runs a data comparison: for each table pair, both databases are read in key order at the
//! same time, the two streams are merged on the key, and the rows that differ are stored.

use std::cmp::Ordering;
use std::path::PathBuf;

use futures_util::{StreamExt, TryStreamExt};
use serde::Serialize;
use tokio::sync::mpsc;

use super::mapping::{self, DataMappings, TablePair};
use super::options::{DataOpt, DataOptions};
use super::store::{RowKind, Session, StoredRow, TableWriter};
use super::value::{Equality, Value, cmp_key, same};
use crate::catalog::{DataType, Database, ObjectName};
use crate::db::{self, ConnectionSpec, quote};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableResult {
    pub pair: TablePair,
    /// Rows per kind, in `RowKind::ALL` order.
    pub counts: [u64; 4],
    #[serde(skip)]
    pub offsets: [Vec<u64>; 4],
    #[serde(skip)]
    pub path: PathBuf,
    pub error: Option<String>,
    /// Decided by checksum without reading rows.
    pub by_checksum: bool,
    /// The key is not unique in the data on one side.
    pub repeated_keys: bool,
}

impl TableResult {
    pub fn count(&self, k: RowKind) -> u64 {
        self.counts[k as usize]
    }

    pub fn identical(&self) -> bool {
        self.error.is_none() && self.count(RowKind::Different) + self.count(RowKind::OnlyIn1) + self.count(RowKind::OnlyIn2) == 0
    }
}

pub struct DataComparison {
    pub session: Session,
    pub tables: Vec<TableResult>,
    pub only_in_1: Vec<ObjectName>,
    pub only_in_2: Vec<ObjectName>,
}

impl DataComparison {
    pub fn identical(&self) -> bool {
        self.tables.iter().all(TableResult::identical)
    }
}

/// Which pairs to compare; `include` decides by pair.
#[allow(clippy::too_many_arguments)]
pub fn compare(
    spec1: &ConnectionSpec,
    spec2: &ConnectionSpec,
    db1: &Database,
    db2: &Database,
    opts: &DataOptions,
    user: &DataMappings,
    include: impl Fn(&TablePair) -> bool,
    parallel: usize,
    progress: impl Fn(&TablePair, &TableResult),
) -> Result<DataComparison, String> {
    let (pairs, only_in_1, only_in_2) = mapping::map_tables(db1, db2, opts, user);
    let pairs: Vec<TablePair> = pairs.into_iter().filter(|p| include(p)).collect();
    let session = Session::create().map_err(|e| format!("cannot create the results folder: {e}"))?;
    let rt = db::runtime()?;
    let tables = rt.block_on(async {
        let jobs = pairs.into_iter().enumerate().map(|(i, pair)| {
            let path = session.table_path(i);
            async move { compare_table(spec1, spec2, pair, opts, path).await }
        });
        let mut done: Vec<TableResult> = futures_util::stream::iter(jobs).buffered(parallel.max(1)).collect().await;
        for r in &done {
            progress(&r.pair, r);
        }
        done.sort_by(|a, b| a.pair.source.cmp(&b.pair.source));
        done
    });
    Ok(DataComparison { session, tables, only_in_1, only_in_2 })
}

async fn compare_table(spec1: &ConnectionSpec, spec2: &ConnectionSpec, pair: TablePair, opts: &DataOptions, path: PathBuf) -> TableResult {
    let mut result = TableResult {
        pair,
        counts: [0; 4],
        offsets: Default::default(),
        path,
        error: None,
        by_checksum: false,
        repeated_keys: false,
    };
    if let Some(p) = &result.pair.problem {
        result.error = Some(p.clone());
        return result;
    }
    if let Err(e) = run(spec1, spec2, opts, &mut result).await {
        result.error = Some(e);
    }
    result
}

/// The expression a key column is ordered and matched by. Strings are compared after trailing
/// spaces are removed (as SQL Server does) and, for case-insensitive columns, upper-cased; both
/// servers order them by code point so the client can merge them exactly.
fn sort_expr(column: &str, t: &DataType, case_insensitive: bool) -> String {
    let c = quote(column);
    if matches!(t.name.as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "sysname") {
        let mut e = format!("RTRIM(CAST({c} AS nvarchar(4000)))");
        if case_insensitive {
            e = format!("UPPER({e})");
        }
        format!("{e} COLLATE Latin1_General_100_BIN2")
    } else {
        c
    }
}

fn value_expr(column: &str, t: &DataType) -> String {
    let c = quote(column);
    if t.schema.is_some() || matches!(t.name.as_str(), "hierarchyid" | "geography" | "geometry") {
        format!("CAST({c} AS varbinary(max))")
    } else {
        c
    }
}

/// ` WHERE <filter>` from a user filter written with or without the WHERE keyword.
fn where_clause(filter: &Option<String>) -> String {
    let Some(w) = filter.as_deref().map(str::trim).filter(|w| !w.is_empty()) else { return String::new() };
    let w = match w.get(..5) {
        Some(head) if head.eq_ignore_ascii_case("where") && w[5..].starts_with(char::is_whitespace) => w[5..].trim(),
        _ => w,
    };
    format!(" WHERE {w}")
}

fn select_sql(pair: &TablePair, side: u8, opts: &DataOptions) -> String {
    let table = if side == 1 { pair.source.quoted() } else { pair.target.quoted() };
    let name = |i: usize| if side == 1 { &pair.columns[i].source } else { &pair.columns[i].target };
    let ty = |i: usize| if side == 1 { &pair.columns[i].source_type } else { &pair.columns[i].target_type };
    let ci = |i: usize| pair.columns[i].case_insensitive && !opts.has(DataOpt::ForceBinaryCollation);
    let mut list: Vec<String> = pair.key.iter().map(|&k| sort_expr(name(k), ty(k), ci(k))).collect();
    list.extend((0..pair.columns.len()).map(|i| value_expr(name(i), ty(i))));
    let filter = where_clause(if side == 1 { &pair.where1 } else { &pair.where2 });
    let order: Vec<String> = (1..=pair.key.len()).map(|i| i.to_string()).collect();
    format!("SELECT {} FROM {table} WITH (NOLOCK){filter} ORDER BY {}", list.join(", "), order.join(", "))
}

/// Types BINARY_CHECKSUM skips, which would make a checksum miss differences.
fn checksum_blind(t: &DataType) -> bool {
    t.schema.is_some() || matches!(t.name.as_str(), "text" | "ntext" | "image" | "xml" | "hierarchyid" | "geography" | "geometry" | "sql_variant")
}

async fn checksum(conn: &mut db::Connection, pair: &TablePair, side: u8) -> Result<(i64, i64), String> {
    let table = if side == 1 { pair.source.quoted() } else { pair.target.quoted() };
    let cols: Vec<String> =
        pair.columns.iter().map(|c| quote(if side == 1 { &c.source } else { &c.target })).collect();
    let filter = where_clause(if side == 1 { &pair.where1 } else { &pair.where2 });
    let rows = db::query(
        conn,
        &format!("SELECT COUNT_BIG(*), ISNULL(CHECKSUM_AGG(BINARY_CHECKSUM({})), 0) FROM {table} WITH (NOLOCK){filter}", cols.join(", ")),
    )
    .await?;
    let r = rows.first().ok_or("no checksum returned")?;
    let n = match Value::read(r, 0) {
        Value::Int(n) => n,
        _ => 0,
    };
    let c = match Value::read(r, 1) {
        Value::Int(n) => n,
        _ => 0,
    };
    Ok((n, c))
}

const CHUNK: usize = 512;

async fn produce(spec: &ConnectionSpec, sql: String, tx: mpsc::Sender<Result<Vec<Vec<Value>>, String>>) {
    let run = async {
        let mut conn = db::connect_async(spec).await?;
        db::execute(&mut conn, "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED").await?;
        let mut stream = conn.simple_query(&sql).await.map_err(|e| e.to_string())?.into_row_stream();
        let mut chunk = Vec::with_capacity(CHUNK);
        while let Some(row) = stream.try_next().await.map_err(|e| e.to_string())? {
            let width = row.len();
            chunk.push((0..width).map(|i| Value::read(&row, i)).collect());
            if chunk.len() == CHUNK && tx.send(Ok(std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK)))).await.is_err() {
                return Ok(());
            }
        }
        if !chunk.is_empty() {
            let _ = tx.send(Ok(chunk)).await;
        }
        Ok::<(), String>(())
    };
    if let Err(e) = run.await {
        let _ = tx.send(Err(e)).await;
    }
}

/// Pulls rows one at a time from a producer.
struct Rows {
    rx: mpsc::Receiver<Result<Vec<Vec<Value>>, String>>,
    buf: std::vec::IntoIter<Vec<Value>>,
}

impl Rows {
    async fn next(&mut self) -> Result<Option<Vec<Value>>, String> {
        loop {
            if let Some(r) = self.buf.next() {
                return Ok(Some(r));
            }
            match self.rx.recv().await {
                Some(Ok(chunk)) => self.buf = chunk.into_iter(),
                Some(Err(e)) => return Err(e),
                None => return Ok(None),
            }
        }
    }
}

fn cmp_keys(a: &[Value], b: &[Value], n: usize) -> Ordering {
    (0..n).map(|i| cmp_key(&a[i], &b[i])).find(|o| o.is_ne()).unwrap_or(Ordering::Equal)
}

async fn run(spec1: &ConnectionSpec, spec2: &ConnectionSpec, opts: &DataOptions, result: &mut TableResult) -> Result<(), String> {
    let pair = &result.pair;
    let keys = pair.key.len();
    let width = pair.columns.len();
    if opts.has(DataOpt::UseChecksumComparison) && !pair.columns.iter().any(|c| checksum_blind(&c.source_type) || checksum_blind(&c.target_type)) {
        let (mut c1, mut c2) = tokio::try_join!(db::connect_async(spec1), db::connect_async(spec2))?;
        let (a, b) = tokio::try_join!(checksum(&mut c1, pair, 1), checksum(&mut c2, pair, 2))?;
        if a == b {
            result.counts[RowKind::Identical as usize] = a.0 as u64;
            result.by_checksum = true;
            return Ok(());
        }
    }
    let (tx1, rx1) = mpsc::channel(4);
    let (tx2, rx2) = mpsc::channel(4);
    let p1 = produce(spec1, select_sql(pair, 1, opts), tx1);
    let p2 = produce(spec2, select_sql(pair, 2, opts), tx2);
    let mut writer = TableWriter::create(&result.path).map_err(|e| e.to_string())?;
    let store_identical = opts.has(DataOpt::IncludeIdenticalRecords);
    let eq: Vec<Equality> = pair
        .columns
        .iter()
        .map(|c| Equality {
            binary: opts.has(DataOpt::ForceBinaryCollation),
            case_insensitive_column: c.case_insensitive,
            trim_trailing_spaces: opts.has(DataOpt::TrimTrailingSpaces),
            empty_is_null: opts.has(DataOpt::TreatEmptyStringAsNull),
            full_precision: opts.has(DataOpt::UseFullPrecisionForComparison),
        })
        .collect();
    let mut counts = [0u64; 4];
    let mut repeated = false;
    let consume = async {
        let mut r1 = Rows { rx: rx1, buf: Vec::new().into_iter() };
        let mut r2 = Rows { rx: rx2, buf: Vec::new().into_iter() };
        let (mut a, mut b) = (r1.next().await?, r2.next().await?);
        let (mut last1, mut last2): (Option<Vec<Value>>, Option<Vec<Value>>) = (None, None);
        let check = |last: &Option<Vec<Value>>, cur: &Vec<Value>, side: u8, repeated: &mut bool| -> Result<(), String> {
            if let Some(prev) = last {
                match cmp_keys(prev, cur, keys) {
                    Ordering::Greater => {
                        return Err(format!(
                            "database {side} returned rows out of key order; the key's collation or type cannot be compared safely (choose other key columns)"
                        ));
                    }
                    Ordering::Equal => *repeated = true,
                    Ordering::Less => {}
                }
            }
            Ok(())
        };
        loop {
            let order = match (&a, &b) {
                (None, None) => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(x), Some(y)) => cmp_keys(x, y, keys),
            };
            match order {
                Ordering::Less => {
                    let x = a.take().unwrap();
                    check(&last1, &x, 1, &mut repeated)?;
                    let row = StoredRow { kind: RowKind::OnlyIn1, source: x[keys..].to_vec(), target: Vec::new(), differs: Vec::new() };
                    writer.write(&row).map_err(|e| e.to_string())?;
                    counts[RowKind::OnlyIn1 as usize] += 1;
                    last1 = Some(x);
                    a = r1.next().await?;
                }
                Ordering::Greater => {
                    let y = b.take().unwrap();
                    check(&last2, &y, 2, &mut repeated)?;
                    let row = StoredRow { kind: RowKind::OnlyIn2, source: Vec::new(), target: y[keys..].to_vec(), differs: Vec::new() };
                    writer.write(&row).map_err(|e| e.to_string())?;
                    counts[RowKind::OnlyIn2 as usize] += 1;
                    last2 = Some(y);
                    b = r2.next().await?;
                }
                Ordering::Equal => {
                    let (x, y) = (a.take().unwrap(), b.take().unwrap());
                    check(&last1, &x, 1, &mut repeated)?;
                    check(&last2, &y, 2, &mut repeated)?;
                    let differs: Vec<bool> = (0..width).map(|i| !same(&x[keys + i], &y[keys + i], eq[i])).collect();
                    let kind = if differs.iter().any(|d| *d) { RowKind::Different } else { RowKind::Identical };
                    counts[kind as usize] += 1;
                    if kind == RowKind::Different || store_identical {
                        let row = StoredRow { kind, source: x[keys..].to_vec(), target: y[keys..].to_vec(), differs };
                        writer.write(&row).map_err(|e| e.to_string())?;
                    }
                    last1 = Some(x);
                    last2 = Some(y);
                    a = r1.next().await?;
                    b = r2.next().await?;
                }
            }
        }
        Ok::<(), String>(())
    };
    let ((), (), merged) = tokio::join!(p1, p2, consume);
    merged?;
    result.offsets = writer.finish().map_err(|e| e.to_string())?;
    result.counts = counts;
    result.repeated_keys = repeated;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::mapping::ColumnPair;

    #[test]
    fn select_statements() {
        let col = |n: &str, t: &str, ci: bool| ColumnPair {
            source: n.into(),
            target: n.into(),
            source_type: DataType { name: t.into(), ..Default::default() },
            target_type: DataType { name: t.into(), ..Default::default() },
            read_only: false,
            identity: false,
            case_insensitive: ci,
            nullable: true,
        };
        let pair = TablePair {
            source: ObjectName::new("dbo", "T"),
            target: ObjectName::new("dbo", "T"),
            columns: vec![col("Code", "varchar", true), col("Id", "int", false), col("Name", "nvarchar", true)],
            key: vec![1, 0],
            key_name: Some("PK".into()),
            where1: Some("WHERE Id > 5".into()),
            where2: None,
            problem: None,
            is_view: false,
        };
        assert_eq!(
            select_sql(&pair, 1, &DataOptions::default_set()),
            "SELECT [Id], UPPER(RTRIM(CAST([Code] AS nvarchar(4000)))) COLLATE Latin1_General_100_BIN2, [Code], [Id], [Name] FROM [dbo].[T] WITH (NOLOCK) WHERE Id > 5 ORDER BY 1, 2"
        );
        let binary = DataOptions::default_set().with(DataOpt::ForceBinaryCollation);
        assert!(select_sql(&pair, 2, &binary).contains("RTRIM(CAST([Code] AS nvarchar(4000))) COLLATE"));
    }
}
