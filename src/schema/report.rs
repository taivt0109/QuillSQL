//! Comparison reports for `/report`, in SQL Compare's formats: its XML layout (so existing
//! parsers keep working), an HTML page and a classic HTML page with the same information, and
//! Excel (SpreadsheetML as SQL Compare writes it, or an .xlsx workbook).

use std::fmt::Write;

use super::compare::{DiffKind, Difference, ObjectType};
use super::linediff::Line;

/// One side of the comparison as the report shows it.
#[derive(Clone, Debug, Default)]
pub struct DataSource {
    /// `live`, `snapshot`, `folder`, `backup`, or empty for an empty database.
    pub kind: &'static str,
    pub server: String,
    pub database: String,
    /// `creationdate` of a snapshot, already formatted.
    pub created: Option<String>,
}

/// One object in the report, with its scripts aligned line by line.
#[derive(Clone, Debug)]
pub struct Item {
    pub object_type: ObjectType,
    pub kind: DiffKind,
    /// `[schema].[name]` (or `[name]`), the name in database 1 or, when only in 2, database 2.
    pub full_name: String,
    pub owner1: String,
    pub name1: String,
    pub owner2: String,
    pub name2: String,
    pub lines: Vec<Line>,
}

pub struct Report {
    pub timestamp: String,
    pub source: DataSource,
    pub target: DataSource,
    /// Already chosen and ordered (see [`order`]).
    pub items: Vec<Item>,
    pub include_identical: bool,
}

/// SQL Compare's name of the object type (`StoredProcedure`).
pub fn type_name(t: ObjectType) -> &'static str {
    match t {
        ObjectType::Schema => "Schema",
        ObjectType::Role => "Role",
        ObjectType::User => "User",
        ObjectType::UserDefinedType => "UserDefinedType",
        ObjectType::Table => "Table",
        ObjectType::View => "View",
        ObjectType::Function => "Function",
        ObjectType::StoredProcedure => "StoredProcedure",
        ObjectType::Sequence => "Sequence",
        ObjectType::Synonym => "Synonym",
        ObjectType::ExtendedProperty => "ExtendedProperty",
    }
}

/// The type as people read it, as in SQL Compare's Excel report.
fn readable_type(t: ObjectType) -> &'static str {
    match t {
        ObjectType::StoredProcedure => "Stored Procedure",
        ObjectType::UserDefinedType => "User Defined Type",
        ObjectType::Function => "User Defined Function",
        ObjectType::ExtendedProperty => "Extended Property",
        other => type_name(other),
    }
}

/// `status` attribute values.
fn status(k: DiffKind) -> &'static str {
    match k {
        DiffKind::OnlyIn1 => "onlyin1",
        DiffKind::OnlyIn2 => "onlyin2",
        DiffKind::Different => "different",
        DiffKind::Equal => "equal",
    }
}

fn status_rank(k: DiffKind) -> u8 {
    match k {
        DiffKind::OnlyIn1 => 0,
        DiffKind::OnlyIn2 => 1,
        DiffKind::Different => 2,
        DiffKind::Equal => 3,
    }
}

/// SQL Compare's report order of object types; types without a rank come first.
fn type_rank(t: ObjectType) -> u8 {
    match t {
        ObjectType::Table => 1,
        ObjectType::StoredProcedure => 2,
        ObjectType::View => 3,
        ObjectType::Function => 6,
        ObjectType::Role => 7,
        ObjectType::User => 9,
        ObjectType::UserDefinedType => 10,
        ObjectType::Synonym => 14,
        ObjectType::Schema => 26,
        ObjectType::Sequence | ObjectType::ExtendedProperty => 0,
    }
}

/// Sorts items by status, type and name.
pub fn order(items: &mut [Item]) {
    items.sort_by(|a, b| {
        (status_rank(a.kind), type_rank(a.object_type), a.full_name.as_str()).cmp(&(status_rank(b.kind), type_rank(b.object_type), b.full_name.as_str()))
    });
}

