//! Runs a data comparison: for each table pair, both databases are read in key order at the
//! same time, the two streams are merged on the key, and the rows that differ are stored.

use std::cmp::Ordering;
use std::path::PathBuf;

use futures_util::{StreamExt, TryStreamExt};
use serde::Serialize;
use tokio::sync::mpsc;

use super::mapping::{self, ColumnPair, DataMappings, TablePair};
use super::options::{DataOpt, DataOptions};
use super::store::{RowKind, Session, StoredRow, TableWriter};
use super::value::{Equality, Fold, Value, Variant, cmp_key, same};
use crate::catalog::{DataType, Database, ObjectName};
use crate::db::{self, ConnectionSpec, literal, quote};

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
    /// IDENT_CURRENT of each side, read for "Reseed identity columns".
    pub identity_current: (Option<i128>, Option<i128>),
    /// Per compared column, the number of Different rows in which it differs.
    pub column_differences: Vec<u64>,
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
    /// Enabled database DDL triggers of database 2 (read for "Disable DDL triggers").
    pub ddl_triggers: Vec<String>,
}

impl DataComparison {
    pub fn identical(&self) -> bool {
        self.tables.iter().all(TableResult::identical)
    }
}

/// Values above this many bytes are compared by length and hash on the server.
pub const LARGE: i64 = 1 << 20;

/// What the server can do, from its version.
#[derive(Clone, Copy)]
struct Server {
    /// TRANSLATE (SQL Server 2017).
    translate: bool,
    /// HASHBYTES over more than 8000 bytes (SQL Server 2016).
    hash: bool,
}

impl Server {
    fn of(db: &Database) -> Server {
        let v = if db.server.is_azure_sql_database() { 99 } else { db.server.major_version };
        Server { translate: v >= 14, hash: v >= 13 }
    }
}

/// One table's rows held in memory (a scripts folder's data scripts): values by column, as a
/// live read would return them.
#[derive(Clone, Debug, Default)]
pub struct MemoryTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Tables in memory, by name.
pub type Memory = std::collections::HashMap<ObjectName, MemoryTable>;

