//! Comparison reports for `/report`: a simple XML document, or a self-contained HTML page
//! that lists every difference with its property changes.

use std::fmt::Write;

use super::compare::{Comparison, DiffKind, Difference};

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn kind_text(k: DiffKind) -> &'static str {
    match k {
        DiffKind::Equal => "Equal",
        DiffKind::Different => "Different",
        DiffKind::OnlyIn1 => "OnlyIn1",
        DiffKind::OnlyIn2 => "OnlyIn2",
    }
}

fn name_of(d: &Difference) -> String {
    if d.name.schema.is_empty() { format!("[{}]", d.name.name) } else { d.name.to_string() }
}

/// `<comparison>` with one `<difference>` per object and its `<property>` changes.
pub fn xml(c: &Comparison, db1: &str, db2: &str, all: bool) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    let _ = writeln!(out, "<comparison source=\"{}\" target=\"{}\" tool=\"QuillSQL {}\">", xml_escape(db1), xml_escape(db2), env!("CARGO_PKG_VERSION"));
    for d in c.differences.iter().filter(|d| all || d.kind != DiffKind::Equal) {
        let _ = writeln!(
            out,
            "  <difference type=\"{}\" name=\"{}\" status=\"{}\" selected=\"{}\">",
            xml_escape(d.object_type.label()),
            xml_escape(&name_of(d)),
            kind_text(d.kind),
            d.selected
        );
        for p in &d.properties {
            let location: Vec<String> = p.location.iter().map(|f| format!("{} {}", f.name, f.source.as_deref().or(f.target.as_deref()).unwrap_or_default())).collect();
            let _ = writeln!(
                out,
                "    <property name=\"{}\" location=\"{}\" source=\"{}\" target=\"{}\" />",
                xml_escape(&p.name),
                xml_escape(&location.join(" / ")),
                xml_escape(p.source.as_deref().unwrap_or("")),
                xml_escape(p.target.as_deref().unwrap_or(""))
            );
        }
        out.push_str("  </difference>\n");
    }
    out.push_str("</comparison>\n");
    out
}

/// A page with a summary and a filterable table of differences.
pub fn html(c: &Comparison, db1: &str, db2: &str, all: bool) -> String {
    let count = |k: DiffKind| c.differences.iter().filter(|d| d.kind == k).count();
    let mut rows = String::new();
    for d in c.differences.iter().filter(|d| all || d.kind != DiffKind::Equal) {
        let status = match d.kind {
            DiffKind::Equal => "identical".to_string(),
            DiffKind::Different => "different".to_string(),
            DiffKind::OnlyIn1 => format!("only in {db1}"),
            DiffKind::OnlyIn2 => format!("only in {db2}"),
        };
        let mut details = String::new();
        for p in &d.properties {
            let location: Vec<String> = p.location.iter().map(|f| format!("{} {}", f.name, f.source.as_deref().or(f.target.as_deref()).unwrap_or_default())).collect();
            let place = if location.is_empty() { String::new() } else { format!("<span class=loc>{}</span> ", xml_escape(&location.join(" › "))) };
            let change = match (&p.source, &p.target) {
                (Some(s), None) => format!("{} <b>{}</b> only in {}", xml_escape(&p.name), xml_escape(s), xml_escape(db1)),
                (None, Some(t)) => format!("{} <b>{}</b> only in {}", xml_escape(&p.name), xml_escape(t), xml_escape(db2)),
                (s, t) => format!(
                    "{}: <del>{}</del> → <ins>{}</ins>",
                    xml_escape(&p.name),
                    xml_escape(s.as_deref().unwrap_or("")),
                    xml_escape(t.as_deref().unwrap_or(""))
                ),
            };
            let _ = write!(details, "<li>{place}{change}</li>");
        }
        let _ = writeln!(
            rows,
            "<tr class=\"{}\"><td>{}</td><td>{}</td><td>{}</td><td><ul>{}</ul></td></tr>",
            kind_text(d.kind),
            xml_escape(d.object_type.label()),
            xml_escape(&name_of(d)),
            xml_escape(&status),
            details
        );
    }
    format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Schema comparison: {src} → {tgt}</title>
<style>
body {{ font: 14px/1.45 "Segoe UI", system-ui, sans-serif; margin: 24px; color: #1f2328; }}
h1 {{ font-size: 20px; margin: 0 0 4px; }} .muted {{ color: #656d76; }}
table {{ border-collapse: collapse; width: 100%; margin-top: 16px; }}
th, td {{ text-align: left; vertical-align: top; padding: 6px 10px; border-bottom: 1px solid #d0d7de; }}
th {{ background: #f6f8fa; position: sticky; top: 0; }}
tr.OnlyIn1 td:nth-child(3) {{ color: #1a7f37; }} tr.OnlyIn2 td:nth-child(3) {{ color: #cf222e; }} tr.Different td:nth-child(3) {{ color: #9a6700; }}
ul {{ margin: 0; padding-left: 18px; }} del {{ background: #ffebe9; text-decoration: none; }} ins {{ background: #dafbe1; text-decoration: none; }}
.loc {{ color: #656d76; }} input {{ padding: 4px 8px; width: 280px; }}
</style></head><body>
<h1>Schema comparison</h1>
<div class=muted>{src} → {tgt} · created by QuillSQL {ver}</div>
<p>{d} different · {o1} only in {src} · {o2} only in {tgt} · {eq} identical</p>
<input id=f placeholder="Filter objects" oninput="for (const r of document.querySelectorAll('tbody tr')) r.hidden = !r.textContent.toLowerCase().includes(this.value.toLowerCase())">
<table><thead><tr><th>Type</th><th>Object</th><th>Status</th><th>Differences</th></tr></thead><tbody>
{rows}</tbody></table></body></html>
"#,
        src = xml_escape(db1),
        tgt = xml_escape(db2),
        ver = env!("CARGO_PKG_VERSION"),
        d = count(DiffKind::Different),
        o1 = count(DiffKind::OnlyIn1),
        o2 = count(DiffKind::OnlyIn2),
        eq = count(DiffKind::Equal),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ObjectName;
    use crate::schema::compare::{ObjectType, PropDiff};

    #[test]
    fn reports_escape_and_list() {
        let c = Comparison {
            differences: vec![Difference {
                object_type: ObjectType::Table,
                name: ObjectName::new("dbo", "A<B"),
                target_name: None,
                kind: DiffKind::Different,
                properties: vec![PropDiff { location: vec![], name: "Data type".into(), source: Some("int".into()), target: Some("bigint".into()) }],
                selected: true,
                trivial: false,
            }],
            auto_mapped_columns: vec![],
        };
        let x = xml(&c, "S", "T", false);
        assert!(x.contains("name=\"[dbo].[A&lt;B]\"") && x.contains("source=\"int\""));
        let h = html(&c, "S", "T", false);
        assert!(h.contains("<del>int</del> → <ins>bigint</ins>") && h.contains("1 different"));
    }
}
