//! `quill schema`: compare two database schemas and deploy the differences.

use std::path::Path;

use super::common::{self, Source};
use crate::args::{self, ArgError, Args, Switch, exit};
use crate::catalog::{self, Database};

const ACTIONS: [Switch; 1] = [Switch {
    name: "makesnapshot",
    aliases: &["mksnap"],
    takes_value: true,
    repeatable: false,
    help: "Write a snapshot of database 1 to a file and stop",
}];

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
    let _source2 = common::source(args, 2)?.ok_or_else(|| {
        ArgError::new(exit::USAGE, "give database 2: /database2 (with /server2) or /snapshot2, or use /makesnapshot")
    })?;
    Err(ArgError::new(exit::GENERAL_ERROR, "schema comparison is not available in this build yet"))
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