/// Where one side's rows come from.
#[derive(Clone, Copy)]
pub enum RowSource<'a> {
    Live(&'a ConnectionSpec),
    Memory(&'a Memory),
}

impl<'a> RowSource<'a> {
    fn live(self) -> Option<&'a ConnectionSpec> {
        match self {
            RowSource::Live(s) => Some(s),
            RowSource::Memory(_) => None,
        }
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
    compare_sources(RowSource::Live(spec1), RowSource::Live(spec2), db1, db2, opts, user, include, parallel, progress)
}

/// With a scripts folder on either side, computed columns are left out of the comparison, as
/// SQL Data Compare does (their values in the scripts are not read).
fn exclude_computed(p: &mut TablePair, db1: &Database, db2: &Database) {
    let computed = |db: &Database, t: &ObjectName, c: &str| {
        db.tables.iter().find(|x| x.schema.eq_ignore_ascii_case(&t.schema) && x.name.eq_ignore_ascii_case(&t.name)).is_some_and(|x| {
            x.columns.iter().any(|col| col.name.eq_ignore_ascii_case(c) && col.computed.is_some())
        })
    };
    let fq = |t: &ObjectName, c: &str| format!("{}.{}", t.quoted(), quote(c));
    let mut keep = Vec::new();
    for (i, c) in p.columns.iter().enumerate() {
        if computed(db1, &p.source, &c.source) || computed(db2, &p.target, &c.target) {
            p.notes.push(mapping::Note {
                code: "computed".into(),
                severity: crate::schema::deploy::Severity::High,
                text: format!(
                    "One of the fields {} and {} are computed - computed columns cannot be compared when one of the data sources is a scripts folder.",
                    fq(&p.source, &c.source),
                    fq(&p.target, &c.target)
                ),
            });
            if p.key.contains(&i) && p.problem.is_none() {
                p.problem = Some(format!("The comparison key of {} uses a computed column, which cannot be compared with a scripts folder.", p.source));
            }
        } else {
            keep.push(i);
        }
    }
    if keep.len() == p.columns.len() {
        return;
    }
    p.key = p.key.iter().filter_map(|k| keep.iter().position(|x| x == k)).collect();
    p.columns = keep.iter().map(|&i| p.columns[i].clone()).collect();
}

/// `compare` with either side's rows from a live database or from memory.
#[allow(clippy::too_many_arguments)]
pub fn compare_sources(
    side1: RowSource,
    side2: RowSource,
    db1: &Database,
    db2: &Database,
    opts: &DataOptions,
    user: &DataMappings,
    include: impl Fn(&TablePair) -> bool,
    parallel: usize,
    progress: impl Fn(&TablePair, &TableResult),
) -> Result<DataComparison, String> {
    let (pairs, only_in_1, only_in_2) = mapping::map_tables(db1, db2, opts, user);
    let folder = matches!(side1, RowSource::Memory(_)) || matches!(side2, RowSource::Memory(_));
    let mut pairs: Vec<TablePair> = pairs.into_iter().filter(|p| include(p)).collect();
    if folder {
        for p in &mut pairs {
            exclude_computed(p, db1, db2);
        }
    }
    let session = Session::create().map_err(|e| format!("cannot create the results folder: {e}"))?;
    let (s1, s2) = (Server::of(db1), Server::of(db2));
    // Rows read from a scripts folder hold whole values, so nothing is compared by hash.
    let server = Server { translate: s1.translate && s2.translate, hash: s1.hash && s2.hash && !folder };
    let rt = db::runtime()?;
    let (tables, ddl_triggers) = rt.block_on(async {
        let jobs = pairs.into_iter().enumerate().map(|(i, pair)| {
            let path = session.table_path(i);
            async move { compare_table(side1, side2, pair, opts, server, path).await }
        });
        let mut done: Vec<TableResult> = futures_util::stream::iter(jobs).buffered(parallel.max(1)).collect().await;
        for r in &done {
            progress(&r.pair, r);
        }
        done.sort_by(|a, b| a.pair.source.cmp(&b.pair.source));
        let mut triggers = Vec::new();
        if opts.has(DataOpt::DisableDdlTriggers)
            && let Some(spec2) = side2.live()
        {
            let mut c = db::connect_async(spec2).await?;
            let rows = db::query(&mut c, "SELECT name FROM sys.triggers WHERE parent_class = 0 AND is_disabled = 0 AND is_ms_shipped = 0 ORDER BY name").await?;
            triggers = rows.iter().filter_map(|r| r.get::<&str, _>(0).map(str::to_string)).collect();
        }
        Ok::<_, String>((done, triggers))
    })?;
    Ok(DataComparison { session, tables, only_in_1, only_in_2, ddl_triggers })
}

async fn compare_table(side1: RowSource<'_>, side2: RowSource<'_>, pair: TablePair, opts: &DataOptions, server: Server, path: PathBuf) -> TableResult {
    let mut result = TableResult {
        pair,
        counts: [0; 4],
        offsets: Default::default(),
        path,
        error: None,
        by_checksum: false,
        repeated_keys: false,
        identity_current: (None, None),
        column_differences: Vec::new(),
    };
    if let Some(p) = &result.pair.problem {
        result.error = Some(p.clone());
        return result;
    }
    if let Err(e) = run(side1, side2, opts, server, &mut result).await {
        result.error = Some(e);
    }
    if opts.has(DataOpt::ReseedIdentity)
        && result.pair.columns.iter().any(|c| c.identity && c.source_identity)
        && let (Some(spec1), Some(spec2)) = (side1.live(), side2.live())
    {
        let (a, b) = tokio::join!(ident_current(spec1, &result.pair.source), ident_current(spec2, &result.pair.target));
        result.identity_current = (a, b);
    }
    result
}

async fn ident_current(spec: &ConnectionSpec, table: &ObjectName) -> Option<i128> {
    let mut c = db::connect_async(spec).await.ok()?;
    let rows = db::query(&mut c, &format!("SELECT CAST(IDENT_CURRENT({}) AS decimal(38, 0))", literal(&table.quoted()))).await.ok()?;
    match Value::read(rows.first()?, 0) {
        Value::Decimal(v, _) => Some(v),
        Value::Int(v) => Some(v as i128),
        _ => None,
    }
}

fn family(t: &DataType) -> &'static str {
    match t.name.as_str() {
        "char" | "varchar" | "nchar" | "nvarchar" | "sysname" | "text" | "ntext" => "string",
        "tinyint" | "smallint" | "int" | "bigint" => "int",
        "date" | "time" | "datetime" | "smalldatetime" | "datetime2" | "datetimeoffset" => "date",
        "uniqueidentifier" => "guid",
        _ => "",
    }
}

