//! Information about the name under the caret: a hover description, and the other
//! places in the script where the same variable, alias or object is used.

use serde::Serialize;

use crate::complete::{Analysis, Column, DbObject, Schema, Source, bare, byte_to_utf16, eq, is_name, resolve, source_columns, utf16_to_byte};
use crate::keywords::is_reserved;
use crate::lexer::Kind;
use crate::refactor::{declared_types, end_of};

fn token_at(a: &Analysis, at: usize) -> Option<usize> {
    (0..a.tokens.len()).find(|&i| {
        let t = &a.tokens[i];
        a.starts[i] <= at && at <= end_of(a, i) && (is_name(t) || t.kind == Kind::Variable)
    })
}

fn statement_of(a: &Analysis, i: usize) -> (usize, usize) {
    a.statements().into_iter().find(|&(s, e)| s <= i && i < e).unwrap_or((0, a.tokens.len()))
}

fn batch_of(a: &Analysis, i: usize) -> (usize, usize) {
    let start = (0..i).rev().find(|&j| a.tokens[j].kind == Kind::Go).map_or(0, |j| j + 1);
    let end = (i..a.tokens.len()).find(|&j| a.tokens[j].kind == Kind::Go).unwrap_or(a.tokens.len());
    (start, end)
}

pub fn describe_object(o: &DbObject) -> String {
    let kind = match o.kind.as_str() {
        "scalar_function" => "function",
        "table_function" => "table-valued function",
        k => k,
    };
    let mut out = format!("{kind} {}.{}", o.schema, o.name);
    if !o.params.is_empty() {
        let params: Vec<String> = o.params.iter().map(|p| format!("{} {}{}", p.name, p.data_type, if p.output { " OUTPUT" } else { "" })).collect();
        out.push_str(&format!(" ({})", params.join(", ")));
    }
    if let Some(d) = o.description.as_deref().filter(|d| !d.trim().is_empty()) {
        out.push_str(&format!("\n{}", d.trim()));
    }
    for p in &o.params {
        if let Some(d) = p.description.as_deref().filter(|d| !d.trim().is_empty()) {
            out.push_str(&format!("\n    {}: {}", p.name, d.trim()));
        }
    }
    const SHOWN: usize = 25;
    for c in o.columns.iter().take(SHOWN) {
        let mut flags = String::new();
        if c.identity {
            flags.push_str(" IDENTITY");
        }
        if c.computed {
            flags.push_str(" computed");
        }
        let note = c.description.as_deref().filter(|d| !d.trim().is_empty()).map_or(String::new(), |d| format!("  -- {}", d.trim()));
        out.push_str(&format!("\n    {} {}{}{}{note}", c.name, c.data_type, if c.nullable { " NULL" } else { " NOT NULL" }, flags));
    }
    if o.columns.len() > SHOWN {
        out.push_str(&format!("\n    ... {} more columns", o.columns.len() - SHOWN));
    }
    out
}

