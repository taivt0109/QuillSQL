//! An HTML report of a data comparison, from the Compare window: the summary of every table and
//! the first rows of each kind, with differing values marked. One self-contained page.

use std::fmt::Write;

use super::compare::DataComparison;
use super::store::{self, RowKind};

/// Rows shown per table and kind.
pub const SAMPLE: usize = 200;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `kinds` in `RowKind::ALL` order says which rows are listed; counts are always shown.
pub fn html(result: &DataComparison, source: &str, target: &str, timestamp: &str, kinds: [bool; 4]) -> std::io::Result<String> {
    let mut s = String::new();
    let _ = write!(
        s,
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Data comparison: {src} and {tgt}</title>\n<style>\n\
body{{font:14px 'Segoe UI',sans-serif;margin:24px 32px;color:#1b1b1b}}h1{{font-size:22px;margin:0 0 4px}}h2{{font-size:17px;margin:28px 0 8px}}\n\
.sub{{color:#616161;margin-bottom:18px}}table{{border-collapse:collapse;margin:6px 0 4px}}th,td{{border:1px solid #e0e0e0;padding:3px 8px;text-align:left;vertical-align:top}}\n\
th{{background:#f5f5f5;font-weight:600}}td.n{{text-align:right}}.d{{background:#fde7c2}}.k{{font-weight:600}}.x{{color:#a4262c}}.muted{{color:#757575}}\n\
th.d{{background:#f9d48f}}td.code{{font-family:Consolas,monospace;white-space:pre-wrap;max-width:420px}}\n\
</style></head><body>\n<h1>Data comparison</h1>\n<div class=\"sub\">{src} &rarr; {tgt} &middot; {ts}</div>\n",
        src = esc(source),
        tgt = esc(target),
        ts = esc(timestamp)
    );
    let _ = writeln!(
        s,
        "<table><tr><th>Object</th><th>Different</th><th>Only in {}</th><th>Only in {}</th><th>Identical</th><th>Status</th></tr>",
        esc(source),
        esc(target)
    );
    for t in &result.tables {
        let status = if let Some(e) = &t.error {
            format!("<span class=\"x\">{}</span>", esc(e))
        } else if let Some(p) = &t.pair.problem {
            format!("<span class=\"x\">{}</span>", esc(p))
        } else if t.identical() {
            "identical".into()
        } else {
            "different".into()
        };
        let name = if t.pair.source == t.pair.target { esc(&t.pair.source.to_string()) } else { format!("{} &rarr; {}", esc(&t.pair.source.to_string()), esc(&t.pair.target.to_string())) };
        let _ = writeln!(
            s,
            "<tr><td>{name}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td>{status}</td></tr>",
            t.count(RowKind::Different),
            t.count(RowKind::OnlyIn1),
            t.count(RowKind::OnlyIn2),
            t.count(RowKind::Identical)
        );
    }
    s.push_str("</table>\n");
    for name in &result.only_in_1 {
        let _ = writeln!(s, "<div class=\"muted\">{} exists only in {}.</div>", esc(&name.to_string()), esc(source));
    }
    for name in &result.only_in_2 {
        let _ = writeln!(s, "<div class=\"muted\">{} exists only in {}.</div>", esc(&name.to_string()), esc(target));
    }
    for t in result.tables.iter().filter(|t| t.error.is_none() && !t.offsets.iter().all(Vec::is_empty)) {
        let p = &t.pair;
        let mut section = String::new();
        for kind in [RowKind::Different, RowKind::OnlyIn1, RowKind::OnlyIn2, RowKind::Identical] {
            let offsets = &t.offsets[kind as usize];
            if !kinds[kind as usize] || offsets.is_empty() {
                continue;
            }
            let label = match kind {
                RowKind::Different => "Different".to_string(),
                RowKind::OnlyIn1 => format!("Only in {source}"),
                RowKind::OnlyIn2 => format!("Only in {target}"),
                RowKind::Identical => "Identical".to_string(),
            };
            let shown = offsets.len().min(SAMPLE);
            let _ = write!(section, "<h3>{} ({}{})</h3>\n<table><tr>", esc(&label), offsets.len(), if shown < offsets.len() { format!(", first {shown} shown") } else { String::new() });
            let paired = kind == RowKind::Different;
            for (i, c) in p.columns.iter().enumerate() {
                let differs = t.column_differences.get(i).copied().unwrap_or(0) > 0;
                let cls = if differs && paired { " class=\"d\"" } else { "" };
                if paired && !p.key.contains(&i) {
                    let _ = write!(section, "<th{cls}>{} ({})</th><th{cls}>{} ({})</th>", esc(&c.source), esc(source), esc(&c.target), esc(target));
                } else {
                    let _ = write!(section, "<th{cls}>{}</th>", esc(&c.source));
                }
            }
            section.push_str("</tr>\n");
            for row in store::read_page(&t.path, offsets, 0, shown)? {
                section.push_str("<tr>");
                for (i, _) in p.columns.iter().enumerate() {
                    let cell = |v: Option<&super::value::Value>| v.map(|v| esc(&v.display())).unwrap_or_default();
                    let (a, b) = (cell(row.source.get(i)), cell(row.target.get(i)));
                    if p.key.contains(&i) {
                        let _ = write!(section, "<td class=\"code k\">{}</td>", if row.source.is_empty() { b } else { a });
                    } else if paired {
                        let d = if row.differs.get(i).copied().unwrap_or(false) { " d" } else { "" };
                        let _ = write!(section, "<td class=\"code{d}\">{a}</td><td class=\"code{d}\">{b}</td>");
                    } else {
                        let _ = write!(section, "<td class=\"code\">{}</td>", if kind == RowKind::OnlyIn2 { b } else { a });
                    }
                }
                section.push_str("</tr>\n");
            }
            section.push_str("</table>\n");
        }
        if !section.is_empty() {
            let _ = write!(s, "<h2>{}</h2>\n{section}", esc(&p.source.to_string()));
        }
    }
    s.push_str("</body></html>\n");
    Ok(s)
}
