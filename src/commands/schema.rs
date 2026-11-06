//! `quill schema`: compare two database schemas and deploy the differences.

use std::path::Path;

use super::common::{self, Source};
use crate::args::{self, ArgError, Args, Switch, exit};
use crate::catalog::Database;
use crate::schema::select::Selection;
use crate::schema::{self, DiffKind, Difference, Mappings, Options, deploy, print};

const ACTIONS: [Switch; 9] = [
    Switch { name: "report", aliases: &["r"], takes_value: true, repeatable: false, help: "Write a comparison report to a file" },
    Switch { name: "reporttype", aliases: &["rt"], takes_value: true, repeatable: false, help: "Xml (default) or Html" },
    Switch {
        name: "reportAllObjectsWithDifferences",
        aliases: &["rad"],
        takes_value: false,
        repeatable: false,
        help: "Report identical objects too",
    },
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
        repeatable: false,
        help: "Comparison and deployment options, comma-separated (list without Default starts from none)",
    },
    Switch {
        name: "include",
        aliases: &[],
        takes_value: true,
        repeatable: true,
        help: "type[:regex] to include; types: object types, All, Identical, Different, Missing, Additional",
    },
    Switch { name: "exclude", aliases: &[], takes_value: true, repeatable: true, help: "type[:regex] to exclude" },
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

fn switches() -> Vec<Switch> {
    common::GENERAL.into_iter().chain(common::SOURCE1).chain(common::SOURCE2).chain(ACTIONS).chain(common::DEPLOY).collect()
}

const SUMMARY: &str = "Compares the schemas of two databases (live or snapshots) and deploys the differences.";

pub fn run(argv: &[String]) -> u8 {
    let switches = switches();
    let args = match args::parse(&switches, argv) {
        Ok(a) => a,
        Err(e) => return common::fail(e),
    };
    if args.has("help") || argv.is_empty() {
        print!("{}", args::usage("schema", SUMMARY, &switches));
        return exit::SUCCESS;
    }
    match execute(&args) {
        Ok(code) => code,
        Err(e) => common::fail(e),
    }
}