fn is_clr(t: &DataType) -> bool {
    t.schema.is_some() || matches!(t.name.as_str(), "hierarchyid" | "geometry" | "geography")
}

fn is_lob(t: &DataType) -> bool {
    matches!(t.name.as_str(), "text" | "ntext" | "image" | "xml") || (matches!(t.name.as_str(), "varchar" | "nvarchar" | "varbinary") && t.length == Some(-1))
}

/// The string folding a key column gets on both servers.
fn key_fold(c: &ColumnPair, opts: &DataOptions) -> Fold {
    if opts.has(DataOpt::ForceBinaryCollation) {
        return Fold::default();
    }
    let mut f = Fold::of(c.collation());
    if !c.case_insensitive {
        f.case = false;
    }
    // A guid compared with text ignores case.
    if family(&c.source_type) == "guid" || family(&c.target_type) == "guid" {
        f.case = true;
    }
    f
}

/// The expression a key column is ordered and matched by. Strings are compared after trailing
/// spaces are removed (as SQL Server does) and folded as the collation ignores case, accents,
/// kana and width; both servers order them by code point so the client can merge them exactly.
/// Keys whose types differ between the databases are brought to a common form.
fn sort_expr(column: &str, this: &DataType, other: &DataType, fold: Fold, server: Server) -> String {
    let c = quote(column);
    let (f1, f2) = (family(this), family(other));
    let text = f1 == "string" || (f2 == "string" && matches!(f1, "int" | "guid"));
    if text {
        let mut e = format!("RTRIM(CAST({c} AS nvarchar(4000)))");
        if fold.case {
            e = format!("UPPER({e})");
        }
        e = format!("{e} COLLATE Latin1_General_100_BIN2");
        let (from, to) = Fold { case: false, ..fold }.table();
        if server.translate && !from.is_empty() {
            e = format!("TRANSLATE({e}, {}, {}) COLLATE Latin1_General_100_BIN2", literal(&from), literal(&to));
        }
        e
    } else if f1 == "date" && f2 == "date" && this.name != other.name {
        format!("CAST({c} AS datetime2(7))")
    } else {
        value_exprs(column, this, false).0.remove(0)
    }
}

/// How a column's value is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// One value.
    Plain,
    /// sql_variant: base type, precision, scale, max length, collation, text.
    Variant,
    /// CLR: bytes and text.
    Clr,
    /// LOB: length, value when small, hash when large.
    Lob,
}

impl Shape {
    fn width(self) -> usize {
        match self {
            Shape::Plain => 1,
            Shape::Clr => 2,
            Shape::Lob => 3,
            Shape::Variant => 6,
        }
    }
}

