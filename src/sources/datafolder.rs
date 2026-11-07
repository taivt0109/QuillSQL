//! The data of a scripts folder, read without a server: the folder's table definitions and the
//! INSERT statements of its data scripts become rows in memory, in the form a live read gives,
//! for the data engine to compare. Deploying data to the folder writes the data scripts again.
//!
//! As SQL Data Compare reads them, the scripts only hold literals and nothing needs a server:
//! computed columns are not read (the comparison leaves them out), CLR, hierarchyid and spatial
//! values are `0x…` binary, sql_variant values take their base type from the literal or its CAST,
//! columns missing from an INSERT are NULL, and any other SQL in a VALUES list is an error.

use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate};

use super::folder::{self, Folder};
use crate::catalog::{Column, DataType, Database, ObjectName, Table, UserTypeKind};
use crate::data::compare::{Memory, MemoryTable};
use crate::data::store::{self, RowKind};
use crate::data::value::{Literal, Value, Variant};
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
    /// A string; true when written N'…'.
    Text(String, bool),
    Bytes(Vec<u8>),
    /// CAST(x AS type(args)) or CONVERT(type(args), x), and any COLLATE after it.
    Cast(Box<Lit>, String, Vec<String>, Option<String>),
    /// A literal with COLLATE.
    Collated(Box<Lit>, String),
}

/// The data script files of a folder: the top level of its Data folder and the files listed in
/// its DataFileSet.
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

