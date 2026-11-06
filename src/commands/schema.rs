//! `quill schema`: compare two database schemas and deploy the differences, with the switches,
//! console output, reports and exit codes of SQL Compare's command line.

use std::path::Path;

use super::common::{self, Source};
use super::console;
use super::sources;
use super::enums;
use crate::args::{ArgError, Args, Rule, Switch, exit};
use crate::catalog::{Database, ObjectName};
use crate::schema::linediff;
use crate::schema::report::{self, Item, Report};
use crate::schema::select::Selection;
use crate::schema::{self, DiffKind, Difference, Mappings, Opt, Options, deploy};

const ACTIONS: [Switch; 12] = [
    Switch { name: "report", aliases: &["r"], takes_value: true, repeatable: false, help: "Write a comparison report to a file" },
    Switch { name: "reporttype", aliases: &["rt"], takes_value: true, repeatable: false, help: "Xml (default), Html, Classic or Excel (Simple and Interactive = Html)" },
    Switch {
        name: "reportAllObjectsWithDifferences",
        aliases: &["rad", "includeAllObjectsWithDifferences", "inca"],
        takes_value: false,
        repeatable: false,
        help: "Report every object that differs, not only the selected ones",
    },
    Switch { name: "includeIdentical", aliases: &["inci"], takes_value: false, repeatable: false, help: "Deprecated: use /include:Identical" },
    Switch {
        name: "makesnapshot",
        aliases: &["mksnap"],
        takes_value: true,
        repeatable: false,
        help: "Write a snapshot of database 1 to a file and stop",
    },
    Switch {
        name: "options",
        aliases: &["o"],
        takes_value: true,
        repeatable: true,
        help: "Comparison and deployment options, comma-separated (a list without Default starts from none)",
    },
    Switch {
        name: "datacompareoptions",
        aliases: &["dco"],
        takes_value: true,
        repeatable: true,
        help: "Options for deploying static data (accepted; static data is not supported yet)",
    },
    Switch {
        name: "include",
        aliases: &[],
        takes_value: true,
        repeatable: true,
        help: "type[:regex] to include; types: object types, Identical, Different, Missing, Additional",
    },
    Switch { name: "exclude", aliases: &[], takes_value: true, repeatable: true, help: "type[:regex] to exclude" },
    Switch { name: "filter", aliases: &["ftr"], takes_value: true, repeatable: false, help: "A saved filter file (.scpf) (not supported yet)" },
    Switch {
        name: "assertidentical",
        aliases: &[],
        takes_value: false,
        repeatable: false,
        help: "Exit 0 when the schemas are identical, 79 when they differ",
    },
    Switch {
        name: "json",
        aliases: &[],
        takes_value: true,
        repeatable: false,
        help: "Write the full comparison as JSON to a file (- for standard output)",
    },
];

const RULES: [Rule; 11] = [
    Rule { name: "assertidentical", excludes: &["synchronize", "makesnapshot", "makescripts", "outputProject"], ..Rule::NONE },
    Rule { name: "reportAllObjectsWithDifferences", depends: &["report"], ..Rule::NONE },
    Rule { name: "includeIdentical", deprecated: Some("/includeIdentical has been deprecated. Please use /include:Identical instead."), ..Rule::NONE },
    Rule {
        name: "makesnapshot",
        excludes: &["backup2", "database2", "server2", "username2", "password2", "report", "project", "snapshot1", "snapshot2", "scripts2", "include", "exclude", "makescripts"],
        path: true,
        ..Rule::NONE
    },
    Rule {
        name: "snapshot1",
        excludes: &["backup1", "database1", "revision1", "server1", "username1", "password1", "scripts1", "sourcecontrol1", "makesnapshot"],
        ..Rule::NONE
    },
    Rule {
        name: "snapshot2",
        excludes: &["backup2", "database2", "revision2", "server2", "username2", "password2", "scripts2", "sourcecontrol2", "makesnapshot", "makescripts", "synchronize"],
        ..Rule::NONE
    },
    Rule { name: "report", excludes: &["makesnapshot", "makescripts"], path: true, ..Rule::NONE },
    Rule { name: "reporttype", depends: &["report"], values: enums::REPORT_TYPES, ..Rule::NONE },
    Rule { name: "filter", excludes: &["exclude"], path: true, ..Rule::NONE },
    Rule { name: "options", values: enums::SCHEMA_OPTIONS, list: true, ..Rule::NONE },
    Rule { name: "datacompareoptions", values: enums::DATA_OPTIONS, list: true, ..Rule::NONE },
];

