//! The data of a scripts folder, read without a server: the folder's table definitions and the
//! INSERT statements of its data scripts become rows in memory, in the form a live read gives,
//! for the data engine to compare. Deploying data to the folder writes the data scripts again.
//!
//! Values the folder cannot give offline (computed columns, CLR types, sql_variant, values
//! compared by hash) make the caller fall back to loading the folder into a database.

use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate};

use super::folder::{self, Folder};
use super::staging::data_files;
use crate::catalog::{Column, DataType, Database, ObjectName, Table, UserTypeKind};
use crate::data::compare::{LARGE, Memory, MemoryTable};
use crate::data::store::{self, RowKind};
use crate::data::value::{Literal, Value};
use crate::schema::text::{Tok, Token, tokenize};

const DAY: i128 = 86_400_000_000_000;

/// A scripts folder with its data in memory.
pub struct DataFolder {
    pub folder: Folder,
    pub memory: Memory,
    /// The data script of each table read.
    pub files: Vec<(ObjectName, PathBuf)>,
    _checkout: Option<super::scm::Checkout>,
}

/// A literal of a data script.
#[derive(Clone, Debug)]
enum Lit {
    Null,
    Number(String),
    Text(String),
    Bytes(Vec<u8>),
}

/// Reads the folder at `dir` (at `revision`, if given). `Ok(Err(reason))` when its data needs a
/// server to be read.
pub fn read(dir: &Path, revision: Option<&str>) -> Result<Result<DataFolder, String>, String> {
    let checkout = revision.map(|r| super::scm::checkout(&dir.to_string_lossy(), r)).transpose()?;
    let read_dir = checkout.as_ref().map(|c| c.dir.clone()).unwrap_or_else(|| dir.to_path_buf());
    let mut f = folder::read(&read_dir)?;
    if checkout.is_some() {
        f.database.name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(f.database.name);
    }
    for t in &f.database.tables {
        if let Some(c) = t.columns.iter().find(|c| needs_server(&f.database, c)) {
            return Ok(Err(format!("{}.[{}] cannot be read without a server", t.object_name(), c.name)));
        }
    }
    let mut memory = Memory::new();
    let mut files = Vec::new();
    for path in data_files(&f) {
        let text = folder::decode(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?, &f.info.encoding);
        match parse_file(&f.database, &f.info.default_schema, &text) {
            Ok(tables) => {
                for (name, t) in tables {
                    files.push((name.clone(), path.clone()));
                    let entry = memory.entry(name).or_insert_with(|| MemoryTable { columns: t.columns.clone(), rows: Vec::new() });
                    if entry.columns != t.columns {
                        return Ok(Err(format!("{}: the INSERT statements list different columns", path.display())));
                    }
                    entry.rows.extend(t.rows);
                }
            }
            Err(reason) => return Ok(Err(format!("{}: {reason}", path.display()))),
        }
    }
    Ok(Ok(DataFolder { folder: f, memory, files, _checkout: checkout }))
}

/// The column's base type: alias types resolved.
fn base_type<'a>(db: &'a Database, t: &'a DataType) -> &'a DataType {
    if let Some(schema) = &t.schema
        && let Some(u) = db.types.iter().find(|u| u.schema.eq_ignore_ascii_case(schema) && u.name.eq_ignore_ascii_case(&t.name))
        && u.kind == UserTypeKind::Alias
        && let Some(b) = &u.base_type
    {
        return b;
    }
    t
}

fn needs_server(db: &Database, c: &Column) -> bool {
    let t = base_type(db, &c.data_type);
    c.computed.is_some() || t.schema.is_some() || matches!(t.name.as_str(), "sql_variant" | "hierarchyid" | "geometry" | "geography")
}

fn significant(text: &str) -> Vec<Token> {
    tokenize(text).into_iter().filter(|t| !matches!(t.kind, Tok::Space | Tok::LineComment | Tok::BlockComment)).collect()
}

fn unquote(s: &str) -> String {
    if s.starts_with('[') && s.ends_with(']') && s.len() >= 2 {
        s[1..s.len() - 1].replace("]]", "]")
    } else if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].replace("\"\"", "\"")
    } else {
        s.to_string()
    }
}

fn string_value(s: &str) -> String {
    let s = s.strip_prefix(['N', 'n']).unwrap_or(s);
    let inner = s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')).unwrap_or(s);
    // A backslash before a line break was doubled so the server would not join the lines.
    inner.replace("''", "'").replace("\\\\\r\n\r\n", "\\\r\n").replace("\\\\\n\n", "\\\n")
}

