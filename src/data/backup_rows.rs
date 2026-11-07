//! One side of a data comparison read from a backup: the rows a server would return for the
//! comparison's SELECT, computed in the client and put in the same key order.

use super::compare::{LARGE, Server, Shape, cmp_keys, family, key_fold, value_exprs};
use super::mapping::TablePair;
use super::options::DataOptions;
use super::value::{Fold, Value};
use crate::catalog::DataType;
use crate::sources::backup::rows::{self, Want};
use crate::sources::backup::values::DAY;
use crate::sources::backup::{Backup, guid_text};

/// The rows of side `side` (1 or 2) of the pair: the key values as the server would sort them,
/// then the compared values, sorted by key.
pub fn sorted(backup: &Backup, pair: &TablePair, side: u8, opts: &DataOptions, server: Server, keys: usize) -> Result<Vec<Vec<Value>>, String> {
    let (table, filter) = if side == 1 { (&pair.source, &pair.where1) } else { (&pair.target, &pair.where2) };
    if filter.as_deref().is_some_and(|w| !w.trim().is_empty()) {
        return Err("a WHERE clause cannot be applied to a backup".into());
    }
    let name = |i: usize| if side == 1 { &pair.columns[i].source } else { &pair.columns[i].target };
    let ty = |i: usize| if side == 1 { &pair.columns[i].source_type } else { &pair.columns[i].target_type };
    let other = |i: usize| if side == 1 { &pair.columns[i].target_type } else { &pair.columns[i].source_type };
    let cat = backup.catalog().map_err(|e| e.to_string())?;
    let object = rows::object_id(&cat, &table.schema, &table.name).ok_or_else(|| format!("{} was not found in the backup", table.quoted()))?;
    let mut wanted: Vec<(String, Want)> = Vec::new();
    let mut shapes = Vec::new();
    for i in 0..pair.columns.len() {
        let shape = value_exprs(name(i), ty(i), server.hash).1;
        let want = if shape == Shape::Lob { Want::Hashed(LARGE) } else { Want::Value };
        wanted.push((name(i).clone(), want));
        shapes.push(shape);
    }
    let read = rows::read(&cat, object, &wanted)?;
    let mut out: Vec<Vec<Value>> = read
        .into_iter()
        .map(|row| {
            let mut v: Vec<Value> = pair.key.iter().map(|&k| sort_value(&row[k], ty(k), other(k), key_fold(&pair.columns[k], opts), server)).collect();
            v.extend(row);
            v
        })
        .collect();
    out.sort_by(|a, b| cmp_keys(a, b, keys));
    Ok(out)
}

/// SQL Server's UPPER: characters whose upper case is a single character.
fn upper(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut u = c.to_uppercase();
            match (u.next(), u.next()) {
                (Some(x), None) => x,
                _ => c,
            }
        })
        .collect()
}

/// A key value as the comparison's ORDER BY expression gives it (see `sort_expr`).
fn sort_value(v: &Value, this: &DataType, other: &DataType, fold: Fold, server: Server) -> Value {
    let (f1, f2) = (family(this), family(other));
    let text = f1 == "string" || (f2 == "string" && matches!(f1, "int" | "guid"));
    if text {
        let s = match v {
            Value::Null => return Value::Null,
            Value::Str(s) => s.clone(),
            Value::Guid(g) => guid_text(g).to_uppercase(),
            other => other.display(),
        };
        let mut s: String = s.chars().take(4000).collect::<String>().trim_end_matches(' ').to_string();
        if fold.case {
            s = upper(&s);
        }
        let (from, to) = Fold { case: false, ..fold }.table();
        if server.translate && !from.is_empty() {
            let map: std::collections::HashMap<char, char> = from.chars().zip(to.chars()).collect();
            s = s.chars().map(|c| *map.get(&c).unwrap_or(&c)).collect();
        }
        return Value::Str(s);
    }
    if f1 == "date" && f2 == "date" && this.name != other.name {
        return match *v {
            Value::Date(d) => Value::DateTime(d as i128 * DAY),
            Value::Time(t) => Value::DateTime(693_595 * DAY + t as i128),
            Value::DateTimeOffset(utc, offset) => Value::DateTime(utc + offset as i128 * 60_000_000_000),
            ref other => other.clone(),
        };
    }
    v.clone()
}