fn switches() -> Vec<Switch> {
    common::GENERAL.into_iter().chain(common::SOURCE1).chain(common::SOURCE2).chain(ACTIONS).chain(common::DEPLOY).collect()
}

const SUMMARY: &str = "Compares the schemas of two databases (live or snapshots) and deploys the differences.";

pub fn run(argv: &[String]) -> u8 {
    let switches = switches();
    let rules: Vec<Rule> = common::shared_rules().into_iter().chain(RULES).collect();
    let cli = common::Cli {
        command: "schema",
        product: "QuillSQL Compare Command Line",
        summary: SUMMARY,
        switches: &switches,
        rules: &rules,
        require_one_of: &["auth", "database1", "snapshot1", "scripts1", "project", "backup1", "sca1", "sourcecontrol1"],
    };
    common::run(&cli, argv, execute)
}

/// Switches that name database 2 (or make it unnecessary).
const DATABASE2: &[&str] = &["project", "database2", "backup2", "scripts2", "makescripts", "sourcecontrol2", "sca2", "snapshot2", "makesnapshot", "empty2", "connection2"];

/// `/include` other than `Identical` and `StaticData`.
fn nontrivial_include(args: &Args) -> bool {
    args.all("include").iter().any(|i| !(i.eq_ignore_ascii_case("identical") || i.eq_ignore_ascii_case("staticdata")))
}

/// Checks the object types of `/include` and `/exclude` against SQL Compare's list.
fn check_rule_types(args: &Args) -> Result<(), ArgError> {
    for name in ["include", "exclude"] {
        for spec in args.all(name) {
            let types = spec.split(':').next().unwrap_or_default();
            if let Some(bad) = types.split(',').map(str::trim).find(|t| !enums::SCHEMA_OBJECT_TYPES.iter().any(|n| n.eq_ignore_ascii_case(t))) {
                return Err(ArgError::new(exit::INVALID_VALUE, format!("Invalid value for switch /{name}: {bad}.")));
            }
        }
    }
    Ok(())
}

/// `/options`: SQL Compare's defaults plus the project's options; a list with `Default` adds to
/// them, a list without it replaces them. Names QuillSQL does not implement are ignored.
fn resolve_options(args: &Args, project: Option<&crate::project::Project>) -> Options {
    let mut base = Options::default_set();
    if let Some(p) = project {
        for name in &p.options {
            if let Ok(o) = Options::parse(name) {
                for f in o.iter() {
                    base.set(f, true);
                }
            }
        }
    }
    let names: Vec<&str> = args.all("options").iter().flat_map(|v| v.split(',')).map(str::trim).filter(|n| !n.is_empty()).collect();
    if names.is_empty() || (names.len() == 1 && names[0].eq_ignore_ascii_case("Default")) {
        return base;
    }
    let mut out = if names.iter().any(|n| n.eq_ignore_ascii_case("Default")) { base } else { Options::none() };
    for name in names.iter().filter(|n| !n.eq_ignore_ascii_case("Default") && !n.eq_ignore_ascii_case("None")) {
        match Options::parse(name) {
            Ok(o) => {
                for f in o.iter() {
                    out.set(f, true);
                }
                console::verbose(&format!("QuillSQL Compare running with option: {name} (OK)."));
            }
            Err(_) => console::verbose(&format!("Option {name} has no effect in QuillSQL.")),
        }
    }
    out
}

/// One side of the run.
enum Side {
    Source(Source),
    Empty,
}

