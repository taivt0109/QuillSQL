//! `quill schema`: compare two database schemas and deploy the differences.

use std::path::Path;

use super::common::{self, Source};
use crate::args::{self, ArgError, Args, Switch, exit};
use crate::catalog::{self, Database};
use crate::schema::select::Selection;
use crate::schema::{self, DiffKind, Difference, Mappings, Options, print};

const ACTIONS: [Switch; 6] = [
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
    common::GENERAL.into_iter().chain(common::SOURCE1).chain(common::SOURCE2).chain(ACTIONS).collect()
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
    let source1 = common::source(args, 1)?
        .ok_or_else(|| ArgError::new(exit::USAGE, "give database 1: /database1 (with /server1) or /snapshot1"))?;
    if let Some(path) = args.get("makesnapshot") {
        let path = Path::new(path);
        common::check_output(path, args)?;
        let db = load(&source1, args)?;
        save_snapshot(&db, path)?;
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
    let options = match args.get("options") {
        Some(list) => Options::parse(list).map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?,
        None => Options::default_set(),
    };
    options.validate().map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?;
    let selection =
        Selection::new(args.all("include"), args.all("exclude")).map_err(|e| ArgError::new(exit::INVALID_VALUE, e))?;
    if let Some(path) = args.get("json").filter(|p| *p != "-") {
        common::check_output(Path::new(path), args)?;
    }

    let db1 = load(&source1, args)?;
    let db2 = load(&source2, args)?;
    let mut comparison = schema::compare(&db1, &db2, &options, &Mappings::default());
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
    Ok(exit::SUCCESS)
}

/// Reads one side of the comparison.
pub fn load(source: &Source, args: &Args) -> Result<Database, ArgError> {
    match source {
        Source::Live(spec) => {
            if args.has("verbose") {
                eprintln!("Reading {}...", spec.describe());
            }
            catalog::read_database(spec)
                .map_err(|e| ArgError::new(exit::EXECUTION_FAILED, format!("{}: {e}", spec.describe())))
        }
        Source::Snapshot(path) => load_snapshot(path),
    }
}

pub fn save_snapshot(db: &Database, path: &Path) -> Result<(), ArgError> {
    let json = serde_json::to_vec_pretty(db).map_err(|e| ArgError::new(exit::GENERAL_ERROR, e.to_string()))?;
    std::fs::write(path, json).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{}: {e}", path.display())))
}

pub fn load_snapshot(path: &Path) -> Result<Database, ArgError> {
    let bytes = std::fs::read(path).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{}: {e}", path.display())))?;
    let db: Database = serde_json::from_slice(&bytes)
        .map_err(|e| ArgError::new(exit::DATA_ERROR, format!("{} is not a QuillSQL snapshot: {e}", path.display())))?;
    if db.model_version > catalog::MODEL_VERSION {
        return Err(ArgError::new(
            exit::DATA_ERROR,
            format!("{} was made by a newer QuillSQL (format {}); update quill", path.display(), db.model_version),
        ));
    }
    Ok(db)
}