/// Whether a difference belongs in the report: selected differences (all differences with
/// `/reportAllObjectsWithDifferences`), and identical objects with `/include:Identical`.
pub fn wanted(d: &Difference, all_differences: bool, include_identical: bool) -> bool {
    if d.kind == DiffKind::Equal { include_identical } else { all_differences || d.selected }
}

/// Text that XML 1.0 can hold.
fn clean(s: &str) -> String {
    s.chars().map(|c| if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') { '?' } else { c }).collect()
}

fn text(s: &str) -> String {
    clean(s).replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn attr(s: &str) -> String {
    text(s).replace('"', "&quot;")
}

fn element(out: &mut String, name: &str, attrs: &[(&str, &str)], body: &str) {
    out.push('<');
    out.push_str(name);
    for (k, v) in attrs {
        let _ = write!(out, " {k}=\"{}\"", attr(v));
    }
    if body.is_empty() {
        out.push_str(" />");
    } else {
        let _ = write!(out, ">{}</{name}>", text(body));
    }
}

fn datasource(out: &mut String, s: &DataSource, id: &str) {
    out.push_str("<datasource");
    if let Some(c) = &s.created {
        let _ = write!(out, " creationdate=\"{}\"", attr(c));
    }
    let _ = write!(out, " type=\"{}\" id=\"{id}\">", attr(s.kind));
    element(out, "server", &[], &s.server);
    element(out, "database", &[], &s.database);
    out.push_str("</datasource>");
}

/// The report XML, in SQL Compare's layout (not indented).
pub fn xml_text(r: &Report) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-16\"?>");
    let _ = write!(out, "<comparison direction=\"1to2\" timestamp=\"{}\"><datasources>", attr(&r.timestamp));
    datasource(&mut out, &r.source, "1");
    datasource(&mut out, &r.target, "2");
    out.push_str("</datasources><differences>");
    for i in &r.items {
        let ty = type_name(i.object_type).to_ascii_lowercase();
        let _ = write!(out, "<difference objecttype=\"{ty}\" status=\"{}\" fqn=\"{}\">", status(i.kind), attr(&format!("{ty}-{}", i.full_name)));
        element(&mut out, "object", &[("llastmodified", ""), ("owner", &i.owner1), ("id", "1")], &i.name1);
        element(&mut out, "object", &[("owner", &i.owner2), ("rlastmodified", ""), ("id", "2")], &i.name2);
        if i.lines.is_empty() {
            out.push_str("<comparisonstrings />");
        } else {
            out.push_str("<comparisonstrings>");
            for l in &i.lines {
                let side = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(" ").to_string();
                let _ = write!(
                    out,
                    "<line type=\"{}\"><left>{}</left><right>{}</right></line>",
                    if l.different { "different" } else { "same" },
                    text(&side(&l.left)),
                    text(&side(&l.right))
                );
            }
            out.push_str("</comparisonstrings>");
        }
        out.push_str("</difference>");
    }
    out.push_str("</differences></comparison>");
    out
}