fn execute(args: &Args) -> Result<u8, ArgError> {
    if !DATABASE2.iter().any(|a| args.has(a)) {
        let list: Vec<String> = DATABASE2[..10].iter().map(|a| format!("/{a}")).collect();
        return Err(ArgError::new(exit::USAGE, format!("You must specify one of the following argument(s): {}", list.join(", "))));
    }
    if args.has("project") || args.has("filter") {
        let by = if args.has("project") { "project" } else { "filter" };
        if args.has("exclude") {
            return Err(ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("The /{by} switch cannot be used in conjunction with the /exclude switch.")));
        }
        if nontrivial_include(args) {
            return Err(ArgError::new(
                exit::UNSATISFIED_ARGUMENT,
                "The /include switch cannot be specified together with the /project or /filter switches except for /include:identical and /include:staticdata",
            ));
        }
    }
    check_rule_types(args)?;
    let project = common::project_of(args, crate::project::ProjectKind::Schema)?;
    let source1 = common::source(args, 1)?.ok_or_else(|| ArgError::new(exit::USAGE, "The /database1 switch requires a database name."))?;
    let options = resolve_options(args, project.as_ref());
    options.validate().map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?;

    if let Some(path) = args.get("makesnapshot") {
        if Path::new(path).exists() && !args.has("force") {
            return Err(ArgError::new(exit::IO_ERROR, format!("Unable to write snapshot to file '{path}' because the file already exists.")));
        }
        let db = common::load(&source1, args, options.has(crate::schema::Opt::DecryptPost2kEncryptedObjects))?;
        console::verbose(&format!("Creating database snapshot file '{path}' from database '{}'...", db.name));
        common::save_snapshot(&db, Path::new(path))?;
        console::log(&format!(
            "Snapshot of {} written to {path}: {} tables, {} views, {} routines.",
            db.name,
            db.tables.len(),
            db.views.len(),
            db.routines.len()
        ));
        return Ok(exit::SUCCESS);
    }
    if let Some(dir) = args.get("makescripts") {
        let (db, _) = load(&source1, args, &options)?;
        return sources::make_scripts(args, &db, Path::new(dir), &options, &describe(Some(&source1), &db));
    }

    let db2_given = ["snapshot2", "connection2", "database2", "server2"].iter().any(|k| args.has(k));
    let side2 = if args.has("empty2") || (!db2_given && project.as_ref().is_some_and(|p| p.target.is_empty_database())) {
        Side::Empty
    } else {
        match common::source(args, 2)? {
            Some(s) => Side::Source(s),
            None if args.has("synchronize") && !args.has("project") => {
                return Err(ArgError::new(exit::USAGE, "You must specify both databases to use the /SYNC switch"));
            }
            None => return Err(ArgError::new(exit::USAGE, "You must specify two valid data sources in order to synchronize.")),
        }
    };
    let (includes, excludes) = match &project {
        Some(p) if !nontrivial_include(args) => {
            (p.include.iter().chain(args.all("include")).cloned().collect::<Vec<_>>(), p.exclude.clone())
        }
        _ => (args.all("include").to_vec(), args.all("exclude").to_vec()),
    };
    let selection = Selection::new(&includes, &excludes).map_err(|e| ArgError::new(exit::USAGE, e))?;
    let include_identical = selection.include_identical || args.has("allowIdenticalDatabases") || args.has("includeIdentical");
    let mappings = project.as_ref().map(|p| p.schema_mappings.clone()).unwrap_or_default();
    let filter = sources::filter(args, project.as_ref().and_then(|p| p.redgate_filter.as_deref()))?;
    if let Some(path) = args.get("json").filter(|p| *p != "-") {
        common::check_output(Path::new(path), args)?;
    }

    let (db1, folder1) = load(&source1, args, &options)?;
    let (db2, folder2) = match &side2 {
        Side::Source(s) => load(s, args, &options)?,
        Side::Empty => (Database { name: String::new(), server: db1.server.clone(), collation: db1.collation.clone(), ..Database::default() }, None),
    };
    // SSDT folders change how the comparison is made, not how scripts and files are written.
    let compare_options = sources::ssdt_options(options.clone(), &[folder1.as_ref(), folder2.as_ref()]);
    console::verbose(&format!("Comparing database {} with database {}...", identifier(&source1, &db1), side_identifier(&side2, &db2)));
    let mut comparison = schema::compare(&db1, &db2, &compare_options, &mappings);
    if let Some(p) = &project {
        for d in &mut comparison.differences {
            if p.deselected.iter().any(|x| x.schema == d.name.schema && x.name == d.name.name && (x.kind.is_empty() || x.kind == d.object_type.label())) {
                d.selected = false;
            }
        }
    }
    // A filter selects in place of /include and /exclude.
    match &filter {
        Some(f) => sources::apply_filter(f, &mut comparison),
        None => {
            selection.apply(&mut comparison);
        }
    }
    let passes = |d: &Difference| filter.as_ref().is_none_or(|f| f.includes(d));
    for d in &mut comparison.differences {
        if d.kind == DiffKind::Equal {
            d.selected = false;
        }
    }
    let identical = !comparison.differences.iter().any(|d| d.selected && d.kind != DiffKind::Equal);

    if let Some(path) = args.get("report") {
        write_report(args, path, &comparison, &db1, &db2, &source1, &side2, &options, include_identical)?;
    }

    if let Some(path) = args.get("json") {
        let json = serde_json::to_string_pretty(&comparison).map_err(|e| ArgError::new(exit::GENERAL_ERROR, e.to_string()))?;
        if path == "-" {
            println!("{json}");
        } else {
            std::fs::write(path, json).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{path}: {e}")))?;
        }
    } else {
        let listed: Vec<&Difference> = comparison
            .differences
            .iter()
            .filter(|d| if d.kind == DiffKind::Equal { include_identical && selection.includes(d) && passes(d) } else { d.selected })
            .collect();
        let (id1, id2) = (identifier(&source1, &db1), side_identifier(&side2, &db2));
        print_summary(&listed, &id1, &id2);
        if console::is_verbose() {
            print_details(&listed, &id1, &id2, &db1, &db2);
        }
        for (table, from, to) in &comparison.auto_mapped_columns {
            console::verbose(&format!("Column {table}.[{from}] was mapped to [{to}] because they are alike (turn off with /options:...,NoAutoColumnMapping)."));
        }
    }

    if let Some(path) = args.get("outputProject") {
        write_project(args, path, project.as_ref(), &source1, &side2, &options, &includes, &excludes)?;
    }

    let assert = args.has("assertidentical");
    if identical && !include_identical && !assert {
        return Err(ArgError::new(
            exit::IDENTICAL,
            "The selected objects are identical or no objects have been selected in the comparison. Use /include:Identical to suppress this error.",
        ));
    }

    let live_target = match &side2 {
        Side::Source(Source::Live(spec)) => Some(spec),
        _ => None,
    };
    let mut script = None;
    if args.has("scriptfile") || (args.has("synchronize") && live_target.is_some()) {
        let mut s = build_script(args, &comparison, &db1, &db2, &options, &mappings, &source1, &side2)?;
        if let Some(f) = &folder1 {
            sources::add_pre_post(&mut s, f, &options)?;
        }
        common::write_script(args, &s)?;
        script = Some(s);
    }
    if let Some(s) = &script {
        common::report_warnings(args, s);
        common::check_warnings(args, s)?;
    }
    if assert && !identical {
        return Ok(exit::NOT_IDENTICAL);
    }
    // Deploying to a scripts folder rewrites its files; ForceSyncScriptGeneration keeps it to the script.
    if let (true, Some(f)) = (args.has("synchronize") && !options.has(crate::schema::Opt::ForceSyncScriptGeneration), &folder2) {
        if matches!(side2, Side::Source(Source::Scripts { revision: Some(_), .. })) {
            return Err(ArgError::new(exit::USAGE, "/synchronize cannot write to a source control revision; give the scripts folder with /scripts2"));
        }
        sources::sync_folder(&db1, f, &comparison.differences, &options)?;
        return Ok(exit::SUCCESS);
    }
    if let (true, Some(target), Some(script)) = (args.has("synchronize"), live_target, &script) {
        if script.is_empty() {
            console::verbose("Nothing to deploy.");
            return Ok(exit::SUCCESS);
        }
        common::make_backup(args, target)?;
        console::verbose("Synchronizing databases...");
        let ran = deploy::execute(target, script, |i, n| console::verbose(&format!("Running batch {} of {n}", i + 1)))
            .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, format!("Synchronization failed due to database error: {e}")))?;
        console::verbose(&format!("Deployed {} to {} ({ran} batches).", db1.name, target.describe()));
    }
    Ok(exit::SUCCESS)
}