/// The select-list expressions for a value, and how to read them back.
fn value_exprs(column: &str, t: &DataType, hash: bool) -> (Vec<String>, Shape) {
    let c = quote(column);
    let plain = match t.name.as_str() {
        "money" => format!("CAST({c} AS decimal(19, 4))"),
        "smallmoney" => format!("CAST({c} AS decimal(10, 4))"),
        "xml" => format!("CONVERT(nvarchar(max), {c}, 1)"),
        _ => c.clone(),
    };
    if t.schema.is_none() && t.name == "sql_variant" {
        let p = |n: &str| format!("SQL_VARIANT_PROPERTY({c}, '{n}')");
        let base = p("BaseType");
        let text = format!(
            "CASE WHEN {c} IS NULL THEN NULL \
             WHEN {base} IN ('date', 'time', 'datetime', 'smalldatetime', 'datetime2', 'datetimeoffset') THEN CONVERT(nvarchar(4000), {c}, 121) \
             WHEN {base} IN ('float', 'real') THEN CONVERT(nvarchar(4000), {c}, 3) \
             WHEN {base} IN ('money', 'smallmoney') THEN CONVERT(nvarchar(4000), {c}, 2) \
             WHEN {base} IN ('binary', 'varbinary') THEN CONVERT(nvarchar(4000), CAST({c} AS varbinary(8000)), 1) \
             ELSE CONVERT(nvarchar(4000), {c}) END"
        );
        let ints = ["Precision", "Scale", "MaxLength"].map(|n| format!("CAST({} AS int)", p(n)));
        return (
            vec![format!("CAST({base} AS nvarchar(128))"), ints[0].clone(), ints[1].clone(), ints[2].clone(), format!("CAST({} AS nvarchar(128))", p("Collation")), text],
            Shape::Variant,
        );
    }
    if is_clr(t) {
        return (vec![format!("CAST({c} AS varbinary(max))"), format!("{c}.ToString()")], Shape::Clr);
    }
    if is_lob(t) && hash {
        let bytes = match t.name.as_str() {
            "xml" => format!("CAST(CONVERT(nvarchar(max), {c}, 1) AS varbinary(max))"),
            "text" => format!("CAST(CAST({c} AS varchar(max)) AS varbinary(max))"),
            "ntext" => format!("CAST(CAST({c} AS nvarchar(max)) AS varbinary(max))"),
            _ => format!("CAST({c} AS varbinary(max))"),
        };
        let len = format!("CAST(DATALENGTH({c}) AS bigint)");
        return (
            vec![
                len.clone(),
                format!("CASE WHEN {len} <= {LARGE} THEN {plain} END"),
                format!("CASE WHEN {len} > {LARGE} THEN HASHBYTES('SHA2_256', {bytes}) END"),
            ],
            Shape::Lob,
        );
    }
    (vec![plain], Shape::Plain)
}

/// ` WHERE <filter>` from a user filter written with or without the WHERE keyword.
pub fn where_clause(filter: &Option<String>) -> String {
    let Some(w) = filter.as_deref().map(str::trim).filter(|w| !w.is_empty()) else { return String::new() };
    let w = match w.get(..5) {
        Some(head) if head.eq_ignore_ascii_case("where") && w[5..].starts_with(char::is_whitespace) => w[5..].trim(),
        _ => w,
    };
    format!(" WHERE {w}")
}

/// The SELECT for one side, and the shape of each compared column.
fn select_sql(pair: &TablePair, side: u8, opts: &DataOptions, server: Server) -> (String, Vec<Shape>) {
    let table = if side == 1 { pair.source.quoted() } else { pair.target.quoted() };
    let name = |i: usize| if side == 1 { &pair.columns[i].source } else { &pair.columns[i].target };
    let ty = |i: usize| if side == 1 { &pair.columns[i].source_type } else { &pair.columns[i].target_type };
    let other = |i: usize| if side == 1 { &pair.columns[i].target_type } else { &pair.columns[i].source_type };
    let mut list: Vec<String> = pair.key.iter().map(|&k| sort_expr(name(k), ty(k), other(k), key_fold(&pair.columns[k], opts), server)).collect();
    let mut shapes = Vec::new();
    for i in 0..pair.columns.len() {
        let (exprs, shape) = value_exprs(name(i), ty(i), server.hash);
        list.extend(exprs);
        shapes.push(shape);
    }
    let filter = where_clause(if side == 1 { &pair.where1 } else { &pair.where2 });
    let order: Vec<String> = (1..=pair.key.len()).map(|i| i.to_string()).collect();
    (format!("SELECT {} FROM {table} WITH (NOLOCK){filter} ORDER BY {}", list.join(", "), order.join(", ")), shapes)
}