/// The XML report file: UTF-16 little endian with a byte order mark.
pub fn xml(r: &Report) -> Vec<u8> {
    [0xFF, 0xFE].into_iter().chain(xml_text(r).encode_utf16().flat_map(u16::to_le_bytes)).collect()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn describe(s: &DataSource) -> String {
    match s.kind {
        "" => "Empty database".to_string(),
        "live" => format!("{} on {}", s.database, s.server),
        kind => format!("{} ({kind}{})", s.database, if s.server.is_empty() { String::new() } else { format!(", {}", s.server) }),
    }
}

const CSS: &str = r#"
body { font: 14px/1.45 "Segoe UI", system-ui, sans-serif; margin: 24px; color: #1f2328; }
h1 { font-size: 20px; margin: 0 0 4px; } h2 { font-size: 16px; margin: 24px 0 8px; } .muted { color: #656d76; }
.counts span { display: inline-block; margin-right: 18px; } .counts b { font-size: 18px; }
details { border: 1px solid #d0d7de; border-radius: 6px; margin: 6px 0; } summary { padding: 6px 10px; cursor: pointer; }
summary .type { color: #656d76; display: inline-block; min-width: 150px; }
table.sql { border-collapse: collapse; width: 100%; font: 12px/1.4 Consolas, monospace; table-layout: fixed; }
table.sql td { padding: 0 8px; white-space: pre-wrap; vertical-align: top; border-top: 1px solid #f0f2f4; }
table.sql th { text-align: left; padding: 4px 8px; background: #f6f8fa; font: 600 12px "Segoe UI", sans-serif; }
td.del { background: #ffebe9; } td.ins { background: #dafbe1; } td.chg { background: #fff8c5; }
table.list { border-collapse: collapse; width: 100%; } table.list td, table.list th { text-align: left; padding: 4px 10px; border-bottom: 1px solid #d0d7de; }
input { padding: 4px 8px; width: 280px; margin-top: 12px; }
"#;

/// Side-by-side rows of a script comparison: left = database 2 (before), right = database 1 (after).
fn sql_table(i: &Item, left_title: &str, right_title: &str, source_first: bool) -> String {
    let mut out = format!("<table class=sql><tr><th>{}</th><th>{}</th></tr>", html_escape(left_title), html_escape(right_title));
    for l in &i.lines {
        let (a, b) = if source_first { (&l.left, &l.right) } else { (&l.right, &l.left) };
        let class = |me: &Option<String>, other: &Option<String>, removed: &'static str| match (l.different, me, other) {
            (false, _, _) => "",
            (true, Some(_), Some(_)) => " class=chg",
            (true, Some(_), None) => removed,
            _ => "",
        };
        let _ = write!(
            out,
            "<tr><td{}>{}</td><td{}>{}</td></tr>",
            class(a, b, " class=del"),
            html_escape(a.as_deref().unwrap_or("")),
            class(b, a, " class=ins"),
            html_escape(b.as_deref().unwrap_or(""))
        );
    }
    out.push_str("</table>");
    out
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title><style>{CSS}</style></head><body>\n{body}\n\
         <script>function filter(v) {{ v = v.toLowerCase(); for (const e of document.querySelectorAll('[data-name]')) e.hidden = !e.dataset.name.includes(v); }}</script>\n</body></html>\n",
        html_escape(title)
    )
}

fn count(r: &Report, k: DiffKind) -> usize {
    r.items.iter().filter(|i| i.kind == k).count()
}

fn display_name(i: &Item) -> String {
    let (owner, name) = if i.kind == DiffKind::OnlyIn2 { (&i.owner2, &i.name2) } else { (&i.owner1, &i.name1) };
    if owner.is_empty() { format!("[{name}]") } else { format!("[{owner}].[{name}]") }
}

/// The HTML report: counts, then modified, created, dropped (and identical) objects with their
/// scripts; for modified objects, the script before (database 2) and after (database 1).
pub fn html(r: &Report) -> String {
    let mut body = format!(
        "<h1>Schema comparison report</h1>\n<div class=muted>Created {} by QuillSQL {}</div>\n\
         <p>Source: <b>{}</b><br>Target: <b>{}</b></p>\n<div class=counts><span><b>{}</b> modified</span><span><b>{}</b> created</span><span><b>{}</b> dropped</span>{}</div>\n\
         <input placeholder=\"Filter objects\" oninput=\"filter(this.value)\">\n",
        html_escape(&r.timestamp),
        env!("CARGO_PKG_VERSION"),
        html_escape(&describe(&r.source)),
        html_escape(&describe(&r.target)),
        count(r, DiffKind::Different),
        count(r, DiffKind::OnlyIn1),
        count(r, DiffKind::OnlyIn2),
        if r.include_identical { format!("<span><b>{}</b> identical</span>", count(r, DiffKind::Equal)) } else { String::new() },
    );
    let sections = [
        (DiffKind::Different, "Modified objects"),
        (DiffKind::OnlyIn1, "Created objects"),
        (DiffKind::OnlyIn2, "Dropped objects"),
        (DiffKind::Equal, "Identical objects"),
    ];
    for (kind, heading) in sections {
        let items: Vec<&Item> = r.items.iter().filter(|i| i.kind == kind).collect();
        if items.is_empty() || (kind == DiffKind::Equal && !r.include_identical) {
            continue;
        }
        let _ = writeln!(body, "<h2>{heading}</h2>");
        for i in items {
            let name = display_name(i);
            let table = match kind {
                DiffKind::Different => sql_table(i, "Before (target)", "After (source)", false),
                DiffKind::OnlyIn2 => sql_table(i, "Target", "", false),
                _ => sql_table(i, "Source", "", true),
            };
            let _ = writeln!(
                body,
                "<details data-name=\"{}\"><summary><span class=type>{}</span>{}</summary>{table}</details>",
                html_escape(&name.to_lowercase()),
                html_escape(readable_type(i.object_type)),
                html_escape(&name)
            );
        }
    }
    page("Schema comparison report", &body)
}

/// The classic report: a summary per status, a list per status, and each object's SQL side by side.
pub fn classic(r: &Report) -> String {
    let (one, two) = (format!("{}.{}", r.source.server, r.source.database), format!("{}.{}", r.target.server, r.target.database));
    let mut body = format!(
        "<h1>QuillSQL Compare report</h1>\n<div class=muted>{} · {} vs {}</div>\n<table class=list><tr><th>Status</th><th>Objects</th></tr>\
         <tr><td>Different</td><td>{}</td></tr><tr><td>In {one} only</td><td>{}</td></tr><tr><td>In {two} only</td><td>{}</td></tr>{}</table>\n\
         <input placeholder=\"Filter objects\" oninput=\"filter(this.value)\">\n",
        html_escape(&r.timestamp),
        html_escape(&one),
        html_escape(&two),
        count(r, DiffKind::Different),
        count(r, DiffKind::OnlyIn1),
        count(r, DiffKind::OnlyIn2),
        if r.include_identical { format!("<tr><td>Identical</td><td>{}</td></tr>", count(r, DiffKind::Equal)) } else { String::new() },
    );
    let groups = [
        (DiffKind::Different, "Different".to_string()),
        (DiffKind::OnlyIn1, format!("In {one} only")),
        (DiffKind::OnlyIn2, format!("In {two} only")),
        (DiffKind::Equal, "Identical".to_string()),
    ];
    for (kind, heading) in groups {
        let items: Vec<&Item> = r.items.iter().filter(|i| i.kind == kind).collect();
        if items.is_empty() {
            continue;
        }
        let _ = writeln!(body, "<h2>{}</h2>", html_escape(&heading));
        for i in items {
            let label = |o: &str, n: &str| if n.is_empty() { String::new() } else if o.is_empty() { format!("[{n}]") } else { format!("[{o}].[{n}]") };
            let _ = writeln!(
                body,
                "<details data-name=\"{}\"><summary><span class=type>{}</span>{} {} {}</summary>{}</details>",
                html_escape(&i.full_name.to_lowercase()),
                html_escape(readable_type(i.object_type)),
                html_escape(&label(&i.owner1, &i.name1)),
                html_escape(kind_symbol(kind)),
                html_escape(&label(&i.owner2, &i.name2)),
                sql_table(i, &one, &two, true)
            );
        }
    }
    page("QuillSQL Compare report", &body)
}

fn kind_symbol(k: DiffKind) -> &'static str {
    match k {
        DiffKind::Different => "<>",
        DiffKind::OnlyIn1 => "->",
        DiffKind::OnlyIn2 => "<-",
        DiffKind::Equal => "=",
    }
}

/// The rows of the Excel report, in its order: different, only in 1, only in 2, identical.
fn excel_rows(r: &Report) -> Vec<[String; 6]> {
    let mut rows = Vec::new();
    for kind in [DiffKind::Different, DiffKind::OnlyIn1, DiffKind::OnlyIn2, DiffKind::Equal] {
        if kind == DiffKind::Equal && !r.include_identical {
            continue;
        }
        for i in r.items.iter().filter(|i| i.kind == kind) {
            rows.push([
                readable_type(i.object_type).to_string(),
                i.owner1.clone(),
                i.name1.clone(),
                kind_symbol(kind).to_string(),
                i.name2.clone(),
                i.owner2.clone(),
            ]);
        }
    }
    rows
}

fn source_cell(s: &DataSource) -> String {
    format!("{}.{} ( {} ) ", s.server, s.database, s.kind)
}

const TITLE: &str = "QuillSQL Compare Report:";
const HEADINGS: [&str; 6] = ["Type", "Owner", "Object", "Status", "Object", "Owner"];

/// The Excel report as SQL Compare writes it: an XML Spreadsheet 2003 workbook.
pub fn excel(r: &Report) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><?mso-application progid=\"Excel.Sheet\"?>\
         <Workbook xmlns=\"urn:schemas-microsoft-com:office:spreadsheet\" xmlns:o=\"urn:schemas-microsoft-com:office:office\" \
         xmlns:x=\"urn:schemas-microsoft-com:office:excel\" xmlns:ss=\"urn:schemas-microsoft-com:office:spreadsheet\" \
         xmlns:html=\"http://www.w3.org/TR/REC-html40\"><Styles><Style ss:ID=\"Default\" ss:Name=\"Normal\"><Alignment ss:Vertical=\"Bottom\" />\
         <Borders /><Font /><Interior /><NumberFormat /><Protection /></Style><Style ss:ID=\"s21\"><Alignment ss:Horizontal=\"Center\" ss:Vertical=\"Bottom\" /></Style>\
         <Style ss:ID=\"s23\"><Alignment ss:Horizontal=\"Center\" ss:Vertical=\"Bottom\" /><Font x:Family=\"Swiss\" ss:Bold=\"1\" /></Style>\
         <Style ss:ID=\"s24\"><Interior ss:Color=\"#C0C0C0\" ss:Pattern=\"Solid\" /></Style></Styles>\
         <Worksheet ss:Name=\"QuillSQL Compare Report\"><Table><Column ss:AutoFitWidth=\"0\" ss:Width=\"66.75\" /><Column ss:AutoFitWidth=\"0\" ss:Width=\"60\" />\
         <Column ss:AutoFitWidth=\"0\" ss:Width=\"112.5\" /><Column ss:AutoFitWidth=\"0\" ss:Width=\"60\" /><Column ss:AutoFitWidth=\"0\" ss:Width=\"112.5\" />\
         <Column ss:AutoFitWidth=\"0\" ss:Width=\"60.75\" />",
    );
    let cell = |style: &str, extra: &str, v: &str| format!("<Cell{extra}{style}><Data ss:Type=\"String\">{}</Data></Cell>", text(v));
    let _ = write!(out, "<Row>{}</Row>", cell(" ss:StyleID=\"s23\"", " ss:Index=\"2\" ss:MergeAcross=\"4\"", &format!("{TITLE}{}", r.timestamp)));
    let _ = write!(
        out,
        "<Row>{}{}{}</Row>",
        cell(" ss:StyleID=\"s23\"", " ss:Index=\"2\" ss:MergeAcross=\"1\"", &source_cell(&r.source)),
        cell(" ss:StyleID=\"s21\"", "", "vs"),
        cell(" ss:StyleID=\"s23\"", " ss:MergeAcross=\"1\"", &source_cell(&r.target))
    );
    out.push_str("<Row>");
    for h in HEADINGS {
        out.push_str(&cell(" ss:StyleID=\"s24\"", "", h));
    }
    out.push_str("</Row>");
    for row in excel_rows(r) {
        out.push_str("<Row>");
        for v in &row {
            out.push_str(&cell("", "", v));
        }
        out.push_str("</Row>");
    }
    out.push_str("</Table></Worksheet></Workbook>");
    out
}

/// The Excel report as an Office Open XML workbook (.xlsx).
pub fn xlsx(r: &Report) -> Vec<u8> {
    let mut rows: Vec<(u32, Vec<(String, u32)>)> = vec![
        (1, vec![(String::new(), 0), (format!("{TITLE}{}", r.timestamp), 1)]),
        (2, vec![(String::new(), 0), (source_cell(&r.source), 1), (String::new(), 0), ("vs".to_string(), 0), (source_cell(&r.target), 1)]),
        (3, HEADINGS.iter().map(|h| (h.to_string(), 2)).collect()),
    ];
    for (n, row) in excel_rows(r).into_iter().enumerate() {
        rows.push((n as u32 + 4, row.into_iter().map(|v| (v, 0)).collect()));
    }
    let mut sheet = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
         <cols><col min=\"1\" max=\"1\" width=\"18\" customWidth=\"1\"/><col min=\"2\" max=\"2\" width=\"14\" customWidth=\"1\"/><col min=\"3\" max=\"3\" width=\"32\" customWidth=\"1\"/>\
         <col min=\"4\" max=\"4\" width=\"8\" customWidth=\"1\"/><col min=\"5\" max=\"5\" width=\"32\" customWidth=\"1\"/><col min=\"6\" max=\"6\" width=\"14\" customWidth=\"1\"/></cols><sheetData>",
    );
    for (n, cells) in &rows {
        let _ = write!(sheet, "<row r=\"{n}\">");
        for (c, (v, style)) in cells.iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let col = (b'A' + c as u8) as char;
            let _ = write!(sheet, "<c r=\"{col}{n}\" t=\"inlineStr\" s=\"{style}\"><is><t xml:space=\"preserve\">{}</t></is></c>", text(v));
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData><mergeCells count=\"3\"><mergeCell ref=\"B1:F1\"/><mergeCell ref=\"B2:C2\"/><mergeCell ref=\"E2:F2\"/></mergeCells></worksheet>");
    let files: [(&str, String); 6] = [
        (
            "[Content_Types].xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
             <Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>\
             <Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/></Types>"
                .into(),
        ),
        (
            "_rels/.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>"
                .into(),
        ),
        (
            "xl/workbook.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
             xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"QuillSQL Compare Report\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
                .into(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
             <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>"
                .into(),
        ),
        (
            "xl/styles.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
             <fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>\
             <fills count=\"3\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill>\
             <fill><patternFill patternType=\"solid\"><fgColor rgb=\"FFC0C0C0\"/><bgColor indexed=\"64\"/></patternFill></fill></fills>\
             <borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
             <cellXfs count=\"3\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>\
             <xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\"/></xf>\
             <xf numFmtId=\"0\" fontId=\"0\" fillId=\"2\" borderId=\"0\" xfId=\"0\" applyFill=\"1\"/></cellXfs></styleSheet>"
                .into(),
        ),
        ("xl/worksheets/sheet1.xml", sheet),
    ];
    zip_stored(&files.iter().map(|(n, c)| (*n, c.as_bytes())).collect::<Vec<_>>())
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A zip archive with the files stored uncompressed.
fn zip_stored(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    // 2026-01-01 00:00 in DOS time.
    let (time, date) = (0u16, ((2026 - 1980) << 9 | 1 << 5 | 1) as u16);
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend(sig.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend(20u16.to_le_bytes());
            h.extend(0u16.to_le_bytes()); // flags
            h.extend(0u16.to_le_bytes()); // stored
            h.extend(time.to_le_bytes());
            h.extend(date.to_le_bytes());
            h.extend(crc.to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes()); // extra
            if central {
                h.extend([0u8; 8]); // comment, disk, internal attributes
                h.extend(0u32.to_le_bytes()); // external attributes
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(*data);
        central.extend(header(0x0201_4b50, true));
    }
    let start = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        let line = |l: &str, r: &str, d: bool| Line { left: Some(l.into()), right: Some(r.into()), different: d };
        let mut items = vec![
            Item {
                object_type: ObjectType::View,
                kind: DiffKind::OnlyIn1,
                full_name: "[dbo].[V]".into(),
                owner1: "dbo".into(),
                name1: "V".into(),
                owner2: String::new(),
                name2: String::new(),
                lines: vec![Line { left: Some("CREATE VIEW V AS SELECT 1 AS [a<b]".into()), right: None, different: true }],
            },
            Item {
                object_type: ObjectType::Table,
                kind: DiffKind::Different,
                full_name: "[dbo].[T]".into(),
                owner1: "dbo".into(),
                name1: "T".into(),
                owner2: "dbo".into(),
                name2: "T".into(),
                lines: vec![line("CREATE TABLE [dbo].[T]", "CREATE TABLE [dbo].[T]", false), line("  [a] [int] NULL", "  [a] [bigint] NULL", true)],
            },
        ];
        order(&mut items);
        Report {
            timestamp: "7 Nov 2026 2:41 PM".into(),
            source: DataSource { kind: "live", server: "srv".into(), database: "Dev".into(), created: None },
            target: DataSource { kind: "snapshot", server: "srv".into(), database: "Prod".into(), created: Some("6 Nov 2026 9:00 AM".into()) },
            items,
            include_identical: false,
        }
    }

    #[test]
    fn xml_has_sql_compare_layout() {
        let x = xml_text(&report());
        assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"utf-16\"?><comparison direction=\"1to2\" timestamp=\"7 Nov 2026 2:41 PM\"><datasources>"));
        assert!(x.contains("<datasource type=\"live\" id=\"1\"><server>srv</server><database>Dev</database></datasource>"));
        assert!(x.contains("<datasource creationdate=\"6 Nov 2026 9:00 AM\" type=\"snapshot\" id=\"2\">"));
        // Only in 1 sorts before different.
        let v = x.find("fqn=\"view-[dbo].[V]\"").unwrap();
        assert!(v < x.find("fqn=\"table-[dbo].[T]\"").unwrap());
        assert!(x.contains("<difference objecttype=\"view\" status=\"onlyin1\" fqn=\"view-[dbo].[V]\"><object llastmodified=\"\" owner=\"dbo\" id=\"1\">V</object><object owner=\"\" rlastmodified=\"\" id=\"2\" />"));
        assert!(x.contains("<line type=\"different\"><left>CREATE VIEW V AS SELECT 1 AS [a&lt;b]</left><right> </right></line>"));
        assert!(x.contains("<line type=\"different\"><left>[a] [int] NULL</left><right>[a] [bigint] NULL</right></line>"));
        assert!(x.ends_with("</differences></comparison>"));
        assert_eq!(&xml(&report())[..4], &[0xFF, 0xFE, b'<', 0]);
        roxmltree::Document::parse(&x.replace("utf-16", "utf-8")).unwrap();
    }

    #[test]
    fn html_and_excel() {
        let r = report();
        let h = html(&r);
        assert!(h.contains("<b>1</b> modified") && h.contains("Before (target)") && h.contains("[bigint]"));
        assert!(classic(&r).contains("In srv.Dev only"));
        let e = excel(&r);
        assert!(e.contains("<Data ss:Type=\"String\">srv.Dev ( live ) </Data>"));
        let different = e.find("<Data ss:Type=\"String\">&lt;&gt;</Data>").unwrap();
        assert!(different < e.find("<Data ss:Type=\"String\">-&gt;</Data>").unwrap());
        roxmltree::Document::parse(&e).unwrap();
        let z = xlsx(&r);
        assert!(z.starts_with(b"PK\x03\x04") && z.windows(9).any(|w| w == b"Stored Pr" || w == b"Table</t>"));
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