fn describe_column(source: &Source, schema: Option<&Schema>, name: &str) -> Option<String> {
    if let Some(o) = schema.and_then(|s| resolve(source, s)) {
        let c = o.columns.iter().find(|c| eq(&c.name, name))?;
        let description = c.description.as_deref().map(str::trim).filter(|d| !d.is_empty()).map_or(String::new(), |d| format!("
{d}"));
        return Some(format!("column {}.{}.{} {}{}{description}", o.schema, o.name, c.name, c.data_type, if c.nullable { " NULL" } else { " NOT NULL" }));
    }
    source_columns(source, schema).into_iter().find(|(c, _)| eq(c, name)).map(|(c, _)| format!("column {}.{c}", source.reference))
}

/// A description of the variable, alias, column or object at the caret (UTF-16).
pub fn hover(text: &str, caret: usize, schema: Option<&Schema>) -> Option<String> {
    let a = Analysis::new(text);
    let i = token_at(&a, utf16_to_byte(text, caret))?;
    let t = &a.tokens[i];
    if t.kind == Kind::Variable {
        if t.text.starts_with("@@") {
            return Some(format!("{} (global variable)", t.text.to_ascii_uppercase()));
        }
        return declared_types(&a).into_iter().find(|(n, _)| eq(n, &t.text)).map(|(n, ty)| format!("{n} {ty}"));
    }
    if t.kind == Kind::Word && is_reserved(&a.up[i]) {
        return None;
    }
    let name = bare(t);
    let stmt = statement_of(&a, i);
    let sources = a.sources(stmt.0, stmt.1);
    let qualified = i >= 2 && a.tokens[i - 1].kind == Kind::Dot && is_name(&a.tokens[i - 2]);
    let qualifies = a.tokens.get(i + 1).is_some_and(|n| n.kind == Kind::Dot);

    if qualified && !qualifies {
        let q = bare(&a.tokens[i - 2]);
        let alias_of_source = !(i >= 4 && a.tokens[i - 3].kind == Kind::Dot);
        if let Some(src) = sources.iter().find(|s| alias_of_source && eq(&s.reference, &q))
            && let Some(d) = describe_column(src, schema, &name) {
                return Some(d);
            }
    }
    if let Some(src) = sources.iter().find(|s| s.alias_at == Some(i) || (qualifies && !qualified && s.alias_at.is_some() && eq(&s.reference, &name))) {
        let target = match schema.and_then(|s| resolve(src, s)) {
            Some(o) => describe_object(o),
            None if src.parts.is_empty() => "derived table".to_string(),
            None => src.parts.join("."),
        };
        return Some(format!("{} = {target}", src.reference));
    }
    if let Some(cte) = a.ctes(stmt.0, stmt.1).into_iter().find(|c| eq(&c.reference, &name)) {
        return Some(format!("common table expression {} ({})", cte.reference, cte.columns.join(", ")));
    }
    let schema_meta = schema?;
    let mut first = i;
    while first >= 2 && a.tokens[first - 1].kind == Kind::Dot && is_name(&a.tokens[first - 2]) {
        first -= 2;
    }
    let mut parts = Vec::new();
    let mut j = first;
    while j <= i {
        parts.push(bare(&a.tokens[j]));
        j += 2;
    }
    let lookup = Source { reference: String::new(), parts, columns: Vec::new(), at: first, alias_at: None };
    if let Some(o) = resolve(&lookup, schema_meta) {
        return Some(describe_object(o));
    }
    if !qualified {
        let owners: Vec<&Source> = sources.iter().filter(|s| source_columns(s, schema).iter().any(|(c, _)| eq(c, &name))).collect();
        if let [owner] = owners.as_slice() {
            return describe_column(owner, schema, &name);
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Range {
    pub start: usize,
    pub length: usize,
}

/// Every use of the variable, alias or name at the caret, in UTF-16 offsets.
pub fn occurrences(text: &str, caret: usize) -> Vec<Range> {
    let a = Analysis::new(text);
    let Some(i) = token_at(&a, utf16_to_byte(text, caret)) else { return Vec::new() };
    let t = &a.tokens[i];
    if t.kind == Kind::Word && is_reserved(&a.up[i]) {
        return Vec::new();
    }
    let name = bare(t);
    let (s, e) = if t.kind == Kind::Variable { batch_of(&a, i) } else { statement_of(&a, i) };
    let alias = t.kind != Kind::Variable && a.sources(s, e).iter().any(|src| src.alias_at.is_some() && eq(&src.reference, &name));
    let (s, e) = if t.kind == Kind::Variable || alias { (s, e) } else { batch_of(&a, i) };
    (s..e)
        .filter(|&j| {
            let u = &a.tokens[j];
            if t.kind == Kind::Variable {
                return u.kind == Kind::Variable && eq(&u.text, &t.text);
            }
            is_name(u) && !(u.kind == Kind::Word && is_reserved(&a.up[j])) && eq(&bare(u), &name)
        })
        .map(|j| {
            let start = byte_to_utf16(text, a.starts[j]);
            Range { start, length: a.tokens[j].text.encode_utf16().count() }
        })
        .collect()
}

fn column_list(o: &DbObject) -> String {
    o.columns
        .iter()
        .map(|c| format!("{} {}{}{}", c.name, c.data_type, if c.nullable { " NULL" } else { " NOT NULL" }, if c.identity { " IDENTITY" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The tables, views, functions and procedures the script refers to, as `schema.name`.
pub fn referenced_objects(text: &str, schema: &Schema) -> Vec<String> {
    used_objects(text, schema).iter().map(|o| format!("{}.{}", o.schema, o.name)).collect()
}

fn used_objects<'s>(text: &str, schema: &'s Schema) -> Vec<&'s DbObject> {
    let a = Analysis::new(text);
    let mut used: Vec<&DbObject> = Vec::new();
    for (s, e) in a.statements() {
        let mut sources = a.sources(s, e);
        if s < e && matches!(a.up[s].as_str(), "EXEC" | "EXECUTE") {
            let mut j = s + 1;
            let mut parts = Vec::new();
            while j < e && (is_name(&a.tokens[j]) || a.tokens[j].kind == Kind::Dot) {
                if is_name(&a.tokens[j]) {
                    parts.push(bare(&a.tokens[j]));
                }
                j += 1;
            }
            sources.push(Source { reference: String::new(), parts, columns: Vec::new(), at: s, alias_at: None });
        }
        for src in &sources {
            if let Some(o) = resolve(src, schema)
                && !used.iter().any(|u| std::ptr::eq(*u, o)) {
                    used.push(o);
                }
        }
    }
    used
}

/// A compact description of the database for an AI prompt: the objects the script uses with
/// their columns, parameters and foreign keys; or, for a script that uses none, the database's
/// own tables and views. Stops at about `budget` characters.
pub fn schema_context(text: &str, schema: &Schema, budget: usize) -> String {
    let mut used = used_objects(text, schema);
    let system = |o: &DbObject| matches!(o.schema.as_str(), "sys" | "INFORMATION_SCHEMA");
    if used.is_empty() {
        used = schema.objects.iter().filter(|o| !system(o) && matches!(o.kind.as_str(), "table" | "view")).collect();
    }
    let mut out = format!("Database {} (default schema {}).\n", schema.database, schema.default_schema);
    for (i, o) in used.iter().enumerate() {
        let line = if o.params.is_empty() {
            format!("{} {}.{} ({})\n", o.kind.to_uppercase(), o.schema, o.name, column_list(o))
        } else {
            let params: Vec<String> = o.params.iter().map(|p| format!("{} {}{}", p.name, p.data_type, if p.output { " OUTPUT" } else { "" })).collect();
            format!("{} {}.{} ({})\n", o.kind.to_uppercase(), o.schema, o.name, params.join(", "))
        };
        if out.len() + line.len() > budget {
            out.push_str(&format!("... and {} more objects not listed.\n", used.len() - i));
            break;
        }
        out.push_str(&line);
    }
    for fk in &schema.foreign_keys {
        let involved = |s: &str, t: &str| used.iter().any(|o| eq(&o.schema, s) && eq(&o.name, t));
        if involved(&fk.schema, &fk.table) && involved(&fk.ref_schema, &fk.ref_table) {
            let line = format!("FOREIGN KEY {}.{} ({}) REFERENCES {}.{} ({})\n", fk.schema, fk.table, fk.columns.join(", "), fk.ref_schema, fk.ref_table, fk.ref_columns.join(", "));
            if out.len() + line.len() > budget {
                break;
            }
            out.push_str(&line);
        }
    }
    out
}

/// The object the name at the caret (UTF-16) refers to: a table, view, procedure or function
/// by name, the object behind an alias, or the table of an `alias.column`.
/// The table or view column at the caret (UTF-16), for its card: `c.Name`, `dbo.Customers.Name`,
/// or a name only one table of the statement has.
pub fn column_at<'s>(text: &str, caret: usize, schema: &'s Schema) -> Option<(&'s DbObject, &'s Column)> {
    let a = Analysis::new(text);
    let i = token_at(&a, utf16_to_byte(text, caret))?;
    let t = &a.tokens[i];
    if !is_name(t) || (t.kind == Kind::Word && is_reserved(&a.up[i])) || a.tokens.get(i + 1).is_some_and(|n| n.kind == Kind::Dot) {
        return None;
    }
    let name = bare(t);
    let stmt = statement_of(&a, i);
    let sources = a.sources(stmt.0, stmt.1);
    let column = |o: &'s DbObject| o.columns.iter().find(|c| eq(&c.name, &name)).map(|c| (o, c));
    if i >= 2 && a.tokens[i - 1].kind == Kind::Dot && is_name(&a.tokens[i - 2]) {
        let q = bare(&a.tokens[i - 2]);
        let alias_of_source = !(i >= 4 && a.tokens[i - 3].kind == Kind::Dot);
        if let Some(found) = sources.iter().filter(|s| alias_of_source && eq(&s.reference, &q)).find_map(|s| resolve(s, schema).and_then(column)) {
            return Some(found);
        }
        // `schema.table.column`: the object named before the column.
        let mut first = i - 2;
        while first >= 2 && a.tokens[first - 1].kind == Kind::Dot && is_name(&a.tokens[first - 2]) {
            first -= 2;
        }
        let parts: Vec<String> = (first..i - 1).step_by(2).map(|j| bare(&a.tokens[j])).collect();
        let lookup = Source { reference: String::new(), parts, columns: Vec::new(), at: first, alias_at: None };
        return resolve(&lookup, schema).and_then(column);
    }
    let owners: Vec<(&DbObject, &Column)> = sources.iter().filter_map(|s| resolve(s, schema).and_then(column)).collect();
    match owners.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

pub fn object_at<'s>(text: &str, caret: usize, schema: &'s Schema) -> Option<&'s DbObject> {
    let a = Analysis::new(text);
    let i = token_at(&a, utf16_to_byte(text, caret))?;
    let t = &a.tokens[i];
    if !is_name(t) || (t.kind == Kind::Word && is_reserved(&a.up[i])) {
        return None;
    }
    let mut first = i;
    while first >= 2 && a.tokens[first - 1].kind == Kind::Dot && is_name(&a.tokens[first - 2]) {
        first -= 2;
    }
    let mut last = i;
    while last + 2 < a.tokens.len() && a.tokens[last + 1].kind == Kind::Dot && is_name(&a.tokens[last + 2]) {
        last += 2;
    }
    let stmt = statement_of(&a, i);
    let sources = a.sources(stmt.0, stmt.1);
    let head = bare(&a.tokens[first]);
    let alias = sources.iter().find(|s| s.alias_at.is_some() && eq(&s.reference, &head) && (first < last || s.alias_at == Some(i)));
    if let Some(o) = alias.and_then(|s| resolve(s, schema)) {
        return Some(o);
    }
    for end in [i, last] {
        let parts: Vec<String> = (first..=end).step_by(2).map(|j| bare(&a.tokens[j])).collect();
        let lookup = Source { reference: String::new(), parts, columns: Vec::new(), at: first, alias_at: None };
        if let Some(o) = resolve(&lookup, schema) {
            return Some(o);
        }
    }
    None
}