/// `server.database`, as SQL Compare names a data source in its output.
fn identifier(source: &Source, db: &Database) -> String {
    match source {
        Source::Live(spec) => format!("{}.{}", spec.server, db.name),
        Source::Snapshot(_) | Source::Scripts { .. } => db.name.clone(),
    }
}

fn side_identifier(side: &Side, db: &Database) -> String {
    match side {
        Side::Source(s) => identifier(s, db),
        Side::Empty => "Empty".into(),
    }
}

fn full_name(n: &ObjectName) -> String {
    if n.schema.is_empty() { format!("[{}]", n.name.replace(']', "]]")) } else { format!("[{}].[{}]", n.schema.replace(']', "]]"), n.name.replace(']', "]]")) }
}

/// Text cut to `len - 1` characters when it does not fit, then padded to `len`.
fn force_length(s: &str, len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let cut: String = if chars.len() >= len { chars[..len.saturating_sub(1)].iter().collect() } else { s.to_string() };
    format!("{cut:<len$}")
}

/// Keeps the start and the end of a long name: `[dbo].[Very...Name]`.
fn left_truncate(s: &str, prefix: usize, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head: String = chars[..prefix].iter().collect();
    let tail: String = chars[chars.len() - (max - prefix - 3)..].iter().collect();
    format!("{head}...{tail}")
}

