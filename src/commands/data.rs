//! `quill data`: compare the data of two databases and deploy the differences.

use std::path::Path;

use regex::{Regex, RegexBuilder};

use super::common::{self, Source};
use crate::args::{self, ArgError, Args, Switch, exit};
use crate::catalog::Database;
use crate::data::compare::{self, DataComparison, TableResult};
use crate::data::mapping::{DataMappings, TableOverride, TablePair};
use crate::data::store::{self, RowKind};
use crate::data::{DataOpt, DataOptions};
use crate::db::ConnectionSpec;

const ACTIONS: [Switch; 9] = [
    Switch {
        name: "options",
        aliases: &["o"],
        takes_value: true,
        repeatable: false,
        help: "Data compare options, comma-separated (a list without Default starts from none)",
    },
    Switch {
        name: "include",
        aliases: &[],
        takes_value: true,
        repeatable: true,
        help: "Table|View|All[:regex], or Identical / Different / Missing / Additional to choose the rows reported",
    },
    Switch { name: "exclude", aliases: &[], takes_value: true, repeatable: true, help: "Table|View|All[:regex] to leave out" },
    Switch {
        name: "includeColumns",
        aliases: &["ic"],
        takes_value: true,
        repeatable: true,
        help: "tableRegex:colRegex,colRegex — compare only these columns of matching tables",
    },
    Switch {
        name: "excludeColumns",
        aliases: &["ec"],
        takes_value: true,
        repeatable: true,
        help: "tableRegex:colRegex,colRegex — leave these columns out",
    },
    Switch {
        name: "comparisonkeys",
        aliases: &["ck"],
        takes_value: true,
        repeatable: true,
        help: "tableRegex:indexName (or col,col) — the key that pairs rows",
    },
    Switch { name: "export", aliases: &["e"], takes_value: true, repeatable: false, help: "Write CSV files of the results to a folder" },
    Switch {
        name: "assertidentical",
        aliases: &[],
        takes_value: false,
        repeatable: false,
        help: "Exit 0 when the data is identical, 79 when it differs",
    },
    Switch {
        name: "parallel",
        aliases: &[],
        takes_value: true,
        repeatable: false,
        help: "Tables compared at the same time (default 4)",
    },
];

fn switches() -> Vec<Switch> {
    common::GENERAL.into_iter().chain(common::SOURCE1).chain(common::SOURCE2).chain(ACTIONS).collect()
}

const SUMMARY: &str = "Compares the data of two databases table by table and deploys the differences.";

pub fn run(argv: &[String]) -> u8 {
    let switches = switches();
    let args = match args::parse(&switches, argv) {
        Ok(a) => a,
        Err(e) => return common::fail(e),
    };
    if args.has("help") || argv.is_empty() {
        print!("{}", args::usage("data", SUMMARY, &switches));
        return exit::SUCCESS;
    }
    match execute(&args) {
        Ok(code) => code,
        Err(e) => common::fail(e),
    }
}

fn regex(p: &str) -> Result<Regex, ArgError> {
    RegexBuilder::new(p).case_insensitive(true).build().map_err(|e| ArgError::new(exit::INVALID_VALUE, format!("'{p}': {e}")))
}

/// `/include` and `/exclude`: object rules plus the row kinds to report.
struct Rules {
    include: Vec<(Option<bool>, Option<Regex>)>,
    exclude: Vec<(Option<bool>, Option<Regex>)>,
    kinds: [bool; 4],
}

