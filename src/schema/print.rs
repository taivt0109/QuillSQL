//! Console output for `quill schema`: a summary table, then the property differences of each
//! object as an indented tree.

use std::fmt::Write;

use super::compare::{DiffKind, Difference, Frame};

pub fn summary(diffs: &[&Difference], db1: &str, db2: &str) -> String {
    let mut out = String::new();
    let width = diffs.iter().map(|d| display_name(d).chars().count()).max().unwrap_or(4).clamp(4, 70);
    let _ = writeln!(out, "{:<18} {:<width$}  {:^4}  {db1} → {db2}", "Object type", "Name", "");
    for d in diffs {
        let what = match d.kind {
            DiffKind::Equal => "identical",
            DiffKind::Different => "different",
            DiffKind::OnlyIn1 => "only in database 1",
            DiffKind::OnlyIn2 => "only in database 2",
        };
        let _ = writeln!(out, "{:<18} {:<width$}  {:^4}  {what}", d.object_type.label(), display_name(d), d.kind.marker());
    }
    out
}

fn display_name(d: &Difference) -> String {
    let name = |n: &crate::catalog::ObjectName| {
        if n.schema.is_empty() { crate::db::quote(&n.name) } else { n.to_string() }
    };
    match &d.target_name {
        Some(t) => format!("{} → {}", name(&d.name), name(t)),
        None => name(&d.name),
    }
}

fn value(v: &Option<String>) -> String {
    match v {
        Some(s) if s.contains('\n') => format!("{:?}", s),
        Some(s) => s.clone(),
        None => "(none)".into(),
    }
}

fn frame(f: &Frame) -> String {
    match (&f.source, &f.target) {
        (Some(a), Some(b)) if a == b => format!("{} {a}", f.name),
        (a, b) => format!("{} {} / {}", f.name, value(a), value(b)),
    }
}

/// The property differences of one object as an indented tree.
pub fn details(d: &Difference) -> String {
    let mut out = format!("{} {} {}\n", d.kind.marker(), d.object_type.label(), display_name(d));
    let mut printed: Vec<&Frame> = Vec::new();
    for p in &d.properties {
        let common = printed.iter().zip(&p.location).take_while(|(a, b)| **a == *b).count();
        printed.truncate(common);
        for f in &p.location[common..] {
            let _ = writeln!(out, "{}{}", "   ".repeat(printed.len() + 1), frame(f));
            printed.push(f);
        }
        let indent = "   ".repeat(printed.len() + 1);
        let line = match (&p.source, &p.target) {
            (Some(a), None) => format!("{} {} only in database 1", p.name, a),
            (None, Some(b)) => format!("{} {} only in database 2", p.name, b),
            (a, b) => format!("{}: {} → {}", p.name, value(a), value(b)),
        };
        let _ = writeln!(out, "{indent}{line}");
    }
    out
}