/// Reads a row: the key values, then one value per compared column.
fn read_row(row: &tiberius::Row, keys: usize, shapes: &[Shape]) -> Vec<Value> {
    let mut out: Vec<Value> = (0..keys).map(|i| Value::read(row, i)).collect();
    let mut i = keys;
    for &shape in shapes {
        let v = match shape {
            Shape::Plain => Value::read(row, i),
            Shape::Clr => match Value::read(row, i) {
                Value::Bytes(b) => Value::Clr(b, match Value::read(row, i + 1) {
                    Value::Str(s) => Some(s),
                    _ => None,
                }),
                _ => Value::Null,
            },
            Shape::Lob => match (Value::read(row, i), Value::read(row, i + 1), Value::read(row, i + 2)) {
                (Value::Null, _, _) => Value::Null,
                (Value::Int(n), _, Value::Bytes(h)) if n > LARGE => Value::Large(n, h),
                (_, v, _) => v,
            },
            Shape::Variant => match Value::read(row, i) {
                Value::Str(base_type) => {
                    let int = |k: usize| match Value::read(row, i + k) {
                        Value::Int(n) => n,
                        _ => 0,
                    };
                    let text = |k: usize| match Value::read(row, i + k) {
                        Value::Str(s) => Some(s),
                        _ => None,
                    };
                    Value::Variant(Box::new(Variant {
                        base_type,
                        precision: int(1) as u8,
                        scale: int(2) as u8,
                        max_length: int(3) as i32,
                        collation: text(4),
                        text: text(5).unwrap_or_default(),
                    }))
                }
                _ => Value::Null,
            },
        };
        out.push(v);
        i += shape.width();
    }
    out
}

/// Each side's own checksum and row count from sp_table_validation; `None` when the procedure
/// cannot run (the rows are then compared).
async fn checksum(spec: &ConnectionSpec, table: &ObjectName) -> Option<(String, i64)> {
    let mut conn = db::connect_async(spec).await.ok()?;
    let sql = format!(
        "DECLARE @c decimal(38, 0), @r int; \
         EXEC sp_table_validation @table = {}, @owner = {}, @rowcount_only = 2, @full_or_fast = 0, @expected_checksum = @c OUTPUT, @expected_rowcount = @r OUTPUT; \
         SELECT CAST(@c AS nvarchar(50)), CAST(@r AS bigint)",
        literal(&table.name),
        literal(&table.schema)
    );
    let rows = db::query(&mut conn, &sql).await.ok()?;
    let r = rows.first()?;
    match (Value::read(r, 0), Value::read(r, 1)) {
        (Value::Str(c), Value::Int(n)) => Some((c, n)),
        _ => None,
    }
}

const CHUNK: usize = 512;
const CHUNK_BYTES: usize = 4 << 20;

fn size(v: &Value) -> usize {
    match v {
        Value::Str(s) => s.len(),
        Value::Bytes(b) | Value::Clr(b, _) => b.len(),
        _ => 16,
    }
}