impl Rules {
    fn new(args: &Args) -> Result<Rules, ArgError> {
        let mut r = Rules { include: Vec::new(), exclude: Vec::new(), kinds: [false, true, true, true] };
        let mut kinds_given = false;
        for (list, is_include) in [(args.all("include"), true), (args.all("exclude"), false)] {
            for spec in list {
                let (ty, pattern) = match spec.split_once(':') {
                    Some((t, p)) => (t, Some(regex(p)?)),
                    None => (spec.as_str(), None),
                };
                let kind = match ty.to_ascii_lowercase().as_str() {
                    "identical" => Some(RowKind::Identical),
                    "different" => Some(RowKind::Different),
                    "missing" => Some(RowKind::OnlyIn1),
                    "additional" => Some(RowKind::OnlyIn2),
                    _ => None,
                };
                if let Some(k) = kind {
                    if is_include {
                        if k != RowKind::Identical && !kinds_given {
                            r.kinds = [r.kinds[0], false, false, false];
                            kinds_given = true;
                        }
                        r.kinds[k as usize] = true;
                    } else {
                        r.kinds[k as usize] = false;
                    }
                    continue;
                }
                let view = match ty.to_ascii_lowercase().as_str() {
                    "table" => Some(false),
                    "view" => Some(true),
                    "all" => None,
                    other => return Err(ArgError::new(exit::INVALID_VALUE, format!("'{other}' is not Table, View or All"))),
                };
                if is_include { r.include.push((view, pattern)) } else { r.exclude.push((view, pattern)) }
            }
        }
        Ok(r)
    }

    fn matches(rule: &(Option<bool>, Option<Regex>), p: &TablePair) -> bool {
        rule.0.is_none_or(|v| v == p.is_view)
            && rule.1.as_ref().is_none_or(|re| re.is_match(&p.source.to_string()) || re.is_match(&format!("{}.{}", p.source.schema, p.source.name)))
    }

    fn includes(&self, p: &TablePair) -> bool {
        !self.exclude.iter().any(|r| Self::matches(r, p)) && (self.include.is_empty() || self.include.iter().any(|r| Self::matches(r, p)))
    }
}

/// `/comparisonkeys`, `/includeColumns`, `/excludeColumns` as per-table overrides.
fn overrides(args: &Args, db1: &Database) -> Result<DataMappings, ArgError> {
    let mut m = DataMappings::default();
    let split = |s: &String, what: &str| -> Result<(Regex, String), ArgError> {
        let (t, rest) = s.split_once(':').ok_or_else(|| ArgError::new(exit::INVALID_VALUE, format!("/{what} needs tableRegex:value")))?;
        Ok((regex(t)?, rest.to_string()))
    };
    let entry = |m: &mut DataMappings, table: &crate::catalog::Table| -> usize {
        let name = table.object_name();
        if let Some(i) = m.tables.iter().position(|t| t.source == name) {
            return i;
        }
        m.tables.push(TableOverride { source: name, ..Default::default() });
        m.tables.len() - 1
    };
    let matches = |re: &Regex, t: &crate::catalog::Table| re.is_match(&t.object_name().to_string()) || re.is_match(&format!("{}.{}", t.schema, t.name));
    for spec in args.all("comparisonkeys") {
        let (re, key) = split(spec, "comparisonkeys")?;
        for t in db1.tables.iter().filter(|t| matches(&re, t)) {
            let cols: Vec<String> = match t.indexes.iter().find(|i| i.name.eq_ignore_ascii_case(key.trim_matches(['[', ']']))) {
                Some(ix) => ix.columns.iter().map(|c| c.name.clone()).collect(),
                None => key.trim_matches(['(', ')']).split(',').map(|c| c.trim().trim_matches(['[', ']']).to_string()).collect(),
            };
            let i = entry(&mut m, t);
            m.tables[i].key = cols;
        }
    }
    for (switch, include) in [("includeColumns", true), ("excludeColumns", false)] {
        for spec in args.all(switch) {
            let (re, cols) = split(spec, switch)?;
            let col_res: Vec<Regex> = cols.split(',').map(|c| regex(c.trim())).collect::<Result<_, _>>()?;
            for t in db1.tables.iter().filter(|t| matches(&re, t)) {
                let i = entry(&mut m, t);
                for c in &t.columns {
                    let hit = col_res.iter().any(|r| r.is_match(&c.name));
                    if hit != include && !m.tables[i].excluded_columns.contains(&c.name) {
                        m.tables[i].excluded_columns.push(c.name.clone());
                    }
                }
            }
        }
    }
    Ok(m)
}