fn execute(args: &Args) -> Result<u8, ArgError> {
    let project = common::project_of(args, crate::project::ProjectKind::Schema)?;
    let source1 = common::source(args, 1)?
        .ok_or_else(|| ArgError::new(exit::USAGE, "give database 1: /database1 (with /server1) or /snapshot1"))?;
    if let Some(path) = args.get("makesnapshot") {
        let path = Path::new(path);
        common::check_output(path, args)?;
        let db = common::load(&source1, args)?;
        common::save_snapshot(&db, path)?;
        if !args.has("quiet") {
            println!(
                "Snapshot of {} written to {}: {} tables, {} views, {} routines.",
                db.name,
                path.display(),
                db.tables.len(),
                db.views.len(),
                db.routines.len()
            );
        }
        return Ok(exit::SUCCESS);
    }
    let source2 = common::source(args, 2)?.ok_or_else(|| {
        ArgError::new(exit::USAGE, "give database 2: /database2 (with /server2) or /snapshot2, or use /makesnapshot")
    })?;
    let options = match (args.get("options"), &project) {
        (Some(list), _) => Options::parse(list).map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?,
        (None, Some(p)) => Options::parse(&p.options.join(",")).map_err(|e| ArgError::new(exit::DATA_ERROR, e))?,
        (None, None) => Options::default_set(),
    };
    options.validate().map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?;
    let (includes, excludes) = match &project {
        Some(p) if !args.has("include") && !args.has("exclude") => (p.include.clone(), p.exclude.clone()),
        _ => (args.all("include").to_vec(), args.all("exclude").to_vec()),
    };
    let selection = Selection::new(&includes, &excludes).map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?;
    let mappings = project.as_ref().map(|p| p.schema_mappings.clone()).unwrap_or_default();
    if let Some(path) = args.get("json").filter(|p| *p != "-") {
        common::check_output(Path::new(path), args)?;
    }

    let db1 = common::load(&source1, args)?;
    let db2 = common::load(&source2, args)?;
    let mut comparison = schema::compare(&db1, &db2, &options, &mappings);
    if let Some(p) = &project {
        for d in &mut comparison.differences {
            if p.deselected.iter().any(|x| x.schema == d.name.schema && x.name == d.name.name && (x.kind.is_empty() || x.kind == d.object_type.label())) {
                d.selected = false;
            }
        }
    }
    let identical = comparison.identical();
    let listed: Vec<Difference> = selection.apply(&mut comparison).into_iter().cloned().collect();
    let quiet = args.has("quiet");

    if let Some(path) = args.get("json") {
        let json = serde_json::to_string_pretty(&comparison).map_err(|e| ArgError::new(exit::GENERAL_ERROR, e.to_string()))?;
        if path == "-" {
            println!("{json}");
        } else {
            std::fs::write(path, json).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{path}: {e}")))?;
        }
    } else if !quiet {
        let refs: Vec<&Difference> = listed.iter().collect();
        print!("{}", print::summary(&refs, &db1.name, &db2.name));
        if args.has("verbose") {
            for d in listed.iter().filter(|d| d.kind == DiffKind::Different) {
                println!();
                print!("{}", print::details(d));
            }
        }
        for (table, from, to) in &comparison.auto_mapped_columns {
            println!("Column {table}.[{from}] was mapped to [{to}] because they are alike (turn off with /options:...,NoAutoColumnMapping).");
        }
    }

    if let Some(path) = args.get("report") {
        let path = Path::new(path);
        common::check_output(path, args)?;
        let all = args.has("reportAllObjectsWithDifferences") || selection.include_identical;
        let text = match args.get("reporttype").unwrap_or("xml").to_ascii_lowercase().as_str() {
            "xml" => schema::report::xml(&comparison, &db1.name, &db2.name, all),
            "html" | "interactive" | "simple" => schema::report::html(&comparison, &db1.name, &db2.name, all),
            other => return Err(ArgError::new(exit::INVALID_VALUE, format!("/reporttype: '{other}' is not Xml or Html"))),
        };
        std::fs::write(path, text).map_err(|e| ArgError::new(exit::REPORT_FAILED, format!("{}: {e}", path.display())))?;
        if !quiet {
            println!("Report written to {}.", path.display());
        }
    }

    if args.has("assertidentical") {
        if identical {
            if !quiet {
                println!("The schemas are identical.");
            }
            return Ok(exit::SUCCESS);
        }
        if !quiet {
            println!("The schemas are not identical.");
        }
        return Ok(exit::NOT_IDENTICAL);
    }
    if identical && !selection.include_identical {
        if !quiet {
            println!("The schemas are identical.");
        }
        return Ok(exit::IDENTICAL);
    }

    if !args.has("scriptfile") && !args.has("synchronize") {
        return Ok(exit::SUCCESS);
    }
    if args.has("synchronize") && !matches!(source2, Source::Live(_)) {
        return Err(ArgError::new(exit::USAGE, "/synchronize needs database 2 to be a live database, not a snapshot"));
    }
    let script = build_script(args, &comparison, &db1, &db2, &options, &mappings, &source1, &source2)?;
    common::write_script(args, &script)?;
    common::report_warnings(args, &script);
    common::check_warnings(args, &script)?;
    if let (true, Source::Live(target)) = (args.has("synchronize"), &source2) {
        if script.is_empty() {
            if !quiet {
                println!("Nothing to deploy.");
            }
            return Ok(exit::SUCCESS);
        }
        let verbose = args.has("verbose");
        let ran = deploy::execute(target, &script, |i, n| {
            if verbose {
                eprintln!("Running batch {} of {n}", i + 1);
            }
        })
        .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, e))?;
        if !quiet {
            println!("Deployed {} to {} ({ran} batches).", db1.name, target.describe());
        }
    }
    Ok(exit::SUCCESS)
}

fn describe(source: &Source, db: &Database) -> String {
    match source {
        Source::Live(spec) => format!("{}.{}", spec.server, db.name),
        Source::Snapshot(path) => format!("{} (snapshot {})", db.name, path.display()),
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
    source2: &Source,
) -> Result<deploy::DeploymentScript, ArgError> {
    let settings = deploy::ScriptSettings {
        transaction_isolation_level: args.get("transactionIsolationLevel").unwrap_or("SERIALIZABLE").to_ascii_uppercase(),
        source: describe(source1, db1),
        target: describe(source2, db2),
        target_database: db2.name.clone(),
        target_major_version: if db2.server.is_azure_sql_database() { 99 } else { db2.server.major_version },
    };
    deploy::generate(comparison, db1, db2, options, mappings, &settings).map_err(|e| ArgError::new(exit::INVALID_VALUE, e))
}