/// The INSERT statements of one file, by table.
fn parse_file(db: &Database, default_schema: &str, text: &str) -> Result<Vec<(ObjectName, MemoryTable)>, String> {
    let t = significant(text);
    let mut out: Vec<(ObjectName, MemoryTable)> = Vec::new();
    let mut k = 0;
    while k < t.len() {
        let w = t[k].text.to_ascii_uppercase();
        if w == "INSERT" {
            k += 1;
            if t.get(k).is_some_and(|x| x.text.eq_ignore_ascii_case("INTO")) {
                k += 1;
            }
            let mut parts = vec![unquote(&t.get(k).ok_or("INSERT without a table")?.text)];
            k += 1;
            while t.get(k).is_some_and(|x| x.text == ".") {
                parts.push(unquote(&t.get(k + 1).ok_or("bad table name")?.text));
                k += 2;
            }
            let name = match parts.as_slice() {
                [n] => ObjectName::new(default_schema, n),
                [.., s, n] => ObjectName::new(s, n),
                [] => return Err("INSERT without a table".into()),
            };
            let table = db
                .tables
                .iter()
                .find(|x| x.schema.eq_ignore_ascii_case(&name.schema) && x.name.eq_ignore_ascii_case(&name.name))
                .ok_or_else(|| format!("no table {name} in the folder"))?;
            let mut columns = Vec::new();
            if t.get(k).is_some_and(|x| x.text == "(") {
                k += 1;
                while k < t.len() && t[k].text != ")" {
                    if t[k].text != "," {
                        columns.push(unquote(&t[k].text));
                    }
                    k += 1;
                }
                k += 1;
            } else {
                columns = table.columns.iter().filter(|c| c.computed.is_none()).map(|c| c.name.clone()).collect();
            }
            if !t.get(k).is_some_and(|x| x.text.eq_ignore_ascii_case("VALUES")) {
                return Err(format!("INSERT INTO {name} without VALUES is not read"));
            }
            k += 1;
            let mut rows = Vec::new();
            loop {
                if !t.get(k).is_some_and(|x| x.text == "(") {
                    return Err("bad VALUES list".into());
                }
                k += 1;
                let mut row = Vec::new();
                loop {
                    let (lit, next) = literal(&t, k)?;
                    row.push(lit);
                    k = next;
                    match t.get(k).map(|x| x.text.as_str()) {
                        Some(",") => k += 1,
                        Some(")") => {
                            k += 1;
                            break;
                        }
                        _ => return Err("bad VALUES list".into()),
                    }
                }
                if row.len() != columns.len() {
                    return Err(format!("INSERT INTO {name}: {} values for {} columns", row.len(), columns.len()));
                }
                rows.push(row);
                if t.get(k).is_some_and(|x| x.text == ",") {
                    k += 1;
                    continue;
                }
                break;
            }
            let all = all_columns(table);
            let mut mt = MemoryTable { columns: all.iter().map(|c| c.name.clone()).collect(), rows: Vec::new() };
            for row in rows {
                let mut values = Vec::with_capacity(all.len());
                for c in &all {
                    let v = match columns.iter().position(|n| n.eq_ignore_ascii_case(&c.name)) {
                        Some(i) => to_value(&row[i], base_type(db, &c.data_type))
                            .ok_or_else(|| format!("{name}.[{}]: value {:?} is not a {}", c.name, row[i], c.data_type.sql()))?,
                        // A column left out takes its default, which only a server can work out.
                        None if c.default.is_some() || c.identity.is_some() => return Err(format!("{name}.[{}] is not in the INSERT statements", c.name)),
                        None => Value::Null,
                    };
                    if large(&v) {
                        return Err(format!("{name}.[{}] holds values over {LARGE} bytes", c.name));
                    }
                    values.push(v);
                }
                mt.rows.push(values);
            }
            match out.iter_mut().find(|(n, _)| *n == name) {
                Some((_, existing)) => existing.rows.extend(mt.rows),
                None => out.push((name, mt)),
            }
            continue;
        }
        // SET IDENTITY_INSERT, GO and the like.
        k += 1;
    }
    Ok(out)
}

fn all_columns(t: &Table) -> Vec<&Column> {
    t.columns.iter().collect()
}