fn live(source: Source, n: u8) -> Result<ConnectionSpec, ArgError> {
    match source {
        Source::Live(spec) => Ok(spec),
        Source::Snapshot(_) => Err(ArgError::new(exit::USAGE, format!("data compare needs database {n} to be a live database"))),
    }
}

fn execute(args: &Args) -> Result<u8, ArgError> {
    let spec1 = live(common::source(args, 1)?.ok_or_else(|| ArgError::new(exit::USAGE, "give database 1: /server1 and /database1"))?, 1)?;
    let spec2 = live(common::source(args, 2)?.ok_or_else(|| ArgError::new(exit::USAGE, "give database 2: /server2 and /database2"))?, 2)?;
    let mut options = match args.get("options") {
        Some(list) => DataOptions::parse(list).map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?,
        None => DataOptions::default_set(),
    };
    let rules = Rules::new(args)?;
    if rules.kinds[RowKind::Identical as usize] {
        options = options.with(DataOpt::IncludeIdenticalRecords);
    }
    let parallel = match args.get("parallel") {
        Some(p) => p.parse::<usize>().map_err(|_| ArgError::new(exit::INVALID_VALUE, format!("/parallel: '{p}' is not a number")))?,
        None => 4,
    };
    let quiet = args.has("quiet");
    let read = |spec: &ConnectionSpec| {
        if args.has("verbose") {
            eprintln!("Reading the schema of {}...", spec.describe());
        }
        crate::catalog::read_database(spec).map_err(|e| ArgError::new(exit::EXECUTION_FAILED, format!("{}: {e}", spec.describe())))
    };
    let db1 = read(&spec1)?;
    let db2 = read(&spec2)?;
    let user = overrides(args, &db1)?;
    let verbose = args.has("verbose");
    let result = compare::compare(&spec1, &spec2, &db1, &db2, &options, &user, |p| rules.includes(p), parallel, |p, r| {
        if verbose {
            eprintln!("Compared {} ({} rows differ)", p.source, r.count(RowKind::Different) + r.count(RowKind::OnlyIn1) + r.count(RowKind::OnlyIn2));
        }
    })
    .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, e))?;

    if !quiet {
        print!("{}", summary(&result, &rules, &db1.name, &db2.name));
    }
    if let Some(dir) = args.get("export") {
        export(&result, &rules, Path::new(dir), &db1.name, &db2.name).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{dir}: {e}")))?;
        if !quiet {
            println!("Results written to {dir}.");
        }
    }
    let failed: Vec<&TableResult> = result.tables.iter().filter(|t| t.error.is_some()).collect();
    for t in &failed {
        eprintln!("error: {}: {}", t.pair.source, t.error.as_deref().unwrap_or_default());
    }
    let identical = result.identical();
    if args.has("assertidentical") {
        return Ok(if identical && failed.is_empty() { exit::SUCCESS } else { exit::NOT_IDENTICAL });
    }
    if !failed.is_empty() {
        return Ok(exit::EXECUTION_FAILED);
    }
    if identical && !rules.kinds[RowKind::Identical as usize] {
        if !quiet {
            println!("The data is identical.");
        }
        return Ok(exit::IDENTICAL);
    }
    Ok(exit::SUCCESS)
}

