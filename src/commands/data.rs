//! `quill data`: compare the data of two databases and deploy the differences, with the
//! switches, console summary, CSV export and exit codes of SQLDataCompare.exe.

use std::cell::RefCell;
use std::path::Path;

use regex::{Regex, RegexBuilder};

use super::common::{self, Source};
use crate::args::{self, ArgError, Args, Switch, exit};
use crate::catalog::Database;
use crate::data::compare::{self, DataComparison, TableResult};
use crate::data::mapping::{DataMappings, TableOverride, TablePair};
use crate::data::store::{self, RowKind};
use crate::data::value::Value;
use crate::data::{DataOpt, DataOptions, sync};
use crate::db::ConnectionSpec;
use crate::schema::deploy;

/// SQLDataCompare.exe's own exit codes.
const NOTHING_TO_COMPARE: u8 = 60;
const CANNOT_CREATE: u8 = 73;

const NOTHING_TO_COMPARE_TEXT: &str = "Nothing to compare.  Either there are no tables/views that can be compared, or everything has been filtered out with /include or /exclude. Use /Include:Identical to see tables/views that exist in both databases but could not be compared.";

const ACTIONS: [Switch; 18] = [
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
        aliases: &["ic", "columns", "cols"],
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
        help: "tableRegex:indexName (or tableRegex:(col,col)) — the key that pairs rows",
    },
    Switch { name: "casesensitive", aliases: &["cs"], takes_value: false, repeatable: false, help: "Same as /options:CaseSensitiveObjectDefinition (unless IgnoreCase)" },
    Switch { name: "export", aliases: &["e"], takes_value: true, repeatable: false, help: "Write CSV files of the results to a folder" },
    Switch {
        name: "exportidenticaltables",
        aliases: &["eit"],
        takes_value: false,
        repeatable: false,
        help: "With /export: identical tables are not an error (deprecated; use /include:Identical)",
    },
    Switch {
        name: "allowIdenticalDatabases",
        aliases: &["aidb", "aid"],
        takes_value: false,
        repeatable: false,
        help: "Identical databases exit 0 rather than 63 (deprecated; use /include:Identical)",
    },
    Switch {
        name: "assertidentical",
        aliases: &[],
        takes_value: false,
        repeatable: false,
        help: "Exit 0 when the data is identical, 79 when it differs",
    },
    Switch {
        name: "MaxTransactionSizeMB",
        aliases: &["mts"],
        takes_value: true,
        repeatable: false,
        help: "Split the deployment into transactions of at most this many MB (0 = one transaction)",
    },
    Switch { name: "outputProject", aliases: &["outpr"], takes_value: true, repeatable: false, help: "Save the settings used as a project file" },
    Switch {
        name: "makescripts",
        aliases: &["mkscr"],
        takes_value: true,
        repeatable: false,
        help: "Write the data of database 1 as INSERT scripts to <folder>\\Data and stop",
    },
    Switch { name: "out", aliases: &[], takes_value: true, repeatable: false, help: "Also write the console output to a file" },
    Switch { name: "outputwidth", aliases: &[], takes_value: true, repeatable: false, help: "Width of the summary (80 to 1024; default 80)" },
    Switch { name: "html", aliases: &[], takes_value: false, repeatable: false, help: "With /help: print the help as HTML" },
    Switch {
        name: "parallel",
        aliases: &[],
        takes_value: true,
        repeatable: false,
        help: "Tables compared at the same time (default 4)",
    },
];

fn switches() -> Vec<Switch> {
    common::GENERAL.into_iter().chain(common::SOURCE1).chain(common::SOURCE2).chain(ACTIONS).chain(common::DEPLOY).collect()
}

const SUMMARY: &str = "Compares the data of two databases table by table and deploys the differences.";