/// One summary row (two lines when the database 2 name does not fit).
fn summary_row(d: &Difference, name_width: usize) -> String {
    const TYPE_WIDTH: usize = 24;
    let name = full_name(&d.name);
    let mut mapped = String::new();
    if let (Some(t), DiffKind::Different | DiffKind::Equal) = (&d.target_name, d.kind) {
        let mut owner_changed = false;
        if t.schema != d.name.schema && !t.schema.is_empty() {
            owner_changed = true;
            mapped.push_str(&format!("->[{}]", t.schema));
        }
        if t.name != d.name.name {
            mapped.push_str(if owner_changed { "." } else { "->" });
            mapped.push_str(&format!("[{}]", t.name));
        }
    }
    let (text, extra) = if name.chars().count() + mapped.chars().count() > name_width - 1 {
        (name, format!("\n{}{}", " ".repeat(TYPE_WIDTH), force_length(&mapped, name_width)))
    } else {
        (format!("{name}{mapped}"), String::new())
    };
    let room = name_width - 1;
    let text = force_length(&left_truncate(&text, 3.max((room as f32 * 0.3) as usize), room), name_width);
    let status = match d.kind {
        DiffKind::Different => "<>  <>  ",
        DiffKind::Equal => "==  ==  ",
        DiffKind::OnlyIn1 => ">>      ",
        DiffKind::OnlyIn2 => "    <<  ",
    };
    let extra = if mapped.is_empty() { String::new() } else { extra };
    format!("{}{text}{status}{extra}", force_length(report::type_name(d.object_type), TYPE_WIDTH))
}

fn print_summary(listed: &[&Difference], id1: &str, id2: &str) {
    let w = console::width();
    let name_width = w - 24 - 4 - 4 - 1;
    console::log("");
    console::log("Summary Information");
    console::log(&"=".repeat(w - 1));
    console::log(&format!("DB1 = {id1}"));
    console::log(&format!("DB2 = {id2}"));
    console::log("");
    console::log(&format!("{:<24}{:<name_width$}{:<4}{:<4}", "Object type", "Name", "DB1", "DB2"));
    console::log(&"-".repeat(w - 1));
    for d in listed {
        console::log(&summary_row(d, name_width));
    }
    console::log(&"-".repeat(w - 1));
    console::log("Please note: Only included differences are displayed above and only included differences are deployed");
}

