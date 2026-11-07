//! The fixture `sources_followup.sql` and the model fields it sets, listed so a source that
//! drops one of them (owners, table options, typed extended properties…) fails even where the
//! comparison would let it pass.

use quillsql::catalog::Database;

pub const FOLLOWUP: &str = include_str!("../fixtures/compare/sources_followup.sql");

/// One line per feature value of `db`, sorted.
pub fn features(db: &Database) -> Vec<String> {
    let mut out = Vec::new();
    let own = |o: &Option<String>| o.clone().unwrap_or_default();
    for t in &db.tables {
        out.push(format!("table {}.{} owner={} lvt={} filetable={:?} stretch={} history={}", t.schema, t.name, own(&t.owner), t.large_value_types_out_of_row, t.file_table, t.remote_data_archive, t.history_table));
        for tr in &t.triggers {
            out.push(format!("trigger {}.{} ansi={} qi={}", t.schema, tr.name, tr.settings.ansi_nulls, tr.settings.quoted_identifier));
        }
        for ix in &t.indexes {
            out.push(format!("index {}.{}.{}", t.schema, t.name, ix.name));
        }
    }
    for v in &db.views {
        out.push(format!("view {}.{} owner={}", v.schema, v.name, own(&v.owner)));
    }
    for r in &db.routines {
        out.push(format!("routine {}.{} owner={}", r.schema, r.name, own(&r.owner)));
    }
    for t in &db.types {
        out.push(format!("type {}.{} owner={} default={} rule={}", t.schema, t.name, own(&t.owner), own(&t.bound_default), own(&t.bound_rule)));
    }
    for s in &db.sequences {
        out.push(format!("sequence {}.{} owner={}", s.schema, s.name, own(&s.owner)));
    }
    for s in &db.synonyms {
        out.push(format!("synonym {}.{} owner={}", s.schema, s.name, own(&s.owner)));
    }
    for p in &db.security_policies {
        out.push(format!("policy {}.{} owner={}", p.schema, p.name, own(&p.owner)));
    }
    for p in &db.extended_properties {
        out.push(format!("xprop {} {:?} {:?} {:?} {:?} {} = {:?} ({:?})", p.class, p.schema, p.object, p.minor_type, p.minor_name, p.name, p.value, p.value_type));
    }
    // The grants every database gives public are not compared, and snapshots leave them out.
    for p in db.permissions.iter().filter(|p| !(p.grantee == "public" && p.permission.starts_with("VIEW ANY COLUMN "))) {
        out.push(format!("perm {} {:?} {:?} {:?} {} {} {}", p.class, p.schema, p.object, p.column, p.grantee, p.permission, p.state));
    }
    for p in &db.principals {
        out.push(format!("principal {} {} member_of={:?}", p.kind, p.name, p.member_of));
    }
    out.sort();
    out
}

/// Features of `a` that `b` lacks and the other way round.
pub fn assert_same_features(a: &Database, b: &Database, what: &str) {
    let (fa, fb) = (features(a), features(b));
    let only_a: Vec<_> = fa.iter().filter(|x| !fb.contains(x)).collect();
    let only_b: Vec<_> = fb.iter().filter(|x| !fa.contains(x)).collect();
    assert!(only_a.is_empty() && only_b.is_empty(), "{what}:\nonly in the first: {only_a:#?}\nonly in the second: {only_b:#?}");
}
