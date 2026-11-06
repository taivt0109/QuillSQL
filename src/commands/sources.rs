//! Command-line side of scripts folders, source control and filters: `/scripts1|2`,
//! `/sourcecontrol1|2` with `/revision1|2`, `/makescripts`, deploying to a scripts folder,
//! `/ignoreParserErrors` and `/filter`.

use std::path::{Path, PathBuf};

use super::common::Source;
use crate::args::{ArgError, Args, Switch, exit};
use crate::catalog::Database;
use crate::schema::filter::Filter;
use crate::schema::{Comparison, Difference, Opt, Options};
use crate::sources::{folder, parse, scm};

/// SQL Compare's exit codes for scripts folders.
pub const NOTHING_TO_COMPARE: u8 = 60;
pub const PARSER_ERROR: u8 = 62;
/// `/include` with `/filter` or `/project`.
pub const DEPENDENCY: u8 = 8;

/// Per-side switches: a scripts folder or a revision of one in source control.
macro_rules! folder_switches {
    ($n:literal) => {
        [
            Switch {
                name: concat!("scripts", $n),
                aliases: &[concat!("scr", $n)],
                takes_value: true,
                repeatable: false,
                help: concat!("A scripts folder in place of database ", $n),
            },
            Switch {
                name: concat!("sourcecontrol", $n),
                aliases: &[],
                takes_value: false,
                repeatable: false,
                help: "A scripts folder in source control (with /revision and /scriptsfolderxml)",
            },
            Switch {
                name: concat!("revision", $n),
                aliases: &[concat!("r", $n), concat!("version", $n), concat!("v", $n)],
                takes_value: true,
                repeatable: false,
                help: "Revision of the scripts folder in source control (HEAD for the latest)",
            },
            Switch {
                name: concat!("scriptsfolderxml", $n),
                aliases: &[concat!("sfx", $n), concat!("scriptfolderxml", $n)],
                takes_value: true,
                repeatable: false,
                help: "File with the location of the scripts folder in source control",
            },
        ]
    };
}

pub const FOLDER1: [Switch; 4] = folder_switches!("1");
pub const FOLDER2: [Switch; 4] = folder_switches!("2");

/// Switches for `quill schema` only.
pub const SCHEMA: [Switch; 5] = [
    Switch {
        name: "makescripts",
        aliases: &["mkscr"],
        takes_value: true,
        repeatable: false,
        help: "Write database 1 as a scripts folder and stop",
    },
    Switch {
        name: "ignoreSourceCaseSensitivity",
        aliases: &[],
        takes_value: false,
        repeatable: false,
        help: "Do not detect case sensitivity when creating a scripts folder",
    },
    Switch { name: "ignoreParserErrors", aliases: &[], takes_value: false, repeatable: false, help: "Ignore syntax errors in scripts folders" },
    Switch {
        name: "scriptsfolderxml",
        aliases: &["sfx", "scriptfolderxml"],
        takes_value: true,
        repeatable: false,
        help: "File with the location of the scripts folder in source control (both sides)",
    },
    Switch { name: "filter", aliases: &["ftr"], takes_value: true, repeatable: false, help: "A saved filter file (.scpf) that selects the objects" },
];

/// `/scriptsN`, or `/sourcecontrolN` with `/revisionN` and `/scriptsfolderxmlN`; `None` when
/// side N is something else.
pub fn source(args: &Args, n: u8) -> Result<Option<Source>, ArgError> {
    let key = |name: &str| format!("{name}{n}");
    let revision = args.get(&key("revision")).map(str::to_string);
    if let Some(path) = args.get(&key("scripts")) {
        let path = PathBuf::from(path);
        if !path.is_dir() {
            return Err(ArgError::new(NOTHING_TO_COMPARE, format!("The scripts folder '{}' could not be found.", path.display())));
        }
        return Ok(Some(Source::Scripts { path, revision }));
    }
    if args.has(&key("sourcecontrol")) {
        let Some(revision) = revision else {
            return Err(ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("/sourcecontrol{n} needs /revision{n}")));
        };
        let xml = args.get(&key("scriptsfolderxml")).or(args.get("scriptsfolderxml")).ok_or_else(|| {
            ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("/sourcecontrol{n} needs /scriptsfolderxml{n}: a file with the scripts folder location"))
        })?;
        let text = std::fs::read(xml).map(|b| folder::decode(&b, "UTF8")).unwrap_or_default();
        let location = scm::location_from_xml(&text).ok_or_else(|| {
            ArgError::new(
                66,
                format!("The file '{xml}' containing the script folder location XML fragment could not be parsed. Check that the file exists and that its contents are well-formed."),
            )
        })?;
        return Ok(Some(Source::Scripts { path: PathBuf::from(location), revision: Some(revision) }));
    }
    Ok(None)
}

