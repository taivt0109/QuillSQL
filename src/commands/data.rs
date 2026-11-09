//! `quill data`: compare the data of two databases and deploy the differences, with the
//! switches, console summary, CSV export and exit codes of the classic data compare command line.

use std::path::Path;

use regex::{Regex, RegexBuilder};

use super::common::{self, Source};
use super::console;
use super::enums;
use crate::args::{ArgError, Args, Rule, Switch, exit};
use crate::catalog::Database;
use crate::data::compare::{self, DataComparison, TableResult};
use crate::data::mapping::{DataMappings, TableOverride, TablePair};
use crate::data::store::{self, RowKind};
use crate::data::value::Value;
use crate::data::{DataOpt, DataOptions, sync};
use crate::db::ConnectionSpec;
use crate::schema::deploy;

/// the classic data compare command line's own exit codes.
const NOTHING_TO_COMPARE: u8 = 60;
const CANNOT_CREATE: u8 = 73;

const NOTHING_TO_COMPARE_TEXT: &str = "Nothing to compare.  Either there are no tables/views that can be compared, or everything has been filtered out with /include or /exclude. Use /Include:Identical to see tables/views that exist in both databases but could not be compared.";

const ACTIONS: [Switch; 12] = [
    Switch {
        name: "options",
        aliases: &["o"],
        takes_value: true,
        repeatable: true,
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

/// the classic data compare command line's own rules, after the shared ones.
const RULES: [Rule; 4] = [
    Rule { name: "options", values: enums::DATA_OPTIONS, list: true, ..Rule::NONE },
    Rule {
        name: "exportidenticaltables",
        depends: &["export"],
        deprecated: Some("/exportidenticaltables has been deprecated. Please use /include:identical instead."),
        ..Rule::NONE
    },
    Rule { name: "casesensitive", deprecated: Some("/casesensitive has been deprecated. Please use /options:CaseSensitiveObjectDefinition instead."), ..Rule::NONE },
    Rule { name: "sca2", excludes: &["synchronize"], ..Rule::NONE },
];

pub fn run(argv: &[String]) -> u8 {
    let switches = switches();
    let rules: Vec<Rule> = common::shared_rules().into_iter().chain(RULES).collect();
    let cli = common::Cli {
        command: "data",
        product: "QuillSQL Data Compare Command Line",
        summary: SUMMARY,
        switches: &switches,
        rules: &rules,
        require_one_of: &["auth", "database1", "backup1", "project", "scripts1", "sourcecontrol1"],
    };
    common::run(&cli, argv, execute)
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
    /// `start`: the row kinds before the switches (the project's, or the default).
    fn new(args: &Args, case_sensitive: bool, start: [bool; 4]) -> Result<Rules, ArgError> {
        let mut r = Rules { include: Vec::new(), exclude: Vec::new(), kinds: start, identical_allowed: false };
        let mut kinds_given = false;
        for (list, is_include) in [(args.all("include"), true), (args.all("exclude"), false)] {
            for spec in list {
                let (types, pattern) = match spec.split_once(':') {
                    Some((t, p)) => (t, Some(regex(p, case_sensitive)?)),
                    None => (spec.as_str(), None),
                };
                // `Table,View:regex`: one rule per type.
                for ty in types.split(',').map(str::trim).filter(|t| !t.is_empty()) {
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
                    if is_include { r.include.push((view, pattern.clone())) } else { r.exclude.push((view, pattern.clone())) }
                }
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

    /// An `/include` rule with a pattern names the pair.
    fn names(&self, p: &TablePair) -> bool {
        !self.exclude.iter().any(|r| Self::matches(r, p)) && self.include.iter().any(|r| r.1.is_some() && Self::matches(r, p))
    }
}

/// `/comparisonkeys`, `/includeColumns`, `/excludeColumns` applied to the mappings (the project's,
/// if any) of the pairs they match, tables and views alike: only the key or the columns change.
/// Returns, for each `/comparisonkeys` index, the table and index name to check.
fn overrides(args: &Args, db1: &Database, pairs: &[TablePair], options: &DataOptions, m: &mut DataMappings) -> Result<Vec<(crate::catalog::ObjectName, String)>, ArgError> {
    let case_sensitive = options.has(DataOpt::CaseSensitiveObjectDefinition);
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
    let entry = |m: &mut DataMappings, name: &crate::catalog::ObjectName| -> usize {
        if let Some(i) = m.tables.iter().position(|t| crate::data::mapping::same_object(&t.source, name, options)) {
            return i;
        }
        m.tables.push(TableOverride { source: name.clone(), ..Default::default() });
        m.tables.len() - 1
    };
    // The database 1 columns of a pair's source table or view.
    let columns = |n: &crate::catalog::ObjectName| -> Vec<String> {
        let same = |s: &str, x: &str| s.eq_ignore_ascii_case(x);
        db1.tables
            .iter()
            .find(|t| same(&t.schema, &n.schema) && same(&t.name, &n.name))
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
            .or_else(|| db1.views.iter().find(|v| same(&v.schema, &n.schema) && same(&v.name, &n.name)).map(|v| v.columns.iter().map(|c| c.name.clone()).collect()))
            .unwrap_or_default()
    };
    let matching = |re: &Regex| -> Vec<crate::catalog::ObjectName> { pairs.iter().filter(|p| re.is_match(&p.source.to_string())).map(|p| p.source.clone()).collect() };
    for spec in args.all("comparisonkeys") {
        let (re, key) = split(spec, "comparisonkeys")?;
        for t in matching(&re) {
            let i = entry(m, &t);
            if key.starts_with('(') && key.ends_with(')') {
                m.tables[i].key = key[1..key.len() - 1].split(',').map(|c| c.trim().trim_matches(['[', ']']).to_string()).collect();
            } else {
                let name = key.trim_matches(['[', ']']).to_string();
                m.tables[i].key = vec![format!("index:{name}")];
                keys.push((t.clone(), name));
            }
        }
    }
    for (switch, include) in [("includeColumns", true), ("excludeColumns", false)] {
        for spec in args.all(switch) {
            let (re, cols) = split(spec, switch)?;
            let col_res: Vec<Regex> = cols.split(',').map(|c| regex(c.trim(), case_sensitive)).collect::<Result<_, _>>()?;
            for t in matching(&re) {
                let i = entry(m, &t);
                for c in columns(&t) {
                    let hit = col_res.iter().any(|r| r.is_match(&c));
                    let excluded = m.tables[i].excluded_columns.iter().any(|x| x.eq_ignore_ascii_case(&c));
                    if include {
                        if hit && excluded {
                            m.tables[i].excluded_columns.retain(|x| !x.eq_ignore_ascii_case(&c));
                        } else if !hit && !excluded {
                            m.tables[i].excluded_columns.push(c.clone());
                        }
                    } else if hit && !excluded {
                        m.tables[i].excluded_columns.push(c.clone());
                    }
                }
            }
        }
    }
    Ok(keys)
}

fn live(source: Source, n: u8) -> Result<ConnectionSpec, ArgError> {
    match source {
        Source::Live(spec) => Ok(spec),
        _ => Err(ArgError::new(exit::USAGE, format!("data compare needs database {n} to be a live database"))),
    }
}

fn execute(args: &Args) -> Result<u8, ArgError> {
    // Dependency checks first, as the classic data compare command line does.
    if !["project", "database2", "connection2", "snapshot2", "backup2", "makescripts", "scripts2", "sourcecontrol2", "sca2", "empty2"].iter().any(|s| args.has(s)) {
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
    // Every /options occurrence counts, as one list.
    let option_list = args.has("options").then(|| args.all("options").join(","));
    let resolved = DataOptions::resolve(option_list.as_deref(), base, args.has("casesensitive"));
    for w in &resolved.warnings {
        console::warn(w);
    }
    let mut options = match &project {
        Some(p) => DataOptions::parse(&p.options.join(",")).map_err(|e| ArgError::new(exit::DATA_ERROR, e))?.union(&resolved.options),
        None => resolved.options,
    };
    let case_sensitive = options.has(DataOpt::CaseSensitiveObjectDefinition);
    let selection = project.as_ref().map(ProjectSelection::of).unwrap_or_default();
    let rules = Rules::new(args, case_sensitive, selection.row_kinds)?;
    if rules.kinds[RowKind::Identical as usize] {
        options = options.with(DataOpt::IncludeIdenticalRecords);
    }
    let parallel = match args.get("parallel") {
        Some(p) => p.parse::<usize>().map_err(|_| ArgError::new(exit::INVALID_VALUE, format!("/parallel: '{p}' is not a number")))?,
        None => 4,
    };
    let width = console::width();
    let max_mb = match args.get("MaxTransactionSizeMB") {
        Some(v) => v.parse::<u64>().map_err(|_| ArgError::new(exit::INVALID_VALUE, format!("Invalid value for switch /MaxTransactionSizeMB: {v}.")))?,
        None => 0,
    };
    let quiet = args.has("quiet");
    let verbose = args.has("verbose");
    let source1 = common::source(args, 1)?.ok_or_else(|| ArgError::new(exit::USAGE, "give database 1: /server1 and /database1"))?;
    let source2 = if args.has("makescripts") { None } else { common::source(args, 2)? };
    // The sides as a project would name them, for /outputProject.
    let project_sides = (common::spec_of(Some(&source1), args, 1), common::spec_of(source2.as_ref(), args, 2));
    // /empty2 (or a project's EmptyDatabaseSource): database 1's tables with no rows.
    let empty2 = args.has("empty2") || (source2.is_none() && project.as_ref().is_some_and(|p| p.target.is_empty_database()));
    // A scripts folder is read without a server, as the classic data compare tool reads it.
    let open = |s: Source, n: u8| -> Result<Opened, ArgError> {
        if let Source::Backup(b) = &s {
            return Ok(Opened::Backup(Box::new(super::backups::open(b)?)));
        }
        let Source::Scripts { path, revision } = s else { return Ok(Opened::Live(live(s, n)?)) };
        if verbose {
            eprintln!("Reading database scripts from '{}'...", path.display());
        }
        let f = crate::sources::datafolder::read(&path, revision.as_deref()).map_err(|e| ArgError::new(exit::NOTHING_TO_COMPARE, e))?;
        // What the folder's parser skipped and QuillDatabaseInfo.xml problems, as the classic compare tool prints them.
        for w in f.folder.warnings.iter().cloned().chain(f.folder.messages.iter().map(|m| m.text())) {
            console::warn(&w);
        }
        Ok(Opened::Folder(Box::new(f)))
    };
    let side1 = open(source1, 1)?;
    let read = |spec: &ConnectionSpec| {
        if verbose {
            eprintln!("Reading the schema of {}...", spec.describe());
        }
        crate::catalog::read_database_with(spec, false).map_err(|e| ArgError::new(exit::EXECUTION_FAILED, format!("{}: {e}", spec.describe())))
    };
    if let Some(dir) = args.get("makescripts") {
        let Opened::Live(spec1) = &side1 else { return Err(ArgError::new(exit::USAGE, "/makescripts needs database 1 to be a live database")) };
        let db1 = read(spec1)?;
        return make_scripts(spec1, &db1, &options, Path::new(dir), args);
    }
    let mut side2 = if empty2 {
        Opened::Empty(Box::default())
    } else {
        open(source2.ok_or_else(|| ArgError::new(exit::USAGE, "give database 2: /server2 and /database2"))?, 2)?
    };
    // A scripts folder is named by its folder, as the classic data compare tool does.
    let model = |side: &Opened| -> Result<(Database, String), ArgError> {
        Ok(match side {
            Opened::Live(spec) => {
                let db = read(spec)?;
                let label = describe(spec, &db);
                (db, label)
            }
            Opened::Folder(f) => (f.folder.database.clone(), f.folder.database.name.clone()),
            Opened::Backup(b) => {
                let db = b.read_database().map_err(super::backups::error)?;
                let label = db.name.clone();
                (db, label)
            }
            Opened::Empty(_) => (Database::default(), "Empty".to_string()),
        })
    };
    let (db1, label1) = model(&side1)?;
    let (db2, label2) = match model(&side2)? {
        (_, label) if empty2 => (db1.clone(), label),
        other => other,
    };
    // The project's mappings first; the switches then change only keys and columns.
    let mut user = project.as_ref().map(|p| p.data_mappings.clone()).unwrap_or_default();
    let (pairs, _, _) = crate::data::mapping::map_tables(&db1, &db2, &options, &user);
    let keys = overrides(args, &db1, &pairs, &options, &mut user)?;
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
    // Without a project, a scripts-folder table with no data script is compared only when an
    // /include pattern names it (else its target rows would all look extra).
    let no_data = |p: &TablePair| match &side1 {
        Opened::Folder(f) if project.is_none() && !p.is_view => !f.files.iter().any(|(n, _)| n.schema.eq_ignore_ascii_case(&p.source.schema) && n.name.eq_ignore_ascii_case(&p.source.name)),
        _ => false,
    };
    let wanted = |p: &TablePair| {
        rules.includes(p) && !deselected.contains(&p.source) && (!no_data(p) || rules.names(p)) && (selection.include_new_tables || selection.selected.contains(&p.source))
    };
    let result = compare::compare_sources(side1.rows(), side2.rows(), &db1, &db2, &options, &user, wanted, parallel, |p, r| {
        if verbose {
            eprintln!("Compared {} ({} rows differ)", p.source, r.count(RowKind::Different) + r.count(RowKind::OnlyIn1) + r.count(RowKind::OnlyIn2));
        }
    })
    .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, e))?;

    if !quiet {
        console::log("Finished");
        for line in summary(&result, &rules, &label1, &label2, width) {
            console::log(&line);
        }
        if verbose {
            console::log("OK");
        }
    }
    // the classic data compare command line prints no mapping or deployment warnings; they are shown with /verbose.
    if verbose {
        for p in result.tables.iter().map(|t| &t.pair).chain(&result.excluded) {
            for n in &p.notes {
                console::warn(&n.text);
            }
            if let Some(problem) = &p.problem {
                console::warn(&format!("{} is not compared: {problem}", p.source));
            }
        }
    }
    if let Some(dir) = args.get("export") {
        export(&result, &rules, Path::new(dir), &db1.name, &db2.name, [side1.spec(), side2.spec()]).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{dir}: {e}")))?;
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
        let target_database = side2.spec().map(|s| s.database.clone()).unwrap_or_else(|| db2.name.clone());
        let mut settings = sync::SyncSettings::new(&label1, &label2, &target_database);
        settings.transaction_isolation_level = args.get("transactionIsolationLevel").unwrap_or("SERIALIZABLE").to_ascii_uppercase();
        settings.kinds = rules.kinds;
        settings.max_transaction_bytes = (max_mb > 0).then_some(max_mb * 1_048_576);
        settings.source_connection = side1.spec().cloned();
        settings.row_messages = true;
        let mut s = sync::generate(&result, &db2, &options, &settings).map_err(|e| ArgError::new(exit::GENERAL_ERROR, e))?;
        let live2 = match &side2 {
            Opened::Live(s) => Some(s),
            _ => None,
        };
        common::add_backup(args, project.as_ref().map(|p| &p.backup), live2, &mut s)?;
        common::write_script(args, &s)?;
        if verbose {
            for w in &s.warnings {
                let object = if w.object.is_empty() { String::new() } else { format!("{}: ", w.object) };
                console::warn(&format!("({:?}) {object}{}", w.severity, w.message));
            }
        }
        common::check_warnings(args, &s)?;
        script = Some(s);
    }
    if assert && differs {
        return Err(ArgError::new(exit::NOT_IDENTICAL, "Exiting with code 79 as /assertidentical was specified"));
    }
    if let Some(out) = args.get("outputProject") {
        save_project(out, project.as_ref(), &project_sides, &options, &user, args)?;
    }
    if args.has("synchronize")
        && let Some(script) = &script
    {
        if script.is_empty() {
            if !quiet {
                console::log("Nothing to deploy.");
            }
        } else if let Opened::Folder(f) = &mut side2 {
            // A scripts folder read without a server: its data scripts are written again.
            let kinds = rules.kinds;
            let files = f.write_back(&result, &|_, k, _| kinds[k as usize]).map_err(|e| ArgError::new(exit::IO_ERROR, e))?;
            if !quiet {
                console::log(&format!("Deployed the data of {} to {label2}: {} data script(s) written.", db1.name, files.len()));
            }
        } else {
            let spec2 = side2.spec().cloned().ok_or_else(|| ArgError::new(exit::USAGE, "database 2 is not a database"))?;
            let ran = deploy::execute(&spec2, script, |i, n| {
                if verbose {
                    eprintln!("Running batch {} of {n}", i + 1);
                }
            })
            .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, e))?;
            if !quiet {
                console::log(&format!("Deployed the data of {} to {} ({ran} batches).", db1.name, label2));
            }
        }
    }
    if !failed.is_empty() {
        return Ok(exit::EXECUTION_FAILED);
    }
    Ok(exit::SUCCESS)
}

/// A data project's table selection and row kinds (`includeNewTables`, `selected`, `rowKinds`).
struct ProjectSelection {
    include_new_tables: bool,
    selected: Vec<crate::catalog::ObjectName>,
    row_kinds: [bool; 4],
}

impl Default for ProjectSelection {
    fn default() -> Self {
        ProjectSelection { include_new_tables: true, selected: Vec::new(), row_kinds: [false, true, true, true] }
    }
}

impl ProjectSelection {
    /// Read through the project's JSON form, whose fields default when absent.
    fn of(p: &crate::project::Project) -> ProjectSelection {
        let v = serde_json::to_value(p).unwrap_or_default();
        let mut s = ProjectSelection::default();
        if let Some(b) = v.get("includeNewTables").and_then(serde_json::Value::as_bool) {
            s.include_new_tables = b;
        }
        if let Some(list) = v.get("selected").and_then(serde_json::Value::as_array) {
            let text = |x: &serde_json::Value, k: &str| x.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
            s.selected = list.iter().map(|x| crate::catalog::ObjectName::new(&text(x, "schema"), &text(x, "name"))).collect();
        }
        if let Some(k) = v.get("rowKinds").and_then(|k| serde_json::from_value::<[bool; 4]>(k.clone()).ok()) {
            s.row_kinds = k;
        }
        if p.options.iter().any(|o| o.eq_ignore_ascii_case("IncludeIdenticalRecords")) {
            s.row_kinds[0] = true;
        }
        s
    }
}

/// One side of a data comparison.
enum Opened {
    Live(ConnectionSpec),
    /// A scripts folder read without a server.
    Folder(Box<crate::sources::datafolder::DataFolder>),
    /// A backup set, read without a server.
    Backup(Box<crate::sources::backup::Backup>),
    /// `/empty2`: no rows.
    Empty(Box<compare::Memory>),
}

impl Opened {
    fn spec(&self) -> Option<&ConnectionSpec> {
        match self {
            Opened::Live(s) => Some(s),
            Opened::Folder(_) | Opened::Backup(_) | Opened::Empty(_) => None,
        }
    }

    fn rows(&self) -> compare::RowSource<'_> {
        match self {
            Opened::Folder(f) => compare::RowSource::Memory(&f.memory),
            Opened::Empty(m) => compare::RowSource::Memory(m),
            Opened::Backup(b) => compare::RowSource::Backup(b),
            other => compare::RowSource::Live(other.spec().unwrap()),
        }
    }
}

/// `server.database`, as the summary and the script header name a database.
fn describe(spec: &ConnectionSpec, db: &Database) -> String {
    format!("{}.{}", spec.server, if db.name.is_empty() { &spec.database } else { &db.name })
}

/// `/outputProject`: the settings of this run as a QuillSQL project (`.qdc`).
fn save_project(
    path: &str,
    project: Option<&crate::project::Project>,
    sides: &(crate::project::SourceSpec, crate::project::SourceSpec),
    options: &DataOptions,
    user: &DataMappings,
    args: &Args,
) -> Result<(), ArgError> {
    if Path::new(path).exists() && !args.has("force") {
        return Err(ArgError::new(CANNOT_CREATE, format!("Project file [{path}] already exists. Use /force to overwrite it.")));
    }
    let mut p = project.cloned().unwrap_or_else(|| crate::project::Project::new(crate::project::ProjectKind::Data));
    // A side given on the command line replaces the project's.
    let switches = ["server", "database", "connection", "snapshot", "backup", "scripts", "sourcecontrol", "sca", "empty"];
    for (n, s) in [(1, &sides.0), (2, &sides.1)] {
        let given = switches.iter().any(|k| args.has(&format!("{k}{n}")));
        if given || project.is_none() {
            if n == 1 { p.source = s.clone() } else { p.target = s.clone() }
        }
    }
    p.options = options.iter().map(|o| o.name().to_string()).collect();
    p.data_mappings = user.clone();
    p.include = args.all("include").to_vec();
    p.exclude = args.all("exclude").to_vec();
    let failed = |e: String| ArgError::new(CANNOT_CREATE, format!("Error occurred writing project to file '{path}': {e}"));
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".scp") || lower.ends_with(".sdc") {
        return Err(failed("only QuillSQL projects (.qsc, .qdc) can be written".into()));
    }
    p.save(Path::new(path)).map_err(failed)
}