/// Reads the folder at `dir` (at `revision`, if given) and the rows of its data scripts.
pub fn read(dir: &Path, revision: Option<&str>) -> Result<DataFolder, String> {
    let checkout = revision.map(|r| super::scm::checkout(&dir.to_string_lossy(), r)).transpose()?;
    let read_dir = checkout.as_ref().map(|c| c.dir.clone()).unwrap_or_else(|| dir.to_path_buf());
    let mut f = folder::read(&read_dir)?;
    if checkout.is_some() {
        f.database.name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(f.database.name);
    }
    let mut memory = Memory::new();
    let mut files = Vec::new();
    let collation = f.database.collation.clone();
    for path in data_files(&f) {
        let text = folder::decode(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?, &f.info.encoding);
        let tables = parse_file(&f.database, &f.info.default_schema, &collation, &text)
            .map_err(|reason| format!("One or more data SQL files could not be parsed. {reason} File: {}", path.display()))?;
        for (name, t) in tables {
            files.push((name.clone(), path.clone()));
            let entry = memory.entry(name).or_insert_with(|| MemoryTable { columns: t.columns.clone(), rows: Vec::new() });
            entry.rows.extend(t.rows);
        }
    }
    Ok(DataFolder { folder: f, memory, files, _checkout: checkout })
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

/// CLR user-defined types, hierarchyid and the spatial types: written and read as binary.
fn is_clr(t: &DataType) -> bool {
    t.schema.is_some() || matches!(t.name.as_str(), "hierarchyid" | "geometry" | "geography")
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

/// The INSERT statements of one file, by table. Large values may also be built up by
/// `UPDATE t SET col.WRITE(…)` and `UPDATETEXT t.col @ptr NULL NULL …` on the row just inserted,
/// and statements may be wrapped in `EXEC(N'…')`.
fn parse_file(db: &Database, default_schema: &str, collation: &str, text: &str) -> Result<Vec<(ObjectName, MemoryTable)>, String> {
    let mut out: Vec<(ObjectName, MemoryTable)> = Vec::new();
    parse_into(db, default_schema, collation, text, &mut out)?;
    Ok(out)
}

fn parse_into(db: &Database, default_schema: &str, collation: &str, text: &str, out: &mut Vec<(ObjectName, MemoryTable)>) -> Result<(), String> {
    let t = significant(text);
    let mut k = 0;
    while k < t.len() {
        let w = t[k].text.to_ascii_uppercase();
        if (w == "EXEC" || w == "EXECUTE") && t.get(k + 1).is_some_and(|x| x.text == "(") {
            let (inner, next) = literal(&t, k + 2)?;
            let Lit::Text(sql, _) = inner else { return Err("Expected a value or NULL".into()) };
            parse_into(db, default_schema, collation, &sql, out)?;
            k = next + 1;
            continue;
        }
        if w == "UPDATE" || w == "UPDATETEXT" {
            k = append_chunk(db, default_schema, collation, &t, k, out)?;
            continue;
        }
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
            let table = find_table(db, &name)?;
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
                for (ordinal, c) in all.iter().enumerate() {
                    let v = match columns.iter().position(|n| n.eq_ignore_ascii_case(&c.name)) {
                        // Computed columns are not compared with a scripts folder, so not read.
                        _ if c.computed.is_some() => Value::Null,
                        Some(i) => column_value(&row[i], base_type(db, &c.data_type), collation)
                            .map_err(|e| format!("{e} on column {ordinal} ({}). Table affected: {name}.", c.name))?,
                        // A column left out of the INSERT is NULL (defaults are not worked out).
                        None => Value::Null,
                    };
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
        // SET IDENTITY_INSERT, GO, DECLARE @ptr … and the like.
        k += 1;
    }
    Ok(())
}

fn find_table<'a>(db: &'a Database, name: &ObjectName) -> Result<&'a Table, String> {
    db.tables
        .iter()
        .find(|x| x.schema.eq_ignore_ascii_case(&name.schema) && x.name.eq_ignore_ascii_case(&name.name))
        .ok_or_else(|| format!("There is no table {name} in the scripts folder."))
}

/// A dotted name at token `k`: its parts and the next token index.
fn dotted(t: &[Token], mut k: usize) -> (Vec<String>, usize) {
    let mut parts = Vec::new();
    if let Some(x) = t.get(k) {
        parts.push(unquote(&x.text));
        k += 1;
    }
    while t.get(k).is_some_and(|x| x.text == ".") && t.get(k + 1).is_some() {
        parts.push(unquote(&t[k + 1].text));
        k += 2;
    }
    (parts, k)
}

/// `UPDATE t SET col.WRITE(value, …) …` or `UPDATETEXT t.col @ptr NULL NULL value`: the value is
/// appended to that column of the row the previous INSERT added. Returns the next token index.
fn append_chunk(db: &Database, default_schema: &str, collation: &str, t: &[Token], k: usize, out: &mut [(ObjectName, MemoryTable)]) -> Result<usize, String> {
    let update = t[k].text.eq_ignore_ascii_case("UPDATE");
    let (parts, mut k) = dotted(t, k + 1);
    let (table, column) = if update {
        if !t.get(k).is_some_and(|x| x.text.eq_ignore_ascii_case("SET")) {
            return Err("Expected SET".into());
        }
        let (target, next) = dotted(t, k + 1);
        k = next;
        let [column, method] = target.as_slice() else { return Err("Expected a column".into()) };
        if !method.eq_ignore_ascii_case("WRITE") {
            return Err(format!("Unsupported update method {method} found in data"));
        }
        k += 1; // (
        (parts, column.clone())
    } else {
        let Some((column, table)) = parts.split_last() else { return Err("Expected a column".into()) };
        // The text pointer, then the offset and length (NULL NULL appends).
        k += 3;
        (table.to_vec(), column.clone())
    };
    let name = match table.as_slice() {
        [n] => ObjectName::new(default_schema, n),
        [.., s, n] => ObjectName::new(s, n),
        [] => return Err("Expected a table".into()),
    };
    let (chunk, next) = literal(t, k)?;
    let target = find_table(db, &name)?;
    let Some((_, mt)) = out.last_mut().filter(|(n, _)| *n == name) else {
        return Err("Update must refer to the row inserted by the previous INSERT statement".into());
    };
    let i = mt.columns.iter().position(|c| c.eq_ignore_ascii_case(&column)).ok_or_else(|| format!("There is no column {column} in {name}."))?;
    let ty = target.columns.iter().find(|c| c.name.eq_ignore_ascii_case(&column)).map(|c| base_type(db, &c.data_type).clone()).unwrap_or_default();
    let add = column_value(&chunk, &ty, collation)?;
    let row = mt.rows.last_mut().ok_or("Update must refer to the row inserted by the previous INSERT statement")?;
    row[i] = match (std::mem::replace(&mut row[i], Value::Null), add) {
        (Value::Str(a), Value::Str(b)) => Value::Str(a + &b),
        (Value::Bytes(mut a), Value::Bytes(b)) => {
            a.extend(b);
            Value::Bytes(a)
        }
        (Value::Null, b) => b,
        (a, _) => a,
    };
    // Skip the rest of the statement (the .WRITE arguments and the WHERE clause).
    let mut k = next;
    while k < t.len() && !matches!(t[k].text.to_ascii_uppercase().as_str(), "INSERT" | "UPDATE" | "UPDATETEXT" | "GO" | "EXEC" | "EXECUTE" | "SET") {
        k += 1;
    }
    Ok(k)
}

fn all_columns(t: &Table) -> Vec<&Column> {
    t.columns.iter().collect()
}

/// The value of a literal in a column of type `t`, or SQL Data Compare's complaint about it.
fn column_value(lit: &Lit, t: &DataType, collation: &str) -> Result<Value, String> {
    if t.schema.is_none() && t.name == "sql_variant" {
        return Ok(variant(lit, collation));
    }
    if is_clr(t) {
        return match strip_cast(lit) {
            Lit::Null => Ok(Value::Null),
            Lit::Bytes(b) => Ok(Value::Clr(b.clone(), None)),
            other => Err(format!("Invalid token type for reading binary (expected HexLiteral but got {})", kind_name(other))),
        };
    }
    to_value(strip_cast(lit), t).ok_or_else(|| format!("Invalid value {} for {}", kind_name(lit), t.sql()))
}

fn strip_cast(lit: &Lit) -> &Lit {
    match lit {
        Lit::Cast(inner, ..) | Lit::Collated(inner, _) => strip_cast(inner),
        other => other,
    }
}

fn kind_name(lit: &Lit) -> &'static str {
    match lit {
        Lit::Null => "Null",
        Lit::Number(_) => "Number",
        Lit::Text(..) => "String",
        Lit::Bytes(_) => "HexLiteral",
        Lit::Cast(inner, ..) | Lit::Collated(inner, _) => kind_name(inner),
    }
}