/// The script lines of one difference, aligned.
fn script_lines(d: &Difference, db1: &Database, db2: &Database, ignore_whitespace: bool) -> Vec<linediff::Line> {
    let left = (d.kind != DiffKind::OnlyIn2).then(|| crate::compare_serve::object_sql(db1, d.object_type, &d.name)).flatten();
    let right = (d.kind != DiffKind::OnlyIn1).then(|| crate::compare_serve::object_sql(db2, d.object_type, d.target_name.as_ref().unwrap_or(&d.name))).flatten();
    linediff::compare(left.as_deref(), right.as_deref(), ignore_whitespace)
}

/// `/verbose`: each object's script, line against line.
fn print_details(listed: &[&Difference], id1: &str, id2: &str, db1: &Database, db2: &Database) {
    let w = console::width();
    console::verbose("");
    console::verbose(&console::fixed_width(&format!("Details - comparing {id1} with {id2}"), w - 1, "", true));
    console::verbose(&"=".repeat(w - 1));
    for d in listed.iter().filter(|d| d.kind != DiffKind::Equal) {
        console::verbose(&format!("{} {}", report::type_name(d.object_type), full_name(&d.name)));
        for l in script_lines(d, db1, db2, false) {
            match (&l.left, &l.right, l.different) {
                (Some(a), _, false) => console::verbose(&format!("== {a}")),
                (Some(a), Some(b), true) => {
                    console::verbose(&format!(">> {a}"));
                    console::verbose(&format!("<< {b}"));
                }
                (Some(a), None, _) => console::verbose(&console::fixed_width(a, w, ">> ", false)),
                (None, Some(b), _) => console::verbose(&console::fixed_width(b, w, "<< ", false)),
                (None, None, _) => {}
            }
        }
        console::verbose(&"-".repeat(w - 1));
    }
    console::verbose("");
}

fn report_source(side: Option<&Source>, db: &Database) -> report::DataSource {
    match side {
        Some(Source::Live(spec)) => report::DataSource { kind: "live", server: spec.server.clone(), database: db.name.clone(), created: None },
        Some(Source::Snapshot(path)) => report::DataSource {
            kind: "snapshot",
            server: String::new(),
            database: db.name.clone(),
            created: std::fs::metadata(path).and_then(|m| m.modified()).ok().map(|t| console::utc(t).report_format()),
        },
        Some(Source::Scripts { revision, .. }) => report::DataSource {
            kind: if revision.is_some() { "source control" } else { "folder" },
            server: String::new(),
            database: db.name.clone(),
            created: None,
        },
        None => report::DataSource { kind: "", server: String::new(), database: String::new(), created: None },
    }
}