fn large(v: &Value) -> bool {
    match v {
        Value::Str(s) => s.len() as i64 > LARGE / 2,
        Value::Bytes(b) => b.len() as i64 > LARGE,
        _ => false,
    }
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    let h = if h.len() % 2 == 1 { format!("0{h}") } else { h.to_string() };
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok()).collect()
}

/// One literal at token `k`; returns it and the next token index.
fn literal(t: &[Token], k: usize) -> Result<(Lit, usize), String> {
    let (mut lit, mut k) = term(t, k)?;
    loop {
        if t.get(k).is_some_and(|x| x.text.eq_ignore_ascii_case("COLLATE")) {
            k += 2;
            continue;
        }
        if t.get(k).is_some_and(|x| x.text == "+") {
            let (rhs, next) = term(t, k + 1)?;
            lit = match (lit, rhs) {
                (Lit::Text(a), Lit::Text(b)) => Lit::Text(a + &b),
                _ => return Err("only strings are joined with +".into()),
            };
            k = next;
            continue;
        }
        return Ok((lit, k));
    }
}

fn term(t: &[Token], k: usize) -> Result<(Lit, usize), String> {
    let tok = t.get(k).ok_or("a value is missing")?;
    let up = tok.text.to_ascii_uppercase();
    match tok.kind {
        Tok::Str => return Ok((Lit::Text(string_value(&tok.text)), k + 1)),
        Tok::Word if up == "NULL" => return Ok((Lit::Null, k + 1)),
        Tok::Word if up.starts_with("0X") => return Ok((Lit::Bytes(parse_hex(&tok.text).ok_or("bad binary value")?), k + 1)),
        Tok::Word if tok.text.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.' || c == '$') => {
            let mut n = tok.text.clone();
            let mut next = k + 1;
            // 1.5e+10 comes as three tokens.
            if n.ends_with(['e', 'E']) && t.get(next).is_some_and(|x| x.text == "+" || x.text == "-") {
                n.push_str(&t[next].text);
                n.push_str(&t.get(next + 1).ok_or("bad number")?.text);
                next += 2;
            }
            return Ok((Lit::Number(n), next));
        }
        Tok::Punct if tok.text == "-" || tok.text == "+" => {
            let (inner, next) = term(t, k + 1)?;
            return match inner {
                Lit::Number(n) => Ok((Lit::Number(if tok.text == "-" { format!("-{n}") } else { n }), next)),
                _ => Err("a sign before something that is not a number".into()),
            };
        }
        Tok::Punct if tok.text == "(" => {
            let (inner, next) = literal(t, k + 1)?;
            if !t.get(next).is_some_and(|x| x.text == ")") {
                return Err("unbalanced parentheses".into());
            }
            return Ok((inner, next + 1));
        }
        _ => {}
    }
    // CONVERT(xml, N'...', 1), CAST(N'...' AS varchar(8000)), replicate(N'', 0).
    if t.get(k + 1).is_some_and(|x| x.text == "(") {
        let close = {
            let mut depth = 0;
            let mut end = None;
            for (j, x) in t.iter().enumerate().skip(k + 1) {
                if x.text == "(" {
                    depth += 1;
                } else if x.text == ")" {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                }
            }
            end.ok_or("unbalanced parentheses")?
        };
        match up.as_str() {
            "CONVERT" => {
                let comma = (k + 2..close).find(|&j| t[j].text == ",").ok_or("bad CONVERT")?;
                let (inner, _) = literal(t, comma + 1)?;
                return Ok((inner, close + 1));
            }
            "CAST" => {
                let (inner, _) = literal(t, k + 2)?;
                return Ok((inner, close + 1));
            }
            "REPLICATE" => {
                let (inner, next) = literal(t, k + 2)?;
                let (count, _) = literal(t, next + 1)?;
                return match (inner, count) {
                    (Lit::Text(s), Lit::Number(n)) => Ok((Lit::Text(s.repeat(n.parse().unwrap_or(0))), close + 1)),
                    _ => Err("bad replicate".into()),
                };
            }
            _ => {}
        }
    }
    Err(format!("'{}' is not a value this reader understands", tok.text))
}