/// `/makescripts`: database 1 as a scripts folder (object scripts and QuillDatabaseInfo.xml)
/// with every table's rows as INSERT statements in `<dir>\Data`.
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
    crate::sources::folder::write_all(db, dir, &crate::schema::Options::default_set()).map_err(|e| ArgError::new(exit::IO_ERROR, e))?;
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
        console::log(&format!("Data scripts written to {}.", data.display()));
    }
    Ok(exit::SUCCESS)
}

/// The left `max` characters kept around `...`, as the classic data compare command line shortens long names.
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
/// Key columns come first (in key order), then the others, as the classic data compare tool's results store
/// holds them; values compared by hash are read again from the live database in `specs`.
fn export(result: &DataComparison, rules: &Rules, dir: &Path, db1: &str, db2: &str, specs: [Option<&ConnectionSpec>; 2]) -> std::io::Result<()> {
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
        let order: Vec<usize> = p.key.iter().copied().chain((0..p.columns.len()).filter(|i| !p.key.contains(i))).collect();
        let mut header = vec![csv("Type")];
        for &i in &order {
            let c = &p.columns[i];
            if p.key.contains(&i) {
                header.push(csv(&format!("{}_k", c.source)));
            } else {
                header.push(csv(&format!("{}_s", c.source)));
                header.push(csv(&format!("{}_t", c.target)));
            }
        }
        let mut out = format!("\u{feff}{}\r\n", header.join(","));
        for kind in kinds {
            for mut row in store::read_all(&t.path, &t.offsets[kind as usize])? {
                for (n, values) in [(1u8, &mut row.source), (2u8, &mut row.target)] {
                    for c in 0..values.len() {
                        if let (Value::Large(..), Some(spec)) = (&values[c], specs[n as usize - 1])
                            && let Ok(full) = compare::fetch_side_value(spec, p, n, c, values)
                        {
                            values[c] = full;
                        }
                    }
                }
                let label = match kind {
                    RowKind::Identical => "Same",
                    RowKind::Different => "Different",
                    RowKind::OnlyIn1 => "In1",
                    RowKind::OnlyIn2 => "In2",
                };
                let mut fields = vec![csv(label)];
                for &i in &order {
                    let c = &p.columns[i];
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

/// The same CSV files for the Compare window's Export: rows of the given kinds (`RowKind::ALL` order).
pub fn export_csv(result: &DataComparison, kinds: [bool; 4], dir: &Path, db1: &str, db2: &str, specs: [Option<&ConnectionSpec>; 2]) -> std::io::Result<()> {
    export(result, &Rules { include: Vec::new(), exclude: Vec::new(), kinds, identical_allowed: false }, dir, db1, db2, specs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_puts_key_columns_first() {
        use crate::data::store::{StoredRow, TableWriter};
        let session = crate::data::store::Session::create().unwrap();
        let db = Database {
            tables: vec![crate::catalog::Table {
                schema: "dbo".into(),
                name: "T".into(),
                columns: ["A", "K"].iter().map(|n| crate::catalog::Column { name: n.to_string(), data_type: crate::catalog::DataType { name: "int".into(), ..Default::default() }, ..Default::default() }).collect(),
                indexes: vec![crate::catalog::Index {
                    name: "PK".into(),
                    primary_key: true,
                    unique: true,
                    columns: vec![crate::catalog::IndexColumn { name: "K".into(), descending: false }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let (pairs, _, _) = crate::data::mapping::map_tables(&db, &db, &DataOptions::default_set(), &DataMappings::default());
        let path = session.table_path(0);
        let mut w = TableWriter::create(&path, false).unwrap();
        w.write(&StoredRow { kind: RowKind::OnlyIn1, source: vec![Value::Int(7), Value::Int(1)], target: vec![], differs: vec![] }).unwrap();
        let t = TableResult {
            pair: pairs.into_iter().next().unwrap(),
            counts: [0, 0, 1, 0],
            offsets: w.finish().unwrap(),
            path,
            error: None,
            by_checksum: false,
            repeated_keys: false,
            identity_current: (None, None),
            column_differences: vec![],
        };
        let dir = session.dir.join("csv");
        let result = DataComparison { session, tables: vec![t], only_in_1: vec![], only_in_2: vec![], ddl_triggers: vec![], excluded: vec![], kinds: Default::default(), masked_reads: false, old_server: false };
        export_csv(&result, [false, true, true, true], &dir, "a", "b", [None, None]).unwrap();
        let text = std::fs::read_to_string(dir.join("dbo.T.csv")).unwrap();
        assert_eq!(text, "\u{feff}\"Type\",\"K_k\",\"A_s\",\"A_t\"\r\n\"In1\",\"1\",\"7\",\"\"\r\n");
    }

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
            excluded: None,
            target_missing: false,
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
            excluded: vec![],
            kinds: Default::default(),
            masked_reads: false,
            old_server: false,
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
