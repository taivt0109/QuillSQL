//! Table rename detection: tables only in database 1 that look like a renamed table only in
//! database 2. SQL Compare's three tests (similar names, similar CREATE TABLE scripts, identical
//! but for the name) make suggestions the user confirms as table mappings; unlike SQL Compare,
//! each table is suggested for at most one partner, best match first.

use serde::Serialize;

use super::compare::{Mappings, ObjectType};
use super::ddl;
use crate::catalog::{Database, ObjectName, Table};

/// One suggested mapping.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rename {
    pub source: ObjectName,
    pub target: ObjectName,
    /// `identical` (only the name differs), `script` (similar definitions) or `name` (similar names).
    pub reason: &'static str,
    /// 0–100, how alike the two CREATE TABLE scripts are.
    pub similarity: u32,
}

/// Scripts longer than this are not compared character by character.
const MAX_SCRIPT: usize = 20_000;

pub fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, x) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(x != y)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The bare names are alike: one starts or ends with the other, or they are at most 5 edits apart.
fn similar_names(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let (ca, cb): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    a.starts_with(b) || b.starts_with(a) || a.ends_with(b) || b.ends_with(a) || levenshtein(&ca, &cb) <= 5
}

/// Tables of `db1` missing from `db2` paired with tables of `db2` missing from `db1`, leaving
/// out tables the mappings already pair.
pub fn detect(db1: &Database, db2: &Database, mappings: &Mappings) -> Vec<Rename> {
    let mapped1 = |t: &Table| mappings.tables.iter().any(|m| m.source == t.object_name());
    let mapped2 = |t: &Table| mappings.tables.iter().any(|m| m.target == t.object_name());
    let only1: Vec<&Table> = db1.tables.iter().filter(|t| db2.table(&t.schema, &t.name).is_none() && !mapped1(t)).collect();
    let only2: Vec<&Table> = db2.tables.iter().filter(|t| db1.table(&t.schema, &t.name).is_none() && !mapped2(t)).collect();
    if only1.is_empty() || only2.is_empty() {
        return Vec::new();
    }
    // The whole table as SQL Compare scripts it (keys, indexes, constraints too), not only its
    // columns: small tables of a few nvarchar columns would otherwise all look alike.
    let script = |db: &Database, t: &Table| super::ddl_objects::object_sql(db, ObjectType::Table, &t.object_name()).unwrap_or_else(|| ddl::create_table(t, None, false));
    let scripts1: Vec<String> = only1.iter().map(|t| script(db1, t)).collect();
    let scripts2: Vec<String> = only2.iter().map(|t| script(db2, t)).collect();
    let mut found = Vec::new();
    for (a, s1) in only1.iter().zip(&scripts1) {
        for (b, s2) in only2.iter().zip(&scripts2) {
            // Identical but for the name: the target's script under the source's name.
            let renamed = s2.replace(&b.object_name().quoted(), &a.object_name().quoted());
            let similarity = if renamed == *s1 {
                100
            } else if s1.len() <= MAX_SCRIPT && s2.len() <= MAX_SCRIPT {
                let (c1, c2): (Vec<char>, Vec<char>) = (s1.chars().collect(), s2.chars().collect());
                let d = levenshtein(&c1, &c2);
                100 - (d * 100 / c1.len().max(1)).min(100) as u32
            } else {
                0
            };
            let reason = if similarity == 100 {
                "identical"
            } else if similarity >= 75 {
                "script"
            } else if similar_names(&a.name, &b.name) {
                "name"
            } else {
                continue;
            };
            found.push(Rename { source: a.object_name(), target: b.object_name(), reason, similarity });
        }
    }
    // Best first, each table once.
    found.sort_by(|x, y| y.similarity.cmp(&x.similarity).then_with(|| x.source.to_string().cmp(&y.source.to_string())));
    let mut out: Vec<Rename> = Vec::new();
    for r in found {
        if !out.iter().any(|o| o.source == r.source || o.target == r.target) {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Column, DataType};

    fn table(schema: &str, name: &str, cols: &[(&str, &str)]) -> Table {
        Table {
            schema: schema.into(),
            name: name.into(),
            columns: cols.iter().map(|(c, ty)| Column { name: (*c).into(), data_type: DataType { name: (*ty).into(), ..Default::default() }, nullable: true, ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn suggests_one_partner_each() {
        let mut db1 = Database::default();
        let mut db2 = Database::default();
        db1.tables.push(table("dbo", "Customer", &[("Id", "int"), ("Name", "nvarchar")]));
        db1.tables.push(table("dbo", "Orders", &[("Id", "int"), ("Total", "money"), ("When", "datetime")]));
        db1.tables.push(table("dbo", "Same", &[("Id", "int")]));
        db2.tables.push(table("dbo", "Customers", &[("Id", "int"), ("Name", "nvarchar")]));
        db2.tables.push(table("sales", "OrdersOld", &[("Id", "int"), ("Total", "money"), ("When", "datetime"), ("Note", "ntext")]));
        db2.tables.push(table("dbo", "Same", &[("Id", "int")]));
        db2.tables.push(table("dbo", "Unrelated", &[("Code", "char"), ("Description", "xml"), ("Flags", "bigint")]));
        let r = detect(&db1, &db2, &Mappings::default());
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!((r[0].source.name.as_str(), r[0].target.name.as_str(), r[0].reason), ("Customer", "Customers", "identical"));
        assert_eq!((r[1].source.name.as_str(), r[1].target.name.as_str()), ("Orders", "OrdersOld"));
        let mapped = Mappings { tables: vec![crate::schema::TableMapping { source: ObjectName::new("dbo", "Customer"), target: ObjectName::new("dbo", "Customers"), columns: vec![] }], ..Default::default() };
        assert_eq!(detect(&db1, &db2, &mapped).len(), 1);
        assert_eq!(levenshtein(&['a', 'b', 'c'], &['a', 'x', 'c', 'd']), 2);
    }
}