#[allow(clippy::too_many_arguments)]
fn write_report(
    args: &Args,
    path: &str,
    comparison: &schema::Comparison,
    db1: &Database,
    db2: &Database,
    source1: &Source,
    side2: &Side,
    options: &Options,
    include_identical: bool,
) -> Result<(), ArgError> {
    if Path::new(path).exists() && !args.has("force") {
        return Err(ArgError::new(exit::IO_ERROR, format!("Unable to write report to file '{path}' because the file already exists.")));
    }
    console::verbose(&format!("Writing report to file '{path}'..."));
    let all = args.has("reportAllObjectsWithDifferences");
    let ignore_ws = options.has(Opt::IgnoreWhiteSpace);
    let mut items: Vec<Item> = comparison
        .differences
        .iter()
        .filter(|d| report::wanted(d, all, include_identical))
        .map(|d| {
            let target = d.target_name.as_ref().unwrap_or(&d.name);
            let (one, two) = match d.kind {
                DiffKind::OnlyIn1 => (Some(&d.name), None),
                DiffKind::OnlyIn2 => (None, Some(&d.name)),
                _ => (Some(&d.name), Some(target)),
            };
            Item {
                object_type: d.object_type,
                kind: d.kind,
                full_name: full_name(&d.name),
                owner1: one.map(|n| n.schema.clone()).unwrap_or_default(),
                name1: one.map(|n| n.name.clone()).unwrap_or_default(),
                owner2: two.map(|n| n.schema.clone()).unwrap_or_default(),
                name2: two.map(|n| n.name.clone()).unwrap_or_default(),
                lines: script_lines(d, db1, db2, ignore_ws),
            }
        })
        .collect();
    report::order(&mut items);
    let side2 = match side2 {
        Side::Source(s) => Some(s),
        Side::Empty => None,
    };
    let r = Report {
        timestamp: console::now().report_format(),
        source: report_source(Some(source1), db1),
        target: report_source(side2, db2),
        items,
        include_identical,
    };
    let bytes = match args.get("reporttype").unwrap_or("Xml") {
        "Html" | "Simple" | "Interactive" => report::html(&r).into_bytes(),
        "Classic" => report::classic(&r).into_bytes(),
        "Excel" if path.to_ascii_lowercase().ends_with(".xlsx") => report::xlsx(&r),
        "Excel" => report::excel(&r).into_bytes(),
        _ => report::xml(&r),
    };
    std::fs::write(path, bytes).map_err(|e| ArgError::new(exit::REPORT_FAILED, format!("Failed to create report: {e}")))
}

fn spec_of(side: Option<&Source>) -> crate::project::SourceSpec {
    use crate::db::Auth;
    use crate::project::{Authentication, SourceSpec};
    match side {
        Some(Source::Live(c)) => {
            let (authentication, user) = match &c.auth {
                Auth::Sql { user, .. } => (Authentication::Sql, user.clone()),
                Auth::AccessToken(_) => (Authentication::Entra, String::new()),
                _ => (Authentication::Windows, String::new()),
            };
            SourceSpec {
                authentication,
                user,
                encrypt: c.encrypt,
                trust_server_certificate: c.trust_server_certificate,
                ..SourceSpec::database(&c.server, &c.database)
            }
        }
        Some(Source::Snapshot(p)) => SourceSpec { kind: "snapshot".into(), path: p.display().to_string(), ..Default::default() },
        Some(Source::Scripts { path, .. }) => SourceSpec { kind: "scripts".into(), path: path.display().to_string(), ..Default::default() },
        None => SourceSpec { kind: "empty".into(), ..Default::default() },
    }
}

/// `/outputProject`: the settings of this run, as a QuillSQL project (`.qsc`) or a SQL Compare
/// project (any other name).
#[allow(clippy::too_many_arguments)]
fn write_project(
    args: &Args,
    path: &str,
    project: Option<&crate::project::Project>,
    source1: &Source,
    side2: &Side,
    options: &Options,
    includes: &[String],
    excludes: &[String],
) -> Result<(), ArgError> {
    if Path::new(path).exists() && !args.has("force") {
        return Err(ArgError::new(exit::REPORT_FAILED, format!("Project file [{path}] already exists. Use /force to overwrite it.")));
    }
    console::verbose(&format!("Writing new project to file '{path}'..."));
    let mut p = project.cloned().unwrap_or_else(|| crate::project::Project::new(crate::project::ProjectKind::Schema));
    p.source = spec_of(Some(source1));
    p.target = spec_of(match side2 {
        Side::Source(s) => Some(s),
        Side::Empty => None,
    });
    p.options = options.iter().map(|o| o.name().to_string()).collect();
    p.include = includes.to_vec();
    p.exclude = excludes.to_vec();
    let failed = |e: String| ArgError::new(exit::REPORT_FAILED, format!("Error occurred writing project to file '{path}': {e}"));
    if path.to_ascii_lowercase().ends_with(".qsc") {
        p.save(Path::new(path)).map_err(failed)
    } else {
        std::fs::write(path, crate::project::export_redgate(&p, &console::now().invariant_format())).map_err(|e| failed(e.to_string()))
    }
}