/// Days since 0001-01-01 and nanoseconds since midnight of `yyyy-mm-dd[ hh:mm:ss[.fffffff]]`.
fn date_time(s: &str) -> Option<(i64, i64, Option<i32>)> {
    let s = s.trim();
    let (date, rest) = match s.split_once([' ', 'T']) {
        Some((d, r)) => (d, r.trim()),
        None if s.contains('-') => (s, ""),
        None => ("1900-01-01", s),
    };
    let mut parts = date.split('-');
    let (y, m, d): (i32, u32, u32) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    let day = NaiveDate::from_ymd_opt(y, m, d)?;
    let days = day.num_days_from_ce() as i64 - 1;
    if rest.is_empty() {
        return Some((days, 0, None));
    }
    let (time, offset) = match rest.find(['+', '-']) {
        Some(p) => (rest[..p].trim(), Some(rest[p..].trim())),
        None => (rest, None),
    };
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let sec = tp.next().unwrap_or("0");
    let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
    let s_: i64 = whole.parse().ok()?;
    let frac_ns: i64 = format!("{frac:0<9}")[..9].parse().ok()?;
    let nanos = ((h * 60 + mi) * 60 + s_) * 1_000_000_000 + frac_ns;
    let offset = match offset {
        Some(o) => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let (oh, om) = o[1..].split_once(':')?;
            Some(sign * (oh.parse::<i32>().ok()? * 60 + om.parse::<i32>().ok()?))
        }
        None => None,
    };
    Some((days, nanos, offset))
}

/// A decimal literal as digits at `scale`, rounded half away from zero.
fn decimal(n: &str, scale: u8) -> Option<i128> {
    let n = n.trim_start_matches('$');
    let (neg, n) = match n.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, n),
    };
    let (int, frac) = n.split_once('.').unwrap_or((n, ""));
    let scale = scale as usize;
    let kept: String = frac.chars().take(scale).collect();
    let digits = format!("{}{kept:0<scale$}", if int.is_empty() { "0" } else { int });
    let mut v: i128 = digits.parse().ok()?;
    if frac.len() > scale && frac.as_bytes()[scale] >= b'5' {
        v += 1;
    }
    Some(if neg { -v } else { v })
}

/// The value a live read returns for a literal stored in a column of type `t`.
fn to_value(lit: &Lit, t: &DataType) -> Option<Value> {
    if let Lit::Null = lit {
        return Some(Value::Null);
    }
    let text = |l: &Lit| match l {
        Lit::Text(s) | Lit::Number(s) => Some(s.clone()),
        _ => None,
    };
    Some(match t.name.as_str() {
        "tinyint" | "smallint" | "int" | "bigint" => Value::Int(text(lit)?.trim().parse().ok()?),
        "bit" => match text(lit)?.trim().to_ascii_lowercase().as_str() {
            "1" | "true" => Value::Bool(true),
            "0" | "false" => Value::Bool(false),
            n => Value::Bool(n.parse::<f64>().ok()? != 0.0),
        },
        "decimal" | "numeric" => {
            let s = t.scale.unwrap_or(0);
            Value::Decimal(decimal(&text(lit)?, s)?, s)
        }
        // Read as decimal(19, 4) and decimal(10, 4).
        "money" | "smallmoney" => Value::Decimal(decimal(&text(lit)?, 4)?, 4),
        "float" => Value::Float(text(lit)?.trim().parse().ok()?),
        "real" => Value::Float(text(lit)?.trim().parse::<f32>().ok()? as f64),
        "char" | "nchar" => {
            let mut s = text(lit)?;
            let n = t.length.unwrap_or(1).max(0) as usize;
            let units = s.encode_utf16().count();
            if units < n {
                s.push_str(&" ".repeat(n - units));
            }
            Value::Str(s)
        }
        "varchar" | "nvarchar" | "text" | "ntext" | "sysname" | "xml" => Value::Str(text(lit)?),
        "binary" | "varbinary" | "image" | "timestamp" => {
            let mut b = match lit {
                Lit::Bytes(b) => b.clone(),
                Lit::Text(s) => s.as_bytes().to_vec(),
                _ => return None,
            };
            if t.name == "binary" {
                let n = t.length.unwrap_or(1).max(0) as usize;
                b.resize(n.max(b.len()), 0);
            }
            Value::Bytes(b)
        }
        "uniqueidentifier" => Value::Guid(tiberius::Uuid::parse_str(text(lit)?.trim()).ok()?.to_bytes_le()),
        "date" => Value::Date(date_time(&text(lit)?)?.0),
        "time" => Value::Time(date_time(&text(lit)?)?.1),
        "datetime" => {
            // To 1/300 s ticks, as SQL Server stores it.
            let (d, n, _) = date_time(&text(lit)?)?;
            let ms = n as i128 / 1_000_000;
            let ticks = (ms * 3 + 5) / 10;
            let (d, ticks) = if ticks >= 300 * 86_400 { (d + 1, ticks - 300 * 86_400) } else { (d, ticks) };
            Value::DateTime(d as i128 * DAY + ticks * 1_000_000_000 / 300)
        }
        "smalldatetime" => {
            let (d, n, _) = date_time(&text(lit)?)?;
            let minutes = (n as i128 + 29_999_999_999) / 60_000_000_000;
            Value::DateTime(d as i128 * DAY + minutes * 60_000_000_000)
        }
        "datetime2" => {
            let (d, n, _) = date_time(&text(lit)?)?;
            Value::DateTime(d as i128 * DAY + n as i128)
        }
        "datetimeoffset" => {
            let (d, n, o) = date_time(&text(lit)?)?;
            let o = o.unwrap_or(0);
            Value::DateTimeOffset(d as i128 * DAY + n as i128 - o as i128 * 60_000_000_000, o)
        }
        _ => return None,
    })
}

