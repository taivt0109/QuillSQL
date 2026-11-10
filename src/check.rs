//! What SQL written by the AI names that the database does not have, found before it reaches the
//! editor: tables and views, columns of a known table, and strings or comments left open.

use crate::complete::{bare, eq, is_name, resolve, source_columns, Analysis, Schema};
use crate::keywords::{is_reserved, DATA_TYPES};
use crate::lexer::{lex_partial, Kind};

/// Words that are not reserved but are no column either: date parts, window frames, MERGE.
const NOT_COLUMNS: &[&str] = &[
    "YEAR", "YY", "YYYY", "QUARTER", "QQ", "Q", "MONTH", "MM", "M", "DAYOFYEAR", "DY", "Y", "DAY", "DD", "D", "WEEK", "WK", "WW",
    "WEEKDAY", "DW", "HOUR", "HH", "MINUTE", "MI", "N", "SECOND", "SS", "S", "MILLISECOND", "MS", "MICROSECOND", "MCS",
    "NANOSECOND", "NS", "ISO_WEEK", "ISOWK", "ISOWW", "TZOFFSET", "TZ", "ROWS", "RANGE", "ONLY", "NEXT", "FIRST", "LAST",
    "UNBOUNDED", "PRECEDING", "FOLLOWING", "CURRENT", "ROW", "TIES", "MATCHED", "TARGET", "SOURCE", "NOLOCK", "INSERTED",
    "DELETED", "ZONE", "ABSENT", "PATH", "AUTO", "RAW", "ELEMENTS", "ROOT", "INCLUDE_NULL_VALUES", "WITHOUT_ARRAY_WRAPPER",
];

/// One line per problem, for the AI to correct: `Customers.Emial: no such column (columns: Id, Name)`.
pub fn problems(text: &str, schema: &Schema) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let (_, error) = lex_partial(text);
    if let Some((e, _)) = error {
        out.push(format!("{} left open (line {})", e.what, e.line));
        return out;
    }
    let a = Analysis::new(text);
    let mut meta = schema.clone();
    meta.objects.extend(a.local_tables(Some(schema)));
    let t = &a.tokens;
    let mut push = |p: String| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    for (start, end) in a.statements() {
        let sources = a.sources(start, end);
        let ctes = a.ctes(start, end);
        // Each source's columns, or None when they cannot be known.
        let mut known: Vec<Option<Vec<String>>> = Vec::new();
        for s in &sources {
            let named = !s.parts.is_empty() && s.columns.is_empty();
            let name = s.parts.last().cloned().unwrap_or_default();
            let elsewhere = s.parts.len() >= 3 && !eq(&s.parts[s.parts.len() - 3], &schema.database);
            let system = s.parts.len() >= 2 && matches!(s.parts[s.parts.len() - 2].to_ascii_uppercase().as_str(), "SYS" | "INFORMATION_SCHEMA");
            let call = t.get(s.at + s.parts.len() * 2 - 1).is_some_and(|x| x.kind == Kind::LParen);
            let builtin = call && crate::functions::all().iter().any(|f| eq(&f.name, &name));
            let local = name.starts_with('#') || name.starts_with('@') || ctes.iter().any(|c| eq(&c.reference, &name));
            // `UPDATE c ... FROM dbo.Customers c`: the alias of another source.
            if s.parts.len() == 1 && sources.iter().any(|o| o.alias_at.is_some() && eq(&o.reference, &name)) {
                known.push(Some(Vec::new()));
                continue;
            }
            if named && !elsewhere && !system && !builtin && !local && resolve(s, &meta).is_none() && !a.up.get(s.at).is_some_and(|w| w == "OPENJSON") {
                push(format!("{}: no such table or view", s.parts.join(".")));
                known.push(None);
                continue;
            }
            let columns: Vec<String> = source_columns(s, Some(&meta)).into_iter().map(|(c, _)| c).collect();
            known.push((!columns.is_empty()).then_some(columns));
        }
        let listed = |columns: &[String]| {
            let shown: Vec<&str> = columns.iter().take(40).map(String::as_str).collect();
            format!("{}{}", shown.join(", "), if columns.len() > shown.len() { ", ..." } else { "" })
        };
        // `alias.column`.
        for j in start..end.saturating_sub(2) {
            let qualified = is_name(&t[j]) && t[j + 1].kind == Kind::Dot && is_name(&t[j + 2]);
            let more = t.get(j + 3).is_some_and(|x| matches!(x.kind, Kind::Dot | Kind::LParen)) || (j > 0 && t[j - 1].kind == Kind::Dot);
            if !qualified || more {
                continue;
            }
            let (q, name) = (bare(&t[j]), bare(&t[j + 2]));
            if let Some(Some(columns)) = (0..sources.len()).filter(|&i| eq(&sources[i].reference, &q)).map(|i| &known[i]).find(|k| k.as_ref().is_none_or(|c| !c.is_empty()))
                && !columns.iter().any(|c| eq(c, &name))
            {
                push(format!("{q}.{name}: no such column (columns of {q}: {})", listed(columns)));
            }
        }
        // A column named alone, where every source's columns are known.
        if sources.is_empty() || known.iter().any(Option::is_none) {
            continue;
        }
        let all: Vec<&String> = known.iter().flatten().flatten().collect();
        let aliases: Vec<String> = (start..end).filter(|&j| j > 0 && a.up[j - 1] == "AS" && is_name(&t[j])).map(|j| bare(&t[j])).collect();
        let prev = |j: usize| (start..j).rev().find(|&i| !t[i].is_comment());
        for j in start..end {
            if !matches!(t[j].kind, Kind::Word | Kind::Bracket) || (t[j].kind == Kind::Word && (is_reserved(&a.up[j]) || NOT_COLUMNS.contains(&a.up[j].as_str()) || DATA_TYPES.contains(&a.up[j].as_str()))) {
                continue;
            }
            let near = |i: Option<usize>| i.map(|i| &t[i]);
            let before = prev(j);
            let after = (j + 1..end).find(|&i| !t[i].is_comment());
            // Part of a name, a call, an alias being given (`AS n`, `c.Name n`) or `n = ...` in a select list.
            if near(before).is_some_and(|b| b.kind == Kind::Dot)
                || near(after).is_some_and(|x| matches!(x.kind, Kind::Dot | Kind::LParen))
                || before.is_some_and(|b| a.up[b] == "AS" || crate::complete::ends_operand(t, b))
                || (after.is_some_and(|x| t[x].text == "=") && a.clause_before(start, j) == "SELECT")
                || !matches!(a.clause_before(start, j).as_str(), "SELECT" | "WHERE" | "ON" | "GROUP" | "ORDER" | "HAVING")
            {
                continue;
            }
            let name = bare(&t[j]);
            let used = all.iter().any(|c| eq(c, &name))
                || sources.iter().any(|s| eq(&s.reference, &name) || s.parts.iter().any(|p| eq(p, &name)))
                || aliases.iter().any(|x| eq(x, &name))
                || ctes.iter().any(|c| eq(&c.reference, &name));
            if !used {
                push(format!("{name}: no such column in {}", sources.iter().map(|s| s.reference.as_str()).collect::<Vec<_>>().join(", ")));
            }
        }
    }
    out
}