/// Reads one side; a scripts folder also comes back as itself, for deploying to it.
fn load(source: &Source, args: &Args, options: &Options) -> Result<(Database, Option<crate::sources::folder::Folder>), ArgError> {
    match source {
        Source::Scripts { path, revision } => {
            let (db, folder) = sources::load(path, revision.as_deref(), args, options)?;
            Ok((db, Some(folder)))
        }
        _ => Ok((common::load(source, args, options.has(crate::schema::Opt::DecryptPost2kEncryptedObjects))?, None)),
    }
}

fn describe(side: Option<&Source>, db: &Database) -> String {
    match side {
        Some(Source::Live(spec)) => format!("{}.{}", spec.server, db.name),
        Some(Source::Snapshot(path)) => format!("{} (snapshot {})", db.name, path.display()),
        Some(Source::Scripts { path, .. }) => format!("{} (scripts folder {})", db.name, path.display()),
        None => "Empty".into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_script(
    args: &Args,
    comparison: &schema::Comparison,
    db1: &Database,
    db2: &Database,
    options: &Options,
    mappings: &Mappings,
    source1: &Source,
    side2: &Side,
) -> Result<deploy::DeploymentScript, ArgError> {
    let side2 = match side2 {
        Side::Source(s) => Some(s),
        Side::Empty => None,
    };
    let settings = deploy::ScriptSettings {
        transaction_isolation_level: args.get("transactionIsolationLevel").unwrap_or("SERIALIZABLE").to_ascii_uppercase(),
        source: describe(Some(source1), db1),
        target: describe(side2, db2),
        target_database: db2.name.clone(),
        target_major_version: if db2.server.is_azure_sql_database() { 99 } else { db2.server.major_version },
    };
    deploy::generate(comparison, db1, db2, options, mappings, &settings).map_err(|e| ArgError::new(exit::INVALID_VALUE, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ObjectType;

    fn diff(kind: DiffKind, name: &str, target: Option<ObjectName>) -> Difference {
        Difference {
            object_type: ObjectType::StoredProcedure,
            name: ObjectName::new("dbo", name),
            target_name: target,
            kind,
            properties: vec![],
            selected: true,
            trivial: false,
        }
    }

    #[test]
    fn summary_rows_use_sql_compare_columns() {
        let row = summary_row(&diff(DiffKind::Different, "Orders", None), 47);
        assert_eq!(row, format!("{:<24}{:<47}<>  <>  ", "StoredProcedure", "[dbo].[Orders]"));
        assert!(summary_row(&diff(DiffKind::OnlyIn1, "A", None), 47).ends_with(">>      "));
        assert!(summary_row(&diff(DiffKind::OnlyIn2, "A", None), 47).ends_with("    <<  "));
        let mapped = summary_row(&diff(DiffKind::Different, "Old", Some(ObjectName::new("sales", "New"))), 47);
        assert!(mapped.contains("[dbo].[Old]->[sales].[New]"), "{mapped}");
        let long = summary_row(&diff(DiffKind::Different, &"x".repeat(80), None), 47);
        assert!(long.contains("...") && long.chars().count() == 24 + 47 + 8, "{long}");
    }

    #[test]
    fn options_merge_like_sql_compare() {
        let mut args = Args::default();
        let mut p = crate::project::Project::new(crate::project::ProjectKind::Schema);
        p.options = vec!["IgnoreComments".into()];
        assert!(resolve_options(&args, Some(&p)).has(Opt::IgnoreComments));
        args.set("options", vec!["Default,IgnoreCollations".into()]);
        let o = resolve_options(&args, Some(&p));
        assert!(o.has(Opt::IgnoreComments) && o.has(Opt::IgnoreCollations) && o.has(Opt::IgnoreWhiteSpace));
        args.set("options", vec!["IgnoreCollations".into(), "UseCompatibilityLevel".into()]);
        let o = resolve_options(&args, Some(&p));
        assert!(!o.has(Opt::IgnoreComments) && !o.has(Opt::IgnoreWhiteSpace) && o.has(Opt::IgnoreCollations));
        args.set("options", vec!["None".into()]);
        assert_eq!(resolve_options(&args, None).iter().count(), 0);
    }
}