impl DataFolder {
    /// Deploys data to the folder: applies the selected kinds of rows of `result` (database 1
    /// is the source, this folder database 2) and writes the changed tables' data scripts.
    pub fn write_back(&mut self, result: &crate::data::compare::DataComparison, kinds: [bool; 4]) -> Result<Vec<PathBuf>, String> {
        let mut written = Vec::new();
        for t in result.tables.iter().filter(|t| t.error.is_none() && !t.identical()) {
            let pair = &t.pair;
            let Some(table) = self.folder.database.tables.iter().find(|x| x.schema.eq_ignore_ascii_case(&pair.target.schema) && x.name.eq_ignore_ascii_case(&pair.target.name)).cloned() else {
                continue;
            };
            let entry = self.memory.entry(table.object_name()).or_insert_with(|| MemoryTable { columns: table.columns.iter().map(|c| c.name.clone()).collect(), rows: Vec::new() });
            let col = |name: &str| entry.columns.iter().position(|c| c.eq_ignore_ascii_case(name));
            let index: Vec<Option<usize>> = pair.columns.iter().map(|c| col(&c.target)).collect();
            let key_of = |values: &[Value]| -> Vec<Value> { pair.key.iter().map(|&k| values[k].clone()).collect() };
            let row_key = |row: &[Value]| -> Vec<Value> { pair.key.iter().map(|&k| index[k].map(|i| row[i].clone()).unwrap_or(Value::Null)).collect() };
            let same_key = |a: &[Value], b: &[Value]| a.iter().zip(b).all(|(x, y)| crate::data::value::cmp_key(x, y).is_eq());
            for kind in [RowKind::Different, RowKind::OnlyIn1, RowKind::OnlyIn2] {
                if !kinds[kind as usize] {
                    continue;
                }
                let offsets = &t.offsets[kind as usize];
                let rows = store::read_page(&t.path, offsets, 0, offsets.len()).map_err(|e| e.to_string())?;
                for r in rows {
                    match kind {
                        RowKind::Different => {
                            let k = key_of(&r.target);
                            if let Some(row) = entry.rows.iter_mut().find(|row| same_key(&row_key(row), &k)) {
                                for (i, ix) in index.iter().enumerate() {
                                    if let Some(ix) = ix {
                                        row[*ix] = r.source[i].clone();
                                    }
                                }
                            }
                        }
                        RowKind::OnlyIn2 => {
                            let k = key_of(&r.target);
                            entry.rows.retain(|row| !same_key(&row_key(row), &k));
                        }
                        RowKind::OnlyIn1 => {
                            let mut row = vec![Value::Null; entry.columns.len()];
                            for (i, ix) in index.iter().enumerate() {
                                if let Some(ix) = ix {
                                    row[*ix] = r.source[i].clone();
                                }
                            }
                            entry.rows.push(row);
                        }
                        RowKind::Identical => {}
                    }
                }
            }
            let keys: Vec<usize> = pair.key.iter().filter_map(|&k| index[k]).collect();
            entry.rows.sort_by(|a, b| keys.iter().map(|&k| crate::data::value::cmp_key(&a[k], &b[k])).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
            let text = script(&table, entry);
            let path = self.files.iter().find(|(n, _)| n.schema.eq_ignore_ascii_case(&table.schema) && n.name.eq_ignore_ascii_case(&table.name)).map(|(_, p)| p.clone()).unwrap_or_else(|| {
                let dir = self.folder.path.join(self.folder.info.prefixes.get("Data").map(String::as_str).unwrap_or("Data"));
                let name: String = format!("{}.{}_Data.sql", table.schema, table.name).chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
                dir.join(name)
            });
            if text.is_empty() {
                if path.exists() {
                    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&path, [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat()).map_err(|e| format!("{}: {e}", path.display()))?;
            written.push(path);
        }
        Ok(written)
    }
}

/// A table's rows as INSERT statements, as `quill data /makescripts` writes them.
fn script(table: &Table, rows: &MemoryTable) -> String {
    if rows.rows.is_empty() {
        return String::new();
    }
    let writable: Vec<(usize, &Column)> = table
        .columns
        .iter()
        .filter(|c| c.computed.is_none() && c.data_type.name != "timestamp" && c.generated_always.is_none())
        .filter_map(|c| rows.columns.iter().position(|n| n.eq_ignore_ascii_case(&c.name)).map(|i| (i, c)))
        .collect();
    let identity = table.columns.iter().any(|c| c.identity.is_some());
    let name = table.object_name().quoted();
    let cols = writable.iter().map(|(_, c)| crate::db::quote(&c.name)).collect::<Vec<_>>().join(", ");
    let mut out = String::new();
    if identity {
        out.push_str(&format!("SET IDENTITY_INSERT {name} ON\r\n"));
    }
    for row in &rows.rows {
        let list: Vec<String> = writable
            .iter()
            .map(|(i, c)| {
                let lit = Literal {
                    source: &c.data_type,
                    target: &c.data_type,
                    source_collation: c.collation.as_deref(),
                    target_collation: c.collation.as_deref(),
                    clr_as_text: false,
                    graph_table: None,
                };
                row[*i].sql(&lit)
            })
            .collect();
        out.push_str(&format!("INSERT INTO {name} ({cols}) VALUES ({})\r\n", list.join(", ")));
    }
    if identity {
        out.push_str(&format!("SET IDENTITY_INSERT {name} OFF\r\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(name: &str) -> DataType {
        DataType { name: name.into(), ..Default::default() }
    }

    #[test]
    fn literals_become_live_values() {
        let v = |s: &str, t: DataType| {
            let toks = significant(s);
            to_value(&literal(&toks, 0).unwrap().0, &t).unwrap()
        };
        assert_eq!(v("-5", ty("int")), Value::Int(-5));
        assert_eq!(v("10.00", ty("money")), Value::Decimal(100000, 4));
        assert_eq!(v("5.55", DataType { scale: Some(1), ..ty("decimal") }), Value::Decimal(56, 1));
        assert_eq!(v("N'it''s'", DataType { length: Some(6), ..ty("nchar") }), Value::Str("it's  ".into()));
        assert_eq!(v("0x0a", DataType { length: Some(2), ..ty("binary") }), Value::Bytes(vec![10, 0]));
        assert_eq!(v("CONVERT(xml,N'<a/>',1)", ty("xml")), Value::Str("<a/>".into()));
        assert_eq!(v("cast(N'ab' COLLATE x as varchar(8000))\r\n+cast(N'cd' COLLATE x as varchar(8000))", ty("varchar")), Value::Str("abcd".into()));
        // datetime keeps 1/300 s ticks: .833 is 250 ticks.
        assert_eq!(v("'0001-01-01 00:00:00.833'", ty("datetime")), Value::DateTime(250 * 1_000_000_000 / 300));
        assert_eq!(v("'0001-01-02'", ty("date")), Value::Date(1));
        assert_eq!(v("'2000-01-01 10:00:00.0000000 +02:00'", ty("datetimeoffset")), {
            let d = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap().num_days_from_ce() as i128 - 1;
            Value::DateTimeOffset(d * DAY + 8 * 3_600_000_000_000, 120)
        });
        let g = v("'6f9619ff-8b86-d011-b42d-00c04fc964ff'", ty("uniqueidentifier"));
        assert_eq!(g.display().to_ascii_lowercase(), "6f9619ff-8b86-d011-b42d-00c04fc964ff");
    }
}