async fn produce(spec: &ConnectionSpec, sql: String, keys: usize, shapes: Vec<Shape>, tx: mpsc::Sender<Result<Vec<Vec<Value>>, String>>) {
    let run = async {
        let mut conn = db::connect_async(spec).await?;
        db::execute(&mut conn, "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED").await?;
        let mut stream = conn.simple_query(&sql).await.map_err(|e| e.to_string())?.into_row_stream();
        let mut chunk = Vec::with_capacity(CHUNK);
        let mut bytes = 0;
        while let Some(row) = stream.try_next().await.map_err(|e| e.to_string())? {
            let values = read_row(&row, keys, &shapes);
            bytes += values.iter().map(size).sum::<usize>();
            chunk.push(values);
            // Rows go in chunks of a bounded count and size, so large values are not piled up.
            if (chunk.len() == CHUNK || bytes >= CHUNK_BYTES) && tx.send(Ok(std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK)))).await.is_err() {
                return Ok(());
            }
            if chunk.is_empty() {
                bytes = 0;
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

/// Sends a side's rows from memory in the order and form the live SELECT would give: key
/// expressions first (strings trimmed, folded and compared by code point), then the values.
async fn produce_memory(memory: &Memory, pair: &TablePair, side: u8, opts: &DataOptions, server: Server, tx: mpsc::Sender<Result<Vec<Vec<Value>>, String>>) {
    let rows = memory_rows(memory, pair, side, opts, server);
    match rows {
        Ok(rows) => {
            for chunk in rows.chunks(CHUNK) {
                if tx.send(Ok(chunk.to_vec())).await.is_err() {
                    return;
                }
            }
        }
        Err(e) => {
            let _ = tx.send(Err(e)).await;
        }
    }
}

fn memory_rows(memory: &Memory, pair: &TablePair, side: u8, opts: &DataOptions, server: Server) -> Result<Vec<Vec<Value>>, String> {
    let table = if side == 1 { &pair.source } else { &pair.target };
    let t = memory
        .iter()
        .find(|(n, _)| n.schema.eq_ignore_ascii_case(&table.schema) && n.name.eq_ignore_ascii_case(&table.name))
        .map(|(_, t)| t)
        .cloned()
        .unwrap_or_default();
    let name = |i: usize| if side == 1 { &pair.columns[i].source } else { &pair.columns[i].target };
    let ty = |i: usize| if side == 1 { &pair.columns[i].source_type } else { &pair.columns[i].target_type };
    let other = |i: usize| if side == 1 { &pair.columns[i].target_type } else { &pair.columns[i].source_type };
    let index: Vec<Option<usize>> = (0..pair.columns.len()).map(|i| t.columns.iter().position(|c| c.eq_ignore_ascii_case(name(i)))).collect();
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(t.rows.len());
    for row in &t.rows {
        let value = |i: usize| index[i].and_then(|k| row.get(k).cloned()).unwrap_or(Value::Null);
        let mut v: Vec<Value> = pair.key.iter().map(|&k| memory_key(value(k), ty(k), other(k), key_fold(&pair.columns[k], opts), server)).collect();
        v.extend((0..pair.columns.len()).map(value));
        out.push(v);
    }
    let keys = pair.key.len();
    out.sort_by(|a, b| cmp_keys(a, b, keys));
    Ok(out)
}

/// A key value as `sort_expr` makes the server return it.
fn memory_key(v: Value, this: &DataType, other: &DataType, fold: Fold, server: Server) -> Value {
    let (f1, f2) = (family(this), family(other));
    let text = f1 == "string" || (f2 == "string" && matches!(f1, "int" | "guid"));
    if text {
        let s = match &v {
            Value::Null => return Value::Null,
            Value::Str(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Guid(_) => v.display().to_uppercase(),
            other => other.display(),
        };
        let mut s = s.trim_end_matches(' ').to_string();
        if fold.case {
            s = s.to_uppercase();
        }
        let (from, to) = Fold { case: false, ..fold }.table();
        if server.translate && !from.is_empty() {
            let map: std::collections::HashMap<char, char> = from.chars().zip(to.chars()).collect();
            s = s.chars().map(|c| *map.get(&c).unwrap_or(&c)).collect();
        }
        return Value::Str(s);
    }
    if f1 == "date" && f2 == "date" && this.name != other.name {
        return match v {
            Value::Date(d) => Value::DateTime(d as i128 * 86_400_000_000_000),
            other => other,
        };
    }
    v
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

fn equality(c: &ColumnPair, opts: &DataOptions) -> Equality {
    let char_type = |t: &DataType| matches!(t.name.as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "sysname");
    Equality {
        binary: opts.has(DataOpt::ForceBinaryCollation),
        fold: Fold::of(c.collation()),
        trim_trailing_spaces: opts.has(DataOpt::TrimTrailingSpaces),
        empty_is_null: opts.has(DataOpt::TreatEmptyStringAsNull) && char_type(&c.target_type),
        full_precision: opts.has(DataOpt::UseFullPrecisionForComparison),
        real: c.source_type.name == "real" || c.target_type.name == "real",
        xml: (c.source_type.name == "xml", c.target_type.name == "xml"),
    }
}

async fn run(side1: RowSource<'_>, side2: RowSource<'_>, opts: &DataOptions, server: Server, result: &mut TableResult) -> Result<(), String> {
    let pair = &result.pair;
    let keys = pair.key.len();
    let width = pair.columns.len();
    // The checksum shortcut (live databases only, never with identical rows shown).
    if opts.has(DataOpt::UseChecksumComparison)
        && !opts.has(DataOpt::IncludeIdenticalRecords)
        && pair.where1 == pair.where2
        && let (Some(spec1), Some(spec2)) = (side1.live(), side2.live())
    {
        let (a, b) = tokio::join!(checksum(spec1, &pair.source), checksum(spec2, &pair.target));
        if let (Some(a), Some(b)) = (a, b)
            && a == b
        {
            result.counts[RowKind::Identical as usize] = a.1 as u64;
            result.by_checksum = true;
            return Ok(());
        }
    }
    let (tx1, rx1) = mpsc::channel(4);
    let (tx2, rx2) = mpsc::channel(4);
    let (sql1, shapes1) = select_sql(pair, 1, opts, server);
    let (sql2, shapes2) = select_sql(pair, 2, opts, server);
    let p1 = async {
        match side1 {
            RowSource::Live(spec) => produce(spec, sql1, keys, shapes1, tx1).await,
            RowSource::Memory(m) => produce_memory(m, pair, 1, opts, server, tx1).await,
        }
    };
    let p2 = async {
        match side2 {
            RowSource::Live(spec) => produce(spec, sql2, keys, shapes2, tx2).await,
            RowSource::Memory(m) => produce_memory(m, pair, 2, opts, server, tx2).await,
        }
    };
    let mut writer = TableWriter::create(&result.path, opts.has(DataOpt::CompressTemporaryFiles)).map_err(|e| e.to_string())?;
    let store_identical = opts.has(DataOpt::IncludeIdenticalRecords);
    let eq: Vec<Equality> = pair.columns.iter().map(|c| equality(c, opts)).collect();
    let mut counts = [0u64; 4];
    let mut column_differences = vec![0u64; width];
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
                    let differs: Vec<bool> = (0..width).map(|i| !pair.key.contains(&i) && !same(&x[keys + i], &y[keys + i], eq[i])).collect();
                    for (n, d) in column_differences.iter_mut().zip(&differs) {
                        *n += *d as u64;
                    }
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
    result.column_differences = column_differences;
    result.repeated_keys = repeated;
    Ok(())
}

/// Reads the full value of column `column` of a database 1 row whose stored values are
/// `values` (for a value that was compared by hash).
pub fn fetch_value(spec: &ConnectionSpec, pair: &TablePair, column: usize, values: &[Value]) -> Result<Value, String> {
    let c = &pair.columns[column];
    let (exprs, _) = value_exprs(&c.source, &c.source_type, false);
    let wheres: Vec<String> = pair
        .key
        .iter()
        .map(|&k| {
            let kc = &pair.columns[k];
            let lit = super::value::Literal {
                source: &kc.source_type,
                target: &kc.source_type,
                source_collation: kc.source_collation.as_deref(),
                target_collation: kc.source_collation.as_deref(),
                clr_as_text: false,
                graph_table: None,
            };
            match &values[k] {
                Value::Null => format!("{} IS NULL", quote(&kc.source)),
                v => format!("{} = {}", quote(&kc.source), v.sql(&lit)),
            }
        })
        .collect();
    let sql = format!("SELECT TOP (1) {} FROM {} WHERE {}", exprs[0], pair.source.quoted(), wheres.join(" AND "));
    let rt = db::runtime()?;
    rt.block_on(async {
        let mut conn = db::connect_async(spec).await?;
        let rows = db::query(&mut conn, &sql).await?;
        Ok(rows.first().map_or(Value::Null, |r| Value::read(r, 0)))
    })
}

/// Every row of database 1's table as INSERT statements, in key order (for `/makescripts`).
pub fn script_rows(spec: &ConnectionSpec, db: &Database, pair: &TablePair) -> Result<String, String> {
    let server = Server { hash: false, ..Server::of(db) };
    let (sql, shapes) = select_sql(pair, 1, &DataOptions::none(), server);
    let keys = pair.key.len();
    let writable: Vec<usize> = (0..pair.columns.len()).filter(|&c| !pair.columns[c].read_only).collect();
    let identity = pair.columns.iter().any(|c| c.identity);
    let name = pair.source.quoted();
    let cols = writable.iter().map(|&c| quote(&pair.columns[c].source)).collect::<Vec<_>>().join(", ");
    let rt = db::runtime()?;
    rt.block_on(async {
        let mut conn = db::connect_async(spec).await?;
        let mut stream = conn.simple_query(&sql).await.map_err(|e| e.to_string())?.into_row_stream();
        let mut out = String::new();
        while let Some(row) = stream.try_next().await.map_err(|e| e.to_string())? {
            let values = read_row(&row, keys, &shapes);
            if out.is_empty() && identity {
                out.push_str(&format!("SET IDENTITY_INSERT {name} ON\r\n"));
            }
            let list: Vec<String> = writable
                .iter()
                .map(|&c| {
                    let col = &pair.columns[c];
                    let lit = super::value::Literal {
                        source: &col.source_type,
                        target: &col.source_type,
                        source_collation: col.source_collation.as_deref(),
                        target_collation: col.source_collation.as_deref(),
                        clr_as_text: false,
                        graph_table: col.graph.as_deref().filter(|g| matches!(*g, "$node_id" | "$edge_id")).map(|_| pair.source.name.as_str()),
                    };
                    values[keys + c].sql(&lit)
                })
                .collect();
            out.push_str(&format!("INSERT INTO {name} ({cols}) VALUES ({})\r\n", list.join(", ")));
        }
        if !out.is_empty() && identity {
            out.push_str(&format!("SET IDENTITY_INSERT {name} OFF\r\n"));
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(n: &str, t: &str, ci: bool) -> ColumnPair {
        ColumnPair {
            source: n.into(),
            target: n.into(),
            source_type: DataType { name: t.into(), ..Default::default() },
            target_type: DataType { name: t.into(), ..Default::default() },
            read_only: false,
            identity: false,
            case_insensitive: ci,
            nullable: true,
            source_collation: ci.then(|| "Latin1_General_CI_AS".into()),
            target_collation: None,
            source_identity: false,
            graph: None,
        }
    }

    #[test]
    fn select_statements() {
        let pair = TablePair {
            source: ObjectName::new("dbo", "T"),
            target: ObjectName::new("dbo", "T"),
            columns: vec![col("Code", "varchar", true), col("Id", "int", false), col("Name", "nvarchar", true), col("V", "sql_variant", false)],
            key: vec![1, 0],
            key_name: Some("PK".into()),
            target_key_name: None,
            where1: Some("WHERE Id > 5".into()),
            where2: None,
            problem: None,
            is_view: false,
            notes: Vec::new(),
        };
        let old = Server { translate: false, hash: false };
        let (sql, shapes) = select_sql(&pair, 1, &DataOptions::default_set(), old);
        assert!(sql.starts_with("SELECT [Id], UPPER(RTRIM(CAST([Code] AS nvarchar(4000)))) COLLATE Latin1_General_100_BIN2, [Code], [Id], [Name], CAST(SQL_VARIANT_PROPERTY([V], 'BaseType') AS nvarchar(128)),"), "{sql}");
        assert!(sql.ends_with(" FROM [dbo].[T] WITH (NOLOCK) WHERE Id > 5 ORDER BY 1, 2"), "{sql}");
        assert_eq!(shapes, [Shape::Plain, Shape::Plain, Shape::Plain, Shape::Variant]);
        let binary = DataOptions::default_set().with(DataOpt::ForceBinaryCollation);
        assert!(select_sql(&pair, 2, &binary, old).0.contains("RTRIM(CAST([Code] AS nvarchar(4000))) COLLATE"));
        let new = Server { translate: true, hash: true };
        assert!(select_sql(&pair, 1, &DataOptions::default_set(), new).0.contains("TRANSLATE(UPPER(RTRIM(CAST([Code] AS nvarchar(4000)))) COLLATE Latin1_General_100_BIN2, N'"));
        // Keys of different types meet in a common form.
        let mixed = DataType { name: "nvarchar".into(), length: Some(10), ..Default::default() };
        assert_eq!(sort_expr("K", &DataType { name: "int".into(), ..Default::default() }, &mixed, Fold::default(), old), "RTRIM(CAST([K] AS nvarchar(4000))) COLLATE Latin1_General_100_BIN2");
        assert_eq!(sort_expr("K", &DataType { name: "date".into(), ..Default::default() }, &DataType { name: "datetime".into(), ..Default::default() }, Fold::default(), old), "CAST([K] AS datetime2(7))");
        let (lob, shape) = value_exprs("B", &DataType { name: "varbinary".into(), length: Some(-1), ..Default::default() }, true);
        assert_eq!(shape, Shape::Lob);
        assert_eq!(lob[2], "CASE WHEN CAST(DATALENGTH([B]) AS bigint) > 1048576 THEN HASHBYTES('SHA2_256', CAST([B] AS varbinary(max))) END");
    }
}