/// Reads a scripts folder (checking out the revision first), printing what the parser skipped.
pub fn load(path: &Path, revision: Option<&str>, args: &Args, options: &Options) -> Result<(Database, folder::Folder), ArgError> {
    let checkout = match revision {
        Some(r) => Some(scm::checkout(&path.to_string_lossy(), r).map_err(|e| ArgError::new(exit::IO_ERROR, e))?),
        None => None,
    };
    let dir = checkout.as_ref().map(|c| c.dir.clone()).unwrap_or_else(|| path.to_path_buf());
    if args.has("verbose") {
        eprintln!("Reading database scripts from '{}'...", path.display());
    }
    let mut f = folder::read(&dir).map_err(|e| ArgError::new(NOTHING_TO_COMPARE, e))?;
    if checkout.is_some() {
        // Name the folder, not the temporary checkout.
        f.database.name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(f.database.name);
        let rel = |p: &Path| p.strip_prefix(&dir).map(|r| path.join(r)).unwrap_or_else(|_| p.to_path_buf());
        for m in &mut f.messages {
            m.file = rel(&m.file);
        }
    }
    let quiet = args.has("quiet");
    for w in &f.warnings {
        if !quiet {
            eprintln!("Warning: {w}");
        }
    }
    let ignore = args.has("ignoreParserErrors");
    if !ignore
        && options.has(Opt::ThrowOnFileParseFailed)
        && let Some(m) = f.messages.iter().find(|m| m.kind == parse::MessageKind::InvalidSyntax)
    {
        return Err(ArgError::new(
            PARSER_ERROR,
            format!("Errors occurred whilst parsing file {}\r\n'{}' at line {}, column {}", m.file.display(), m.detail, m.line, m.column),
        ));
    }
    if !quiet && !ignore {
        for m in &f.messages {
            eprintln!("Warning: {}", m.text());
        }
    }
    let db = f.database.clone();
    Ok((db, f))
}

/// `/makescripts:<folder>`: writes database 1 as a scripts folder.
pub fn make_scripts(args: &Args, db: &Database, dir: &Path, options: &Options, from: &str) -> Result<u8, ArgError> {
    let mut db = db.clone();
    if args.has("ignoreSourceCaseSensitivity") && db.case_sensitive() {
        db.collation = db.collation.replace("_CS_", "_CI_");
    }
    if !args.has("quiet") {
        println!("Creating folder of scripts '{}' from {from}...", dir.display());
    }
    let n = folder::write_all(&db, dir, options).map_err(|e| ArgError::new(exit::IO_ERROR, e))?;
    if !args.has("quiet") {
        println!("Scripts folder {} written: {n} object files.", dir.display());
    }
    Ok(exit::SUCCESS)
}

/// `/synchronize` with a scripts folder as database 2: rewrites the changed object files.
pub fn sync_folder(args: &Args, source: &Database, target: &folder::Folder, differences: &[Difference], options: &Options) -> Result<(), ArgError> {
    let changes = folder::sync(source, target, differences, options).map_err(|e| ArgError::new(exit::IO_ERROR, e))?;
    if !args.has("quiet") {
        if args.has("verbose") {
            for p in &changes.written {
                println!("Wrote {}", p.display());
            }
            for p in &changes.deleted {
                println!("Deleted {}", p.display());
            }
        }
        println!(
            "Synchronized scripts folder {}: {} file(s) written, {} deleted.",
            target.path.display(),
            changes.written.len(),
            changes.deleted.len()
        );
    }
    Ok(())
}

/// Pre- and post-deployment scripts of a scripts folder as database 1 go around the deployment
/// script, unless IgnorePrePostScripts.
pub fn add_pre_post(script: &mut crate::schema::deploy::DeploymentScript, folder: &folder::Folder, options: &Options) -> Result<(), ArgError> {
    if options.has(Opt::IgnorePrePostScripts) {
        return Ok(());
    }
    let (pre, post) = folder::pre_post_scripts(&folder.path, &folder.info).map_err(|e| ArgError::new(exit::IO_ERROR, e))?;
    use crate::schema::deploy::{Severity, Warning, split_batches};
    let note = |message: &str| Warning { severity: Severity::Low, object: String::new(), message: message.into() };
    if let Some(pre) = pre {
        script.sql = format!("{}\r\nGO\r\n{}", pre.trim_end(), script.sql);
        script.batches.splice(0..0, split_batches(&pre));
        script.warnings.push(note("Including pre-deployment script"));
    }
    if let Some(post) = post {
        script.sql = format!("{}\r\nGO\r\n{}\r\nGO\r\n", script.sql.trim_end(), post.trim_end());
        script.batches.extend(split_batches(&post));
        script.warnings.push(note("Including post-deployment script"));
    }
    Ok(())
}

/// The filter of the run: `/filter`, else the project's. Checks SQL Compare's rules for
/// `/include` and `/exclude` with a filter.
pub fn filter(args: &Args, project_filter: Option<&str>) -> Result<Option<Filter>, ArgError> {
    if let Some(path) = args.get("filter") {
        if args.has("exclude") {
            return Err(ArgError::new(exit::USAGE, "The /filter switch cannot be used in conjunction with the /exclude switch."));
        }
        let trivial = |v: &String| v.split(',').all(|x| x.trim().eq_ignore_ascii_case("identical") || x.trim().eq_ignore_ascii_case("staticdata"));
        if !args.all("include").iter().all(trivial) {
            return Err(ArgError::new(
                DEPENDENCY,
                "The /include switch cannot be specified together with the /project or /filter switches except for /include:identical and /include:staticdata",
            ));
        }
        let path = Path::new(path);
        let f = Filter::load(path).map_err(|e| ArgError::new(exit::DATA_ERROR, e))?;
        return Ok(Some(f));
    }
    match project_filter {
        Some(xml) if !args.has("include") && !args.has("exclude") => {
            Filter::from_xml(xml).map(Some).map_err(|e| ArgError::new(exit::DATA_ERROR, format!("the project's filter is invalid: {e}")))
        }
        _ => Ok(None),
    }
}

/// Leaves out of the run what the filter does not include.
pub fn apply_filter(f: &Filter, comparison: &mut Comparison, listed: &mut Vec<Difference>) {
    for d in &mut comparison.differences {
        if !f.includes(d) {
            d.selected = false;
        }
    }
    listed.retain(|d| f.includes(d));
}