/// A sql_variant literal: the base type comes from its CAST or CONVERT, else from the literal
/// (an integer is numeric, `0x…` image, `'…'` text, `N'…'` ntext, `$…` money; a float has no
/// base type and reads as NULL). The text is the one a live read gives (CONVERT style 121 for
/// dates, 3 for floats, 2 for money, upper-case `0x…` for binary).
fn variant(lit: &Lit, default_collation: &str) -> Value {
    let (inner, ty, args, collation) = match lit {
        Lit::Null => return Value::Null,
        Lit::Cast(inner, ty, args, collation) => (strip_cast(inner), Some(ty.clone()), args.clone(), collation.clone()),
        Lit::Collated(inner, c) => (strip_cast(inner), None, Vec::new(), Some(c.clone())),
        other => (other, None, Vec::new(), None),
    };
    let raw = match inner {
        Lit::Null => return Value::Null,
        Lit::Number(n) => n.clone(),
        Lit::Text(s, _) => s.clone(),
        Lit::Bytes(b) => format!("0x{}", b.iter().map(|x| format!("{x:02X}")).collect::<String>()),
        Lit::Cast(..) | Lit::Collated(..) => return Value::Null,
    };
    let base = match ty {
        Some(t) => t,
        None => match inner {
            Lit::Number(n) if n.starts_with('$') || n.starts_with("-$") => "money".into(),
            Lit::Number(n) if n.contains(['.', 'e', 'E']) => return Value::Null,
            Lit::Number(_) => "numeric".into(),
            Lit::Bytes(_) => "image".into(),
            Lit::Text(_, true) => "ntext".into(),
            _ => "text".into(),
        },
    };
    let arg = |i: usize| args.get(i).and_then(|a| a.parse::<i32>().ok());
    let (mut precision, mut scale, mut max_length) = (0u8, 0u8, 0i32);
    let text = match base.as_str() {
        "float" | "real" => {
            let f: f64 = raw.trim().parse().unwrap_or_default();
            scientific(f, if base == "real" { 9 } else { 17 })
        }
        "decimal" | "numeric" | "money" | "smallmoney" => {
            let s = match base.as_str() {
                "money" | "smallmoney" => 4,
                _ => arg(1).unwrap_or_else(|| raw.split_once('.').map_or(0, |(_, f)| f.len() as i32)),
            };
            precision = arg(0).unwrap_or(18) as u8;
            scale = s as u8;
            let digits = decimal(&raw, s as u8).unwrap_or_default();
            let neg = digits < 0;
            let abs = digits.unsigned_abs().to_string();
            let s = s as usize;
            let padded = format!("{abs:0>width$}", width = s + 1);
            let (int, frac) = padded.split_at(padded.len() - s);
            format!("{}{int}{}{frac}", if neg { "-" } else { "" }, if s > 0 { "." } else { "" })
        }
        "binary" | "varbinary" | "image" => {
            let t = raw.to_ascii_uppercase().replacen("0X", "0x", 1);
            max_length = arg(0).unwrap_or(((t.len().saturating_sub(2)) / 2) as i32);
            t
        }
        "uniqueidentifier" => raw.trim_matches(['{', '}']).to_ascii_uppercase(),
        "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" => {
            let n = arg(0).unwrap_or(raw.encode_utf16().count() as i32);
            max_length = if base.starts_with('n') { n * 2 } else { n };
            raw
        }
        _ => raw,
    };
    let character = matches!(base.as_str(), "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext");
    let collation = character.then(|| collation.unwrap_or_else(|| default_collation.to_string()));
    Value::Variant(Box::new(Variant { base_type: base, precision, scale, max_length, collation, text }))
}