fn summary(result: &DataComparison, rules: &Rules, db1: &str, db2: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let width = result.tables.iter().map(|t| t.pair.source.to_string().chars().count()).max().unwrap_or(4).clamp(4, 60);
    let _ = writeln!(out, "{:<6} {:<width$} {:>10} {:>10} {:>10} {:>10}", "Type", "Name", "Different", &format!("{db1} only")[..format!("{db1} only").len().min(10)], &format!("{db2} only")[..format!("{db2} only").len().min(10)], "Identical");
    for t in &result.tables {
        let n = |k: RowKind| t.count(k);
        let status = if let Some(e) = &t.error {
            format!("  error: {e}")
        } else if t.by_checksum {
            "  (checksum)".to_string()
        } else if t.repeated_keys {
            "  (key values repeat)".to_string()
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "{:<6} {:<width$} {:>10} {:>10} {:>10} {:>10}{status}",
            if t.pair.is_view { "View" } else { "Table" },
            t.pair.source.to_string(),
            if rules.kinds[1] { n(RowKind::Different).to_string() } else { "-".into() },
            if rules.kinds[2] { n(RowKind::OnlyIn1).to_string() } else { "-".into() },
            if rules.kinds[3] { n(RowKind::OnlyIn2).to_string() } else { "-".into() },
            n(RowKind::Identical),
        );
    }
    for t in &result.only_in_1 {
        let _ = writeln!(out, "Table  {t} exists only in {db1}");
    }
    for t in &result.only_in_2 {
        let _ = writeln!(out, "Table  {t} exists only in {db2}");
    }
    out
}

fn csv(field: &str) -> String {
    format!("\"{}\"", field.replace('"', "\"\"").replace("\r\n", "\n"))
}

/// `Results Summary.csv` and one CSV per table with differing rows.
fn export(result: &DataComparison, rules: &Rules, dir: &Path, db1: &str, db2: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut s = String::from("\u{feff}");
    s.push_str(&[csv("Object Type"), csv("Object Owner"), csv("Object Name"), csv(&format!("{db1} Only")), csv("Different"), csv(&format!("{db2} Only")), csv("Identical")].join(","));
    s.push('\n');
    for t in &result.tables {
        s.push_str(
            &[
                csv(if t.pair.is_view { "View" } else { "Table" }),
                csv(&t.pair.source.schema),
                csv(&t.pair.source.name),
                csv(&t.count(RowKind::OnlyIn1).to_string()),
                csv(&t.count(RowKind::Different).to_string()),
                csv(&t.count(RowKind::OnlyIn2).to_string()),
                csv(&t.count(RowKind::Identical).to_string()),
            ]
            .join(","),
        );
        s.push('\n');
    }
    std::fs::write(dir.join("Results Summary.csv"), s)?;
    for t in result.tables.iter().filter(|t| t.error.is_none()) {
        let kinds: Vec<RowKind> = RowKind::ALL.into_iter().filter(|k| rules.kinds[*k as usize] && t.count(*k) > 0 && !t.offsets[*k as usize].is_empty()).collect();
        if kinds.is_empty() {
            continue;
        }
        let p = &t.pair;
        let mut header = vec![csv("Type")];
        for (i, c) in p.columns.iter().enumerate() {
            if p.key.contains(&i) {
                header.push(csv(&format!("{}_k", c.source)));
            } else {
                header.push(csv(&format!("{}_s", c.source)));
                header.push(csv(&format!("{}_t", c.target)));
            }
        }
        let mut out = format!("\u{feff}{}\n", header.join(","));
        for k in kinds {
            for row in store::read_all(&t.path, &t.offsets[k as usize])? {
                let label = match k {
                    RowKind::Identical => "Same",
                    RowKind::Different => "Different",
                    RowKind::OnlyIn1 => "In1",
                    RowKind::OnlyIn2 => "In2",
                };
                let mut fields = vec![csv(label)];
                for i in 0..p.columns.len() {
                    let s = row.source.get(i).map(|v| v.display()).unwrap_or_default();
                    let tv = row.target.get(i).map(|v| v.display()).unwrap_or_default();
                    if p.key.contains(&i) {
                        fields.push(csv(if row.source.is_empty() { &tv } else { &s }));
                    } else {
                        fields.push(csv(&s));
                        fields.push(csv(&tv));
                    }
                }
                out.push_str(&fields.join(","));
                out.push('\n');
            }
        }
        let file: String = format!("{}.{}.csv", p.source.schema, p.source.name)
            .chars()
            .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') { c } else { ' ' })
            .collect();
        std::fs::write(dir.join(file), out)?;
    }
    Ok(())
}
