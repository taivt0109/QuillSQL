//! Command-line side of scripts folders, source control and filters: `/scripts1|2`,
//! `/sourcecontrol1|2` with `/revision1|2`, `/makescripts`, deploying to a scripts folder,
//! `/ignoreParserErrors` and `/filter`.

use std::path::{Path, PathBuf};

use super::common::Source;
use super::console;
use crate::args::{ArgError, Args, exit};
use crate::catalog::Database;
use crate::schema::filter::Filter;
use crate::schema::{Comparison, Difference, Opt, Options};
use crate::sources::{folder, parse, scm};

/// `/scriptsN`, or `/sourcecontrolN` with `/revisionN` and `/scriptsfolderxmlN`; `None` when
/// side N is something else.
pub fn source(args: &Args, n: u8) -> Result<Option<Source>, ArgError> {
    let key = |name: &str| format!("{name}{n}");
    let revision = args.get(&key("revision")).map(str::to_string);
    if let Some(path) = args.get(&key("scripts")) {
        let path = PathBuf::from(path);
        if !path.is_dir() {
            return Err(ArgError::new(exit::NOTHING_TO_COMPARE, format!("The scripts folder '{}' could not be found.", path.display())));
        }
        return Ok(Some(Source::Scripts { path, revision }));
    }
    if let Some(path) = args.get(&key("sca")) {
        // A SQL Change Automation project: read like a scripts folder (see folder::read).
        return Ok(Some(Source::Scripts { path: PathBuf::from(path), revision: None }));
    }
    if args.has(&key("sourcecontrol")) {
        let Some(revision) = revision else {
            return Err(ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("/sourcecontrol{n} needs /revision{n}")));
        };
        let xml = args.get(&key("scriptsfolderxml")).or(args.get("scriptsfolderxml")).unwrap_or_default();
        let text = if xml.is_empty() { String::new() } else { std::fs::read(xml).map(|b| folder::decode(&b, "UTF8")).unwrap_or_default() };
        let location = scm::location_from_xml(&text).ok_or_else(|| {
            ArgError::new(
                exit::NO_INPUT,
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
    console::verbose(&format!("Reading database scripts from '{}'...", path.display()));
    let mut f = folder::read(&dir).map_err(|e| ArgError::new(exit::NOTHING_TO_COMPARE, e))?;
    if checkout.is_some() {
        // Name the folder, not the temporary checkout.
        f.database.name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(f.database.name);
        let rel = |p: &Path| p.strip_prefix(&dir).map(|r| path.join(r)).unwrap_or_else(|_| p.to_path_buf());
        for m in &mut f.messages {
            m.file = rel(&m.file);
        }
    }
    for w in &f.warnings {
        console::warn(w);
    }
    let ignore = args.has("ignoreParserErrors");
    if !ignore
        && options.has(Opt::ThrowOnFileParseFailed)
        && let Some(m) = f.messages.iter().find(|m| m.kind == parse::MessageKind::InvalidSyntax)
    {
        return Err(ArgError::new(
            exit::PARSER_ERROR,
            format!("Errors occurred whilst parsing file {}\r\n'{}' at line {}, column {}", m.file.display(), m.detail, m.line, m.column),
        ));
    }
    if !ignore {
        for m in &f.messages {
            console::warn(&m.text());
        }
    }
    let db = f.database.clone();
    Ok((db, f))
}

/// Folders with an SSDT database project are compared with SsdtCompatibleOptions on.
pub fn ssdt_options(options: Options, folders: &[Option<&folder::Folder>]) -> Options {
    if !folders.iter().flatten().any(|f| f.is_ssdt()) {
        return options;
    }
    let group = Options::parse("SsdtCompatibleOptions").unwrap_or_default();
    group.iter().fold(options, |o, x| o.with(x))
}

/// `/makescripts:<folder>`: writes database 1 as a scripts folder.
pub fn make_scripts(args: &Args, db: &Database, dir: &Path, options: &Options, from: &str) -> Result<u8, ArgError> {
    let mut db = db.clone();
    if args.has("ignoreSourceCaseSensitivity") && db.case_sensitive() {
        db.collation = db.collation.replace("_CS_", "_CI_");
    }
    console::verbose(&format!("Creating folder of scripts '{}' from {from}...", dir.display()));
    let n = folder::write_all(&db, dir, options).map_err(|e| ArgError::new(exit::IO_ERROR, e))?;
    console::log(&format!("Scripts folder {} written: {n} object files.", dir.display()));
    Ok(exit::SUCCESS)
}

/// `/synchronize` with a scripts folder as database 2: rewrites the changed object files.
pub fn sync_folder(source: &Database, target: &folder::Folder, differences: &[Difference], options: &Options) -> Result<(), ArgError> {
    let changes = folder::sync(source, target, differences, options).map_err(|e| ArgError::new(exit::IO_ERROR, e))?;
    for p in &changes.written {
        console::verbose(&format!("Wrote {}", p.display()));
    }
    for p in &changes.deleted {
        console::verbose(&format!("Deleted {}", p.display()));
    }
    console::log(&format!(
        "Synchronized scripts folder {}: {} file(s) written, {} deleted.",
        target.path.display(),
        changes.written.len(),
        changes.deleted.len()
    ));
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

/// The filter of the run: `/filter`, else the project's (`execute` refuses `/exclude` and
/// non-trivial `/include` with either).
pub fn filter(args: &Args, project_filter: Option<&str>) -> Result<Option<Filter>, ArgError> {
    if let Some(path) = args.get("filter") {
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
pub fn apply_filter(f: &Filter, comparison: &mut Comparison) {
    for d in &mut comparison.differences {
        if !f.includes(d) {
            d.selected = false;
        }
    }
}