thread_local! {
    /// Everything printed to the console, for `/out`.
    static OUTPUT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Prints a line (and keeps it for `/out`).
fn say(line: &str) {
    println!("{line}");
    OUTPUT.with(|o| {
        if let Some(buf) = o.borrow_mut().as_mut() {
            buf.push_str(line);
            buf.push_str("\r\n");
        }
    });
}

fn warn(line: &str) {
    eprintln!("Warning: {line}");
    OUTPUT.with(|o| {
        if let Some(buf) = o.borrow_mut().as_mut() {
            buf.push_str(&format!("Warning: {line}\r\n"));
        }
    });
}

pub fn run(argv: &[String]) -> u8 {
    let switches = switches();
    let args = match args::parse(&switches, argv) {
        Ok(a) => a,
        Err(e) => return common::fail(e),
    };
    if args.has("help") || argv.is_empty() {
        let text = args::usage("data", SUMMARY, &switches);
        if args.has("html") {
            print!("{}", html_help(&text));
        } else {
            print!("{text}");
        }
        return exit::SUCCESS;
    }
    if args.has("out") {
        OUTPUT.with(|o| *o.borrow_mut() = Some(String::new()));
    }
    let code = match execute(&args) {
        Ok(code) => code,
        Err(e) => {
            OUTPUT.with(|o| {
                if let Some(buf) = o.borrow_mut().as_mut() {
                    buf.push_str(&format!("error: {}\r\n", e.message));
                }
            });
            common::fail(e)
        }
    };
    if let Some(path) = args.get("out") {
        let text = OUTPUT.with(|o| o.borrow_mut().take()).unwrap_or_default();
        if let Err(e) = std::fs::write(path, text) {
            eprintln!("error: {path}: {e}");
            return exit::IO_ERROR;
        }
    }
    code
}

fn html_help(text: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut out = String::from("<html><head><title>quill data</title></head><body>\n");
    let mut lines = text.lines();
    if let Some(first) = lines.next() {
        out.push_str(&format!("<p>{}</p>\n", esc(first)));
    }
    out.push_str("<table>\n");
    for line in lines.filter(|l| l.starts_with("  /")) {
        let (names, help) = line.trim_start().split_at(line.trim_start().find("  ").unwrap_or(line.trim_start().len()));
        out.push_str(&format!("<tr><td><code>{}</code></td><td>{}</td></tr>\n", esc(names.trim()), esc(help.trim())));
    }
    out.push_str("</table>\n</body></html>\n");
    out
}

fn regex(p: &str, case_sensitive: bool) -> Result<Regex, ArgError> {
    RegexBuilder::new(p).case_insensitive(!case_sensitive).build().map_err(|e| ArgError::new(exit::INVALID_VALUE, format!("'{p}': {e}")))
}

/// `/include` and `/exclude`: object rules plus the row kinds to report.
struct Rules {
    include: Vec<(Option<bool>, Option<Regex>)>,
    exclude: Vec<(Option<bool>, Option<Regex>)>,
    kinds: [bool; 4],
    identical_allowed: bool,
}

impl Rules {
    fn new(args: &Args, case_sensitive: bool) -> Result<Rules, ArgError> {
        let mut r = Rules { include: Vec::new(), exclude: Vec::new(), kinds: [false, true, true, true], identical_allowed: false };
        let mut kinds_given = false;
        for (list, is_include) in [(args.all("include"), true), (args.all("exclude"), false)] {
            for spec in list {
                let (ty, pattern) = match spec.split_once(':') {
                    Some((t, p)) => (t, Some(regex(p, case_sensitive)?)),
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
                        if k == RowKind::Identical {
                            r.identical_allowed = true;
                        } else if !kinds_given {
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
        if args.has("allowIdenticalDatabases") || (args.has("export") && args.has("exportidenticaltables")) {
            r.identical_allowed = true;
        }
        Ok(r)
    }

    fn matches(rule: &(Option<bool>, Option<Regex>), p: &TablePair) -> bool {
        rule.0.is_none_or(|v| v == p.is_view)
            && rule.1.as_ref().is_none_or(|re| re.is_match(&p.source.to_string()) || re.is_match(&p.target.to_string()))
    }

    fn includes(&self, p: &TablePair) -> bool {
        !self.exclude.iter().any(|r| Self::matches(r, p)) && (self.include.is_empty() || self.include.iter().any(|r| Self::matches(r, p)))
    }
}

/// `/comparisonkeys`, `/includeColumns`, `/excludeColumns` as per-table overrides. Returns the
/// overrides and, for each `/comparisonkeys` index, the table and index name to check.
fn overrides(args: &Args, db1: &Database, case_sensitive: bool) -> Result<(DataMappings, Vec<(crate::catalog::ObjectName, String)>), ArgError> {
    let mut m = DataMappings::default();
    let mut keys = Vec::new();
    let split = |s: &String, what: &str| -> Result<(Regex, String), ArgError> {
        let (t, rest) = s.split_once(':').ok_or_else(|| {
            ArgError::new(
                exit::USAGE,
                format!("Invalid value for argument /{what}.  Expected to find a table / view matching regular expression and a column matching regular expression."),
            )
        })?;
        Ok((regex(t, case_sensitive)?, rest.to_string()))
    };
    let entry = |m: &mut DataMappings, table: &crate::catalog::Table| -> usize {
        let name = table.object_name();
        if let Some(i) = m.tables.iter().position(|t| t.source == name) {
            return i;
        }
        m.tables.push(TableOverride { source: name, ..Default::default() });
        m.tables.len() - 1
    };
    let matches = |re: &Regex, t: &crate::catalog::Table| re.is_match(&t.object_name().to_string());
    for spec in args.all("comparisonkeys") {
        let (re, key) = split(spec, "comparisonkeys")?;
        for t in db1.tables.iter().filter(|t| matches(&re, t)) {
            let i = entry(&mut m, t);
            if key.starts_with('(') && key.ends_with(')') {
                m.tables[i].key = key[1..key.len() - 1].split(',').map(|c| c.trim().trim_matches(['[', ']']).to_string()).collect();
            } else {
                let name = key.trim_matches(['[', ']']).to_string();
                m.tables[i].key = vec![format!("index:{name}")];
                keys.push((t.object_name(), name));
            }
        }
    }
    for (switch, include) in [("includeColumns", true), ("excludeColumns", false)] {
        for spec in args.all(switch) {
            let (re, cols) = split(spec, switch)?;
            let col_res: Vec<Regex> = cols.split(',').map(|c| regex(c.trim(), case_sensitive)).collect::<Result<_, _>>()?;
            for t in db1.tables.iter().filter(|t| matches(&re, t)) {
                let i = entry(&mut m, t);
                for c in &t.columns {
                    let hit = col_res.iter().any(|r| r.is_match(&c.name));
                    if include {
                        let excluded = m.tables[i].excluded_columns.contains(&c.name);
                        if hit && excluded {
                            m.tables[i].excluded_columns.retain(|x| x != &c.name);
                        } else if !hit && !excluded {
                            m.tables[i].excluded_columns.push(c.name.clone());
                        }
                    } else if hit && !m.tables[i].excluded_columns.contains(&c.name) {
                        m.tables[i].excluded_columns.push(c.name.clone());
                    }
                }
            }
        }
    }
    Ok((m, keys))
}

fn live(source: Source, n: u8) -> Result<ConnectionSpec, ArgError> {
    match source {
        Source::Live(spec) => Ok(spec),
        Source::Snapshot(_) => Err(ArgError::new(exit::USAGE, format!("data compare needs database {n} to be a live database"))),
    }
}

fn execute(args: &Args) -> Result<u8, ArgError> {
    // Dependency checks first, as SQLDataCompare.exe does.
    if !["project", "database2", "connection2", "snapshot2", "makescripts"].iter().any(|s| args.has(s)) {
        return Err(ArgError::new(
            exit::USAGE,
            "You must specify one of the following argument(s): /project, /database2, /backup2, /scripts2, /makescripts, /sourcecontrol2, /sca2, /empty2",
        ));
    }
    for n in 1..=2 {
        if args.has(&format!("password{n}")) && !args.has(&format!("username{n}")) && !args.has("project") {
            return Err(ArgError::new(
                exit::UNSATISFIED_ARGUMENT,
                format!("The /password{n} switch cannot be used without the /username{n} or /project switches."),
            ));
        }
    }
    let project = common::project_of(args, crate::project::ProjectKind::Data)?;
    // Without a project /options start from the defaults; with one they are added to its options.
    let base = if project.is_some() { DataOptions::none() } else { DataOptions::default_set() };
    let resolved = DataOptions::resolve(args.get("options"), base, args.has("casesensitive"));
    for w in &resolved.warnings {
        warn(w);
    }
    let mut options = match &project {
        Some(p) => DataOptions::parse(&p.options.join(",")).map_err(|e| ArgError::new(exit::DATA_ERROR, e))?.union(&resolved.options),
        None => resolved.options,
    };
    let case_sensitive = options.has(DataOpt::CaseSensitiveObjectDefinition);
    let rules = Rules::new(args, case_sensitive)?;
    if rules.kinds[RowKind::Identical as usize] {
        options = options.with(DataOpt::IncludeIdenticalRecords);
    }
    let parallel = match args.get("parallel") {
        Some(p) => p.parse::<usize>().map_err(|_| ArgError::new(exit::INVALID_VALUE, format!("/parallel: '{p}' is not a number")))?,
        None => 4,
    };
    let width = match args.get("outputwidth") {
        Some(w) => w.parse::<usize>().ok().filter(|w| (80..=1024).contains(w)).ok_or_else(|| ArgError::new(exit::INVALID_VALUE, format!("Invalid value for switch /outputwidth: {w}.")))?,
        None => 80,
    };
    let max_mb = match args.get("MaxTransactionSizeMB") {
        Some(v) => v.parse::<u64>().map_err(|_| ArgError::new(exit::INVALID_VALUE, format!("Invalid value for switch /MaxTransactionSizeMB: {v}.")))?,
        None => 0,
    };
    let quiet = args.has("quiet");
    let verbose = args.has("verbose");
    let spec1 = live(common::source(args, 1)?.ok_or_else(|| ArgError::new(exit::USAGE, "give database 1: /server1 and /database1"))?, 1)?;
    let read = |spec: &ConnectionSpec| {
        if verbose {
            eprintln!("Reading the schema of {}...", spec.describe());
        }
        crate::catalog::read_database(spec).map_err(|e| ArgError::new(exit::EXECUTION_FAILED, format!("{}: {e}", spec.describe())))
    };
    if let Some(dir) = args.get("makescripts") {
        let db1 = read(&spec1)?;
        return make_scripts(&spec1, &db1, &options, Path::new(dir), args);
    }
    let spec2 = live(common::source(args, 2)?.ok_or_else(|| ArgError::new(exit::USAGE, "give database 2: /server2 and /database2"))?, 2)?;
    let db1 = read(&spec1)?;
    let db2 = read(&spec2)?;
    let (mut user, keys) = overrides(args, &db1, case_sensitive)?;
    if let Some(p) = &project {
        user.schemas.extend(p.data_mappings.schemas.iter().cloned());
        for t in &p.data_mappings.tables {
            match user.tables.iter_mut().find(|u| u.source == t.source) {
                Some(u) => {
                    if u.target.is_none() {
                        u.target = t.target.clone();
                    }
                    u.columns.extend(t.columns.iter().cloned());
                    u.excluded |= t.excluded;
                }
                None => user.tables.push(t.clone()),
            }
        }
    }
    // A /comparisonkeys index must pair on both sides.
    let (pairs, _, _) = crate::data::mapping::map_tables(&db1, &db2, &options, &user);
    for (table, index) in &keys {
        let ok = pairs.iter().any(|p| p.source == *table && p.key_name.as_deref().is_some_and(|k| if case_sensitive { k == index } else { k.eq_ignore_ascii_case(index) }));
        if !ok {
            return Err(ArgError::new(
                exit::INVALID_VALUE,
                format!("Invalid comparison keys specified for {table} '{index}'.  Only a single key or unique index may be specified."),
            ));
        }
    }
    let deselected: Vec<crate::catalog::ObjectName> = project.iter().flat_map(|p| p.deselected.iter().map(|d| crate::catalog::ObjectName::new(&d.schema, &d.name))).collect();
    let result = compare::compare(&spec1, &spec2, &db1, &db2, &options, &user, |p| rules.includes(p) && !deselected.contains(&p.source), parallel, |p, r| {
        if verbose {
            eprintln!("Compared {} ({} rows differ)", p.source, r.count(RowKind::Different) + r.count(RowKind::OnlyIn1) + r.count(RowKind::OnlyIn2));
        }
    })
    .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, e))?;

    if !quiet {
        say("Finished");
        for line in summary(&result, &rules, &describe(&spec1, &db1), &describe(&spec2, &db2), width) {
            say(&line);
        }
        if verbose {
            say("OK");
        }
    }
    for t in &result.tables {
        for n in t.pair.notes.iter().filter(|n| n.not_compared() || verbose) {
            warn(&n.text);
        }
        if let Some(problem) = &t.pair.problem {
            warn(&format!("{} is not compared: {problem}", t.pair.source));
        }
    }
    if let Some(dir) = args.get("export") {
        export(&result, &rules, Path::new(dir), &db1.name, &db2.name).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{dir}: {e}")))?;
    }
    let failed: Vec<&TableResult> = result.tables.iter().filter(|t| t.error.is_some() && t.pair.problem.is_none()).collect();
    for t in &failed {
        eprintln!("error: {}: {}", t.pair.source, t.error.as_deref().unwrap_or_default());
    }
    let compared = result.tables.iter().filter(|t| t.pair.problem.is_none()).count();
    let differs = result.tables.iter().any(|t| t.error.is_none() && !t.identical());
    let assert = args.has("assertidentical");
    if !rules.identical_allowed && compared == 0 && !assert {
        return Err(ArgError::new(NOTHING_TO_COMPARE, NOTHING_TO_COMPARE_TEXT));
    }
    if !differs && !rules.identical_allowed && failed.is_empty() {
        return Err(ArgError::new(exit::IDENTICAL, NOTHING_TO_COMPARE_TEXT));
    }
    let mut script = None;
    if args.has("scriptfile") || args.has("synchronize") {
        let mut settings = sync::SyncSettings::new(&describe(&spec1, &db1), &describe(&spec2, &db2), &db2.name);
        settings.transaction_isolation_level = args.get("transactionIsolationLevel").unwrap_or("SERIALIZABLE").to_ascii_uppercase();
        settings.kinds = rules.kinds;
        settings.max_transaction_bytes = (max_mb > 0).then_some(max_mb * 1_048_576);
        settings.source_connection = Some(spec1.clone());
        let s = sync::generate(&result, &db2, &options, &settings).map_err(|e| ArgError::new(exit::GENERAL_ERROR, e))?;
        common::write_script(args, &s)?;
        common::report_warnings(args, &s);
        common::check_warnings(args, &s)?;
        script = Some(s);
    }
    if assert && differs {
        return Err(ArgError::new(exit::NOT_IDENTICAL, "Exiting with code 79 as /assertidentical was specified"));
    }
    if let Some(out) = args.get("outputProject") {
        save_project(out, project.as_ref(), &spec1, &spec2, &options, &user, args)?;
    }
    if args.has("synchronize")
        && let Some(script) = &script
    {
        if script.is_empty() {
            if !quiet {
                say("Nothing to deploy.");
            }
        } else {
            let ran = deploy::execute(&spec2, script, |i, n| {
                if verbose {
                    eprintln!("Running batch {} of {n}", i + 1);
                }
            })
            .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, e))?;
            if !quiet {
                say(&format!("Deployed the data of {} to {} ({ran} batches).", db1.name, spec2.describe()));
            }
        }
    }
    if !failed.is_empty() {
        return Ok(exit::EXECUTION_FAILED);
    }
    Ok(exit::SUCCESS)
}

/// `server.database`, as the summary and the script header name a database.
fn describe(spec: &ConnectionSpec, db: &Database) -> String {
    format!("{}.{}", spec.server, if db.name.is_empty() { &spec.database } else { &db.name })
}

/// `/outputProject`: the settings of this run as a project.
fn save_project(
    path: &str,
    project: Option<&crate::project::Project>,
    spec1: &ConnectionSpec,
    spec2: &ConnectionSpec,
    options: &DataOptions,
    user: &DataMappings,
    args: &Args,
) -> Result<(), ArgError> {
    if Path::new(path).exists() && !args.has("force") {
        return Err(ArgError::new(CANNOT_CREATE, format!("Unable to write project to output file '{path}' because the file already exists.")));
    }
    let mut p = project.cloned().unwrap_or_else(|| crate::project::Project::new(crate::project::ProjectKind::Data));
    let side = |spec: &ConnectionSpec, n: u8| {
        let mut s = crate::project::SourceSpec::database(&spec.server, &spec.database);
        s.encrypt = spec.encrypt;
        s.trust_server_certificate = spec.trust_server_certificate;
        if let crate::db::Auth::Sql { user, .. } = &spec.auth {
            s.authentication = crate::project::Authentication::Sql;
            s.user = user.clone();
        }
        let given = ["server", "database", "connection"].iter().any(|k| args.has(&format!("{k}{n}")));
        (given, s)
    };
    for (n, spec) in [(1, spec1), (2, spec2)] {
        let (given, s) = side(spec, n);
        if given || project.is_none() {
            if n == 1 { p.source = s } else { p.target = s }
        }
    }
    p.options = options.iter().map(|o| o.name().to_string()).collect();
    p.data_mappings = user.clone();
    p.include = args.all("include").to_vec();
    p.exclude = args.all("exclude").to_vec();
    p.save(Path::new(path)).map_err(|e| ArgError::new(CANNOT_CREATE, format!("Error occurred writing project to file '{path}': {e}")))
}

/// `/makescripts`: every table of database 1 as INSERT statements in `<dir>\Data`.
fn make_scripts(spec: &ConnectionSpec, db: &Database, opts: &DataOptions, dir: &Path, args: &Args) -> Result<u8, ArgError> {
    if dir.as_os_str().is_empty() {
        return Err(ArgError::new(exit::INVALID_VALUE, "Blank or empty path names are not permitted.  Syntax error in argument /makescripts."));
    }
    let io = |e: std::io::Error| ArgError::new(exit::IO_ERROR, format!("{}: {e}", dir.display()));
    if args.has("force") && dir.exists() {
        for e in std::fs::read_dir(dir).map_err(io)?.flatten() {
            let p = e.path();
            if p.is_dir() { std::fs::remove_dir_all(&p).map_err(io)? } else { std::fs::remove_file(&p).map_err(io)? }
        }
    }
    let data = dir.join("Data");
    std::fs::create_dir_all(&data).map_err(io)?;
    let (pairs, _, _) = crate::data::mapping::map_tables(db, db, opts, &DataMappings::default());
    for p in pairs.iter().filter(|p| !p.is_view) {
        let text = compare::script_rows(spec, db, p).map_err(|e| ArgError::new(exit::EXECUTION_FAILED, format!("{}: {e}", p.source)))?;
        if text.is_empty() {
            continue;
        }
        let name: String = format!("{}.{}_Data.sql", p.source.schema, p.source.name)
            .chars()
            .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
            .collect();
        std::fs::write(data.join(name), [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat()).map_err(io)?;
    }
    if !args.has("quiet") {
        say(&format!("Data scripts written to {}.", data.display()));
    }
    Ok(exit::SUCCESS)
}

/// The left `max` characters kept around `...`, as SQLDataCompare.exe shortens long names.
fn left_truncate(s: &str, prefix: usize, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let tail: String = chars[chars.len() - max + prefix + 3..].iter().collect();
    format!("{}...{tail}", chars[..prefix].iter().collect::<String>())
}

/// Cuts to `len - 1` characters when too long, then pads to `len`.
fn force_length(s: &str, len: usize) -> String {
    let mut s: String = s.to_string();
    if s.chars().count() >= len {
        s = s.chars().take(len - 1).collect();
    }
    format!("{s:<len$}")
}

/// The console summary, line by line.
fn summary(result: &DataComparison, rules: &Rules, db1: &str, db2: &str, columns: usize) -> Vec<String> {
    let (ty_w, rec_w, db_w) = (16, 13, 4);
    let name_w = columns - ty_w - rec_w - db_w - db_w - 1;
    let mut out = vec![String::new(), "Summary Information".into(), "=".repeat(columns - 1), format!("DB1 = {db1}"), format!("DB2 = {db2}"), String::new()];
    out.push(format!("{:<ty_w$}{:<name_w$}{:<rec_w$}{:<db_w$}{:<db_w$}", "Object type", "Name", "Records", "DB1", "DB2"));
    let rule = "-".repeat(columns - 1);
    out.push(rule.clone());
    let identical = rules.kinds[RowKind::Identical as usize];
    for t in &result.tables {
        let p = &t.pair;
        let mut lines = Vec::new();
        let count = |n: u64| format!("{n:>w$}  ", w = rec_w - 2);
        if p.problem.is_some() {
            if identical {
                lines.push("No PK or UQ  x   x".to_string());
            }
        } else {
            if t.count(RowKind::Identical) > 0 && identical {
                lines.push(format!("{}==  ==  ", count(t.count(RowKind::Identical))));
            }
            if t.count(RowKind::Different) > 0 && rules.kinds[RowKind::Different as usize] {
                lines.push(format!("{}<>  <>  ", count(t.count(RowKind::Different))));
            }
            if t.count(RowKind::OnlyIn1) > 0 && rules.kinds[RowKind::OnlyIn1 as usize] {
                lines.push(format!("{}>>      ", count(t.count(RowKind::OnlyIn1))));
            }
            if t.count(RowKind::OnlyIn2) > 0 && rules.kinds[RowKind::OnlyIn2 as usize] {
                lines.push(format!("{}    <<  ", count(t.count(RowKind::OnlyIn2))));
            }
            if lines.is_empty() && identical {
                lines.push("    No data  x   x".to_string());
            }
        }
        let name = p.source.to_string();
        let mut suffix = String::new();
        let moved = p.source.schema != p.target.schema;
        if moved {
            suffix.push_str(&format!("->[{}]", p.target.schema));
        }
        if p.source.name != p.target.name {
            suffix.push_str(if moved { "." } else { "->" });
            suffix.push_str(&format!("[{}]", p.target.name));
        }
        let (shown, second) = if name.chars().count() + suffix.chars().count() > name_w - 1 {
            (name, format!("\r\n{}{}", " ".repeat(ty_w), force_length(&suffix, name_w)))
        } else {
            (format!("{name}{suffix}"), String::new())
        };
        let shown = force_length(&left_truncate(&shown, 3.max(((name_w - 1) as f32 * 0.3) as usize), name_w - 1), name_w);
        let kind = force_length(if p.is_view { "View" } else { "Table" }, ty_w);
        for l in lines {
            out.push(format!("{kind}{shown}{l}{second}"));
        }
    }
    out.push(rule);
    out.push("Please note: Only included differences are displayed above and only included differences are deployed".into());
    out
}

fn csv(field: &str) -> String {
    format!("\"{}\"", field.replace('"', "\"\"").replace("\r\n", "\n"))
}

/// `Results Summary.csv` and one CSV per table with rows of an included kind, as SQL Data
/// Compare writes them (UTF-8 with BOM, CRLF, every field quoted).
fn export(result: &DataComparison, rules: &Rules, dir: &Path, db1: &str, db2: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let k = |kind: RowKind| rules.kinds[kind as usize];
    let mut head = vec![csv("Object Type"), csv("Object Owner"), csv("Object Name")];
    if k(RowKind::OnlyIn1) {
        head.push(csv(&format!("{db1} Only")));
    }
    if k(RowKind::Different) {
        head.push(csv("Different"));
    }
    if k(RowKind::OnlyIn2) {
        head.push(csv(&format!("{db2} Only")));
    }
    head.push(csv("Object Name"));
    head.push(csv("Object Owner"));
    if k(RowKind::Identical) {
        head.push(csv("All Identical"));
    }
    let mut s = format!("\u{feff}{}\r\n", head.join(","));
    let compared: Vec<&TableResult> = result.tables.iter().filter(|t| t.pair.problem.is_none()).collect();
    for t in &compared {
        let p = &t.pair;
        let mut row = vec![csv(if p.is_view { "View" } else { "Table" }), csv(&p.source.schema), csv(&p.source.name)];
        for kind in [RowKind::OnlyIn1, RowKind::Different, RowKind::OnlyIn2] {
            if k(kind) {
                row.push(csv(&t.count(kind).to_string()));
            }
        }
        row.push(csv(&p.target.name));
        row.push(csv(&p.target.schema));
        if k(RowKind::Identical) {
            row.push(csv(&t.count(RowKind::Identical).to_string()));
        }
        s.push_str(&row.join(","));
        s.push_str("\r\n");
    }
    std::fs::write(dir.join("Results Summary.csv"), s)?;
    for t in compared.iter().filter(|t| t.error.is_none()) {
        let kinds: Vec<RowKind> = [RowKind::Identical, RowKind::Different, RowKind::OnlyIn1, RowKind::OnlyIn2].into_iter().filter(|&x| k(x) && !t.offsets[x as usize].is_empty()).collect();
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
        let mut out = format!("\u{feff}{}\r\n", header.join(","));
        for kind in kinds {
            for row in store::read_all(&t.path, &t.offsets[kind as usize])? {
                let label = match kind {
                    RowKind::Identical => "Same",
                    RowKind::Different => "Different",
                    RowKind::OnlyIn1 => "In1",
                    RowKind::OnlyIn2 => "In2",
                };
                let mut fields = vec![csv(label)];
                for (i, c) in p.columns.iter().enumerate() {
                    let side = |vals: &[Value], ty| vals.get(i).map(|v: &Value| v.csv_text(ty)).unwrap_or_default();
                    let (s, tv) = (side(&row.source, &c.source_type), side(&row.target, &c.target_type));
                    if p.key.contains(&i) {
                        fields.push(csv(if row.source.is_empty() { &tv } else { &s }));
                    } else if kind == RowKind::Identical {
                        fields.push(csv(&s));
                        fields.push(csv(&s));
                    } else {
                        fields.push(csv(&s));
                        fields.push(csv(&tv));
                    }
                }
                out.push_str(&fields.join(","));
                out.push_str("\r\n");
            }
        }
        let file: String = p.source.to_string().replace(['[', ']'], "").chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c < ' ' { ' ' } else { c }).collect();
        std::fs::write(dir.join(format!("{file}.csv")), out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_layout() {
        let session = crate::data::store::Session::create().unwrap();
        let pair = |s: &str, t: &str| TablePair {
            source: crate::catalog::ObjectName::new("dbo", s),
            target: crate::catalog::ObjectName::new("dbo", t),
            columns: vec![],
            key: vec![],
            key_name: None,
            target_key_name: None,
            where1: None,
            where2: None,
            problem: None,
            is_view: false,
            notes: vec![],
        };
        let table = |p: TablePair, counts: [u64; 4]| TableResult {
            pair: p,
            counts,
            offsets: Default::default(),
            path: Default::default(),
            error: None,
            by_checksum: false,
            repeated_keys: false,
            identity_current: (None, None),
            column_differences: vec![],
        };
        let result = DataComparison {
            session,
            tables: vec![table(pair("Orders", "Orders"), [3, 2, 1, 0]), table(pair("Old", "New"), [0, 0, 0, 4])],
            only_in_1: vec![],
            only_in_2: vec![],
            ddl_triggers: vec![],
        };
        let rules = Rules { include: vec![], exclude: vec![], kinds: [false, true, true, true], identical_allowed: false };
        let lines = summary(&result, &rules, "dev.Shop", "prod.Shop", 80);
        assert_eq!(lines[1], "Summary Information");
        assert_eq!(lines[2].len(), 79);
        assert_eq!(lines[6], "Object type     Name                                      Records      DB1 DB2 ");
        assert_eq!(lines[8], "Table           [dbo].[Orders]                                      2  <>  <>  ");
        assert_eq!(lines[9], "Table           [dbo].[Orders]                                      1  >>      ");
        assert_eq!(lines[10], "Table           [dbo].[Old]->[New]                                  4      <<  ");
        assert_eq!(lines.last().unwrap(), "Please note: Only included differences are displayed above and only included differences are deployed");
        assert_eq!(left_truncate("abcdefghijklmnop", 3, 10), "abc...mnop");
        assert_eq!(force_length("abcdef", 4), "abc ");
    }
}
