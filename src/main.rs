use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use quillsql::{Enclose, Options, TableAlias, analysis, format, style};

mod serve;

/// Formats T-SQL files in place, or stdin to stdout.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// File or directory to format (directories are searched for *.sql)
    #[arg(short, long, required_unless_present_any = ["stdin", "serve"], conflicts_with = "stdin")]
    path: Option<PathBuf>,
    /// Read UTF-8 SQL from stdin and write the result to stdout
    #[arg(long)]
    stdin: bool,
    /// Serve the SSMS extension: one JSON request per stdin line, one JSON reply per stdout line
    #[arg(long, conflicts_with_all = ["path", "stdin", "check"])]
    serve: bool,
    /// Report files that would change and exit with code 1, without writing
    #[arg(long, conflicts_with = "stdin")]
    check: bool,
    /// Report code analysis issues instead of formatting; exit code 1 if any are warnings
    #[arg(long, conflicts_with_all = ["stdin", "check"])]
    analyze: bool,
    /// Keep the original next to each changed file as <name>.bak
    #[arg(short = 'b', long)]
    create_backups: bool,
    /// Style file: QuillSQL options JSON or a sectioned style (.json)
    #[arg(short, long)]
    style: Option<PathBuf>,
    /// Keep existing whitespace; apply only the token rewrites
    #[arg(long)]
    no_layout: bool,
    /// Upper-case reserved words
    #[arg(long)]
    apply_casing: bool,
    /// Prefix N to non-ASCII string literals assigned to variables
    #[arg(short = 'n', long)]
    prefix_unicode: bool,
    /// Insert missing statement terminators
    #[arg(long)]
    insert_semicolons: bool,
    #[arg(short = 'e', long, value_enum)]
    enclose_identifiers: Option<Enclose>,
    #[arg(long, value_enum)]
    table_alias_style: Option<TableAlias>,
}

impl Cli {
    fn options(&self) -> Result<Options, String> {
        let mut o: Options = match &self.style {
            Some(path) => load_style(path)?,
            None => Options::default(),
        };
        o.layout &= !self.no_layout;
        o.apply_casing |= self.apply_casing;
        o.prefix_unicode |= self.prefix_unicode;
        o.insert_semicolons |= self.insert_semicolons;
        o.enclose_identifiers = self.enclose_identifiers.or(o.enclose_identifiers);
        o.table_alias_style = self.table_alias_style.or(o.table_alias_style);
        Ok(o)
    }
}

/// Reads a QuillSQL options file or a sectioned style file.
fn load_style(path: &Path) -> Result<Options, String> {
    let fail = |e: String| format!("{}: {e}", path.display());
    let bytes = fs::read(path).map_err(|e| fail(e.to_string()))?;
    let (text, _) = decode(&bytes).map_err(fail)?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| fail(e.to_string()))?;
    let (options, warnings) = style::options_from_value(&value).map_err(fail)?;
    for warning in &warnings {
        eprintln!("warning: {}: {warning}", path.display());
    }
    Ok(options)
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

fn decode_utf16(rest: &[u8], from: fn([u8; 2]) -> u16) -> Result<String, String> {
    if !rest.len().is_multiple_of(2) {
        return Err("truncated UTF-16".to_string());
    }
    let units = rest.as_chunks::<2>().0.iter().map(|c| from([c[0], c[1]]));
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|_| "invalid UTF-16".to_string())
}

fn decode(bytes: &[u8]) -> Result<(String, Encoding), String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        let text = std::str::from_utf8(rest).map_err(|_| "invalid UTF-8".to_string())?;
        Ok((text.to_string(), Encoding::Utf8Bom))
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        Ok((decode_utf16(rest, u16::from_le_bytes)?, Encoding::Utf16Le))
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        Ok((decode_utf16(rest, u16::from_be_bytes)?, Encoding::Utf16Be))
    } else {
        // Legacy code pages are refused, since guessing one could corrupt the file on rewrite.
        let text = std::str::from_utf8(bytes)
            .map_err(|_| "not UTF-8 or UTF-16 (save it as Unicode first)".to_string())?;
        Ok((text.to_string(), Encoding::Utf8))
    }
}

fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => [&[0xEF, 0xBB, 0xBF], text.as_bytes()].concat(),
        Encoding::Utf16Le => [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect(),
        Encoding::Utf16Be => [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect(),
    }
}

fn collect_sql_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_sql_files(&entry.path(), out)?;
        } else if kind.is_file()
            && entry.path().extension().is_some_and(|e| e.eq_ignore_ascii_case("sql"))
        {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// First unused `<name>.bak`, `<name>.bak.1`, ... so an earlier backup is never overwritten.
fn backup_path(path: &Path) -> PathBuf {
    let first = suffixed(path, ".bak");
    if !first.exists() {
        return first;
    }
    (1u32..)
        .map(|n| suffixed(path, &format!(".bak.{n}")))
        .find(|p| !p.exists())
        .expect("unbounded range")
}

/// Prints `file:line:column: severity CODE message` for each issue; returns the warning count.
fn analyze_file(path: &Path) -> Result<usize, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let (text, _) = decode(&bytes)?;
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut warnings = 0;
    for issue in analysis::analyze(&text, None) {
        let before = String::from_utf16_lossy(&units[..issue.start]);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        println!("{}:{line}:{column}: {} {} {}", path.display(), issue.severity, issue.code, issue.message);
        if issue.severity == "warning" {
            warnings += 1;
        }
    }
    Ok(warnings)
}

/// Returns whether the file differs from its formatted form.
fn process_file(path: &Path, opts: &Options, cli: &Cli) -> Result<bool, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let (text, encoding) = decode(&bytes)?;
    let formatted = format(&text, opts).map_err(|e| e.to_string())?;
    if formatted == text {
        return Ok(false);
    }
    if cli.check {
        return Ok(true);
    }
    if cli.create_backups {
        fs::copy(path, backup_path(path)).map_err(|e| format!("backup failed: {e}"))?;
    }
    // Replace atomically so a failed write cannot leave a truncated script behind.
    let tmp = suffixed(path, ".quill.tmp");
    fs::write(&tmp, encode(&formatted, encoding)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(true)
}

fn run_stdin(opts: &Options) -> Result<(), String> {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "stdin is not valid UTF-8".to_string())?;
    let formatted = format(&text, opts).map_err(|e| e.to_string())?;
    std::io::stdout().write_all(formatted.as_bytes()).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    // `quill schema ...` and `quill data ...` take SQL Compare style switches, not clap's.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("schema") {
        return ExitCode::from(quillsql::commands::schema::run(&argv[2..]));
    }
    let cli = Cli::parse();
    let opts = match cli.options() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    if cli.serve {
        return match serve::run(&opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        };
    }
    if cli.stdin {
        return match run_stdin(&opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        };
    }
    let root = cli.path.as_ref().expect("clap requires --path without --stdin");
    let mut files = Vec::new();
    if root.is_dir() {
        if let Err(e) = collect_sql_files(root, &mut files) {
            eprintln!("error: {}: {e}", root.display());
            return ExitCode::from(2);
        }
        files.sort();
    } else {
        files.push(root.clone());
    }
    let (mut changed, mut failed) = (0usize, 0usize);
    if cli.analyze {
        let mut warnings = 0;
        for file in &files {
            match analyze_file(file) {
                Ok(n) => warnings += n,
                Err(e) => {
                    failed += 1;
                    eprintln!("error: {}: {e}", file.display());
                }
            }
        }
        println!("{} file(s) analyzed, {warnings} warning(s), {failed} failed", files.len());
        return ExitCode::from(if failed > 0 { 2 } else if warnings > 0 { 1 } else { 0 });
    }
    for file in &files {
        match process_file(file, &opts, &cli) {
            Ok(true) => {
                changed += 1;
                let verb = if cli.check { "would format" } else { "formatted" };
                println!("{verb} {}", file.display());
            }
            Ok(false) => {}
            Err(e) => {
                failed += 1;
                eprintln!("error: {}: {e}", file.display());
            }
        }
    }
    println!("{} file(s) checked, {changed} changed, {failed} failed", files.len());
    if failed > 0 {
        ExitCode::from(2)
    } else if cli.check && changed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