/// `f` as CONVERT style 3 writes it: `digits` significant digits, `e±nnn`.
fn scientific(f: f64, digits: usize) -> String {
    let s = format!("{:.*e}", digits - 1, f);
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    format!("{mantissa}e{}{:03}", if e < 0 { '-' } else { '+' }, e.abs())
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
            let c = t.get(k + 1).map(|x| unquote(&x.text)).unwrap_or_default();
            lit = match lit {
                Lit::Cast(inner, ty, args, _) => Lit::Cast(inner, ty, args, Some(c)),
                Lit::Collated(inner, _) => Lit::Collated(inner, c),
                other => Lit::Collated(Box::new(other), c),
            };
            k += 2;
            continue;
        }
        if t.get(k).is_some_and(|x| x.text == "+") {
            let (rhs, next) = term(t, k + 1)?;
            // Pieces are joined as text, as SQL Data Compare does (1+2 reads as 12).
            let piece = |l: Lit| match strip_cast(&l) {
                Lit::Text(s, n) => Some((s.clone(), *n)),
                Lit::Number(s) => Some((s.clone(), false)),
                _ => None,
            };
            lit = match (piece(lit), piece(rhs)) {
                (Some((a, n)), Some((b, m))) => Lit::Text(a + &b, n || m),
                _ => return Err("Expected a value or NULL".into()),
            };
            k = next;
            continue;
        }
        return Ok((lit, k));
    }
}

fn term(t: &[Token], k: usize) -> Result<(Lit, usize), String> {
    let tok = t.get(k).ok_or("Expected a value or NULL")?;
    let up = tok.text.to_ascii_uppercase();
    match tok.kind {
        Tok::Str => return Ok((Lit::Text(string_value(&tok.text), tok.text.starts_with(['N', 'n'])), k + 1)),
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
                _ => Err("Expected a value or NULL".into()),
            };
        }
        Tok::Punct if tok.text == "(" => {
            let (inner, next) = literal(t, k + 1)?;
            if !t.get(next).is_some_and(|x| x.text == ")") {
                return Err("Expected )".into());
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
            end.ok_or("Expected )")?
        };
        // A type at `at` with its arguments: (name, args, next index).
        let type_at = |at: usize| -> (String, Vec<String>, usize) {
            let name = t.get(at).map(|x| unquote(&x.text).to_ascii_lowercase()).unwrap_or_default();
            let mut args = Vec::new();
            let mut j = at + 1;
            if t.get(j).is_some_and(|x| x.text == "(") {
                j += 1;
                while j < close && t[j].text != ")" {
                    if t[j].text != "," {
                        args.push(t[j].text.to_ascii_lowercase());
                    }
                    j += 1;
                }
                j += 1;
            }
            (name, args, j)
        };
        let constant = |l: &Lit| !matches!(l, Lit::Cast(..));
        // A COLLATE inside the CAST is the value's collation.
        let split = |l: Lit| match l {
            Lit::Collated(inner, c) => (*inner, Some(c)),
            other => (other, None),
        };
        match up.as_str() {
            "CONVERT" => {
                let (ty, args, after) = type_at(k + 2);
                if !t.get(after).is_some_and(|x| x.text == ",") {
                    return Err("Expected ,".into());
                }
                let (inner, _) = literal(t, after + 1)?;
                if !constant(&inner) {
                    return Err("Expected a value or NULL".into());
                }
                let (inner, c) = split(inner);
                return Ok((Lit::Cast(Box::new(inner), ty, args, c), close + 1));
            }
            "CAST" => {
                let (inner, next) = literal(t, k + 2)?;
                if !constant(&inner) || !t.get(next).is_some_and(|x| x.text.eq_ignore_ascii_case("AS")) {
                    return Err("Expected a value or NULL".into());
                }
                let (ty, args, _) = type_at(next + 1);
                let (inner, c) = split(inner);
                return Ok((Lit::Cast(Box::new(inner), ty, args, c), close + 1));
            }
            "REPLICATE" => {
                let (inner, next) = literal(t, k + 2)?;
                let (count, _) = literal(t, next + 1)?;
                return match (split(inner).0, count) {
                    (Lit::Text(s, n), Lit::Number(c)) => Ok((Lit::Text(s.repeat(c.parse().unwrap_or(0)), n), close + 1)),
                    _ => Err("Expected a value or NULL".into()),
                };
            }
            _ => return Err(format!("Found invalid function {} - only replicate is allowed in data", tok.text)),
        }
    }
    Err(format!("Expected a value or NULL. Found {}", tok.text))
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
        Lit::Text(s, _) | Lit::Number(s) => Some(s.clone()),
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
                Lit::Text(s, _) => s.as_bytes().to_vec(),
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
    /// Deploys data to the folder: applies the selected rows of `result` (database 1 is the
    /// source, this folder database 2) and writes the changed tables' data scripts. `select`
    /// says whether a row (its kind and position within the kind) of a pair is deployed; rows
    /// left out keep their old values.
    pub fn write_back(&mut self, result: &crate::data::compare::DataComparison, select: &dyn Fn(&crate::data::mapping::TablePair, RowKind, u64) -> bool) -> Result<Vec<PathBuf>, String> {
        let mut written = Vec::new();
        for t in result.tables.iter().filter(|t| t.error.is_none() && !t.identical()) {
            let pair = &t.pair;
            let chosen = |kind: RowKind| (0..t.offsets[kind as usize].len() as u64).any(|i| select(pair, kind, i));
            if ![RowKind::Different, RowKind::OnlyIn1, RowKind::OnlyIn2].into_iter().any(chosen) {
                continue;
            }
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
                let offsets = &t.offsets[kind as usize];
                let rows = store::read_page(&t.path, offsets, 0, offsets.len()).map_err(|e| e.to_string())?;
                for (i, r) in rows.into_iter().enumerate() {
                    if !select(pair, kind, i as u64) {
                        continue;
                    }
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
            column_value(&literal(&toks, 0).unwrap().0, &t, "Latin1_General_CI_AS").unwrap()
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
        // CLR, hierarchyid and spatial values are binary.
        assert_eq!(v("0x58", ty("hierarchyid")), Value::Clr(vec![0x58], None));
        let toks = significant("N'/1/'");
        assert!(column_value(&literal(&toks, 0).unwrap().0, &ty("hierarchyid"), "x").unwrap_err().contains("expected HexLiteral"));
        // sql_variant: the base type from the CAST, the text as a live read gives it.
        let var = |s: &str| match v(s, ty("sql_variant")) {
            Value::Variant(x) => (x.base_type, x.text, x.max_length, x.collation),
            other => panic!("{other:?}"),
        };
        assert_eq!(var("CAST(N'hi' AS nvarchar(5)) COLLATE Latin1_General_CS_AS"), ("nvarchar".into(), "hi".into(), 10, Some("Latin1_General_CS_AS".into())));
        assert_eq!(var("CAST(1.5 AS float)").1, "1.5000000000000000e+000");
        assert_eq!(var("CAST(1.5 AS decimal(4,2))").1, "1.50");
        assert_eq!(var("CAST(0x0a0b AS varbinary(2))").1, "0x0A0B");
        assert_eq!(var("CAST('2020-01-01 00:00:00.000' AS datetime)").1, "2020-01-01 00:00:00.000");
        assert_eq!(var("5").0, "numeric");
        assert_eq!(var("N'x'"), ("ntext".into(), "x".into(), 2, Some("Latin1_General_CI_AS".into())));
        assert_eq!(v("1.5", ty("sql_variant")), Value::Null);
    }

    #[test]
    fn non_literal_sql_and_appended_chunks() {
        let db = Database {
            tables: vec![Table {
                schema: "dbo".into(),
                name: "T".into(),
                columns: vec![
                    Column { name: "Id".into(), data_type: ty("int"), ..Default::default() },
                    Column { name: "Body".into(), data_type: DataType { length: Some(-1), ..ty("nvarchar") }, ..Default::default() },
                    Column { name: "Stamp".into(), data_type: ty("datetime"), default: Some(Default::default()), ..Default::default() },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = parse_file(&db, "dbo", "x", "INSERT INTO [dbo].[T] ([Id], [Body], [Stamp]) VALUES (1, N'a', GETDATE())").unwrap_err();
        assert!(err.contains("Found invalid function GETDATE - only replicate is allowed in data"), "{err}");
        let text = "INSERT INTO [dbo].[T] ([Id], [Body]) VALUES (1, N'ab')\r\nUPDATE [dbo].[T] SET [Body].WRITE(N'cd', NULL, NULL) WHERE [Id] = 1\r\n\
EXEC(N'INSERT INTO [dbo].[T] ([Id], [Body]) VALUES (2, N''x'')')\r\n";
        let t = parse_file(&db, "dbo", "x", text).unwrap();
        let rows = &t[0].1.rows;
        // A column left out of the INSERT is NULL.
        assert_eq!(rows[0], vec![Value::Int(1), Value::Str("abcd".into()), Value::Null]);
        assert_eq!(rows[1][1], Value::Str("x".into()));
    }
}
