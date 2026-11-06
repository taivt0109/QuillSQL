//! Switches and helpers shared by `quill schema` and `quill data`: the switch tables of SQL
//! Compare's shared command line, their rules, and the run loop (title, `/help`, `/argfile`,
//! `/out`, `/outputwidth`, `/logLevel`, `Error:` output and exit codes).

use std::path::Path;

use super::console::{self, LogLevel, Mode};
use super::enums;
use crate::args::{self, ArgError, Args, Rule, Switch, exit};
use crate::catalog::{self, Database};
use crate::db::{Auth, ConnectionSpec};
use crate::schema::deploy;

macro_rules! source_switches {
    ($n:literal) => {
        [
            Switch {
                name: concat!("server", $n),
                aliases: &[concat!("s", $n)],
                takes_value: true,
                repeatable: false,
                help: concat!("Server of database ", $n, ": host, host,port or host\\instance (default: (local))"),
            },
            Switch {
                name: concat!("database", $n),
                aliases: &[concat!("db", $n)],
                takes_value: true,
                repeatable: false,
                help: concat!("Database ", $n),
            },
            Switch {
                name: concat!("username", $n),
                aliases: &[concat!("u", $n)],
                takes_value: true,
                repeatable: false,
                help: "SQL Server login; omit for Windows authentication",
            },
            Switch {
                name: concat!("password", $n),
                aliases: &[concat!("p", $n)],
                takes_value: true,
                repeatable: false,
                help: concat!("Password; prefer the QUILL_PASSWORD", $n, " environment variable"),
            },
            Switch {
                name: concat!("activedirectory", $n),
                aliases: &[concat!("ad", $n)],
                takes_value: false,
                repeatable: false,
                help: concat!("Microsoft Entra: access token from QUILL_ACCESS_TOKEN", $n),
            },
            Switch {
                name: concat!("connection", $n),
                aliases: &[concat!("cs", $n)],
                takes_value: true,
                repeatable: false,
                help: "Full ADO.NET connection string (replaces server, login and encryption switches)",
            },
            Switch {
                name: concat!("encrypt", $n),
                aliases: &[],
                takes_value: true,
                repeatable: false,
                help: "true (default) or false",
            },
            Switch {
                name: concat!("trustServerCertificate", $n),
                aliases: &[concat!("tsc", $n)],
                takes_value: false,
                repeatable: false,
                help: "Accept the server certificate without validation",
            },
            Switch {
                name: concat!("snapshot", $n),
                aliases: &[concat!("sn", $n)],
                takes_value: true,
                repeatable: false,
                help: concat!("Read database ", $n, " from a snapshot file made with /makesnapshot"),
            },
            Switch {
                name: concat!("backup", $n),
                aliases: &[concat!("b", $n)],
                takes_value: true,
                repeatable: false,
                help: "Backup files (;-separated) in place of the database (not supported yet)",
            },
            Switch {
                name: concat!("backuppasswords", $n),
                aliases: &[concat!("bp", $n), concat!("bpsw", $n)],
                takes_value: true,
                repeatable: false,
                help: "Passwords of the backup files",
            },
            Switch {
                name: concat!("backupset", $n),
                aliases: &[concat!("bs", $n), concat!("bks", $n)],
                takes_value: true,
                repeatable: false,
                help: "Backup set to read (default: the latest)",
            },
            Switch {
                name: concat!("scripts", $n),
                aliases: &[concat!("scr", $n)],
                takes_value: true,
                repeatable: false,
                help: "A scripts folder in place of the database (not supported yet)",
            },
            Switch {
                name: concat!("sca", $n),
                aliases: &[],
                takes_value: true,
                repeatable: false,
                help: "A SQL Change Automation project in place of the database (not supported yet)",
            },
            Switch {
                name: concat!("sourcecontrol", $n),
                aliases: &[],
                takes_value: false,
                repeatable: false,
                help: "A source-controlled scripts folder in place of the database (not supported yet)",
            },
            Switch {
                name: concat!("revision", $n),
                aliases: &[concat!("r", $n), concat!("version", $n), concat!("v", $n)],
                takes_value: true,
                repeatable: false,
                help: "Source control revision (HEAD for the latest)",
            },
            Switch {
                name: concat!("versionusername", $n),
                aliases: &[concat!("vu", $n)],
                takes_value: true,
                repeatable: false,
                help: "Source control user name",
            },
            Switch {
                name: concat!("versionpassword", $n),
                aliases: &[concat!("vp", $n)],
                takes_value: true,
                repeatable: false,
                help: "Source control password",
            },
            Switch {
                name: concat!("scriptsfolderxml", $n),
                aliases: &[concat!("sfx", $n), concat!("scriptfolderxml", $n)],
                takes_value: true,
                repeatable: false,
                help: "File with the scripts folder location XML",
            },
        ]
    };
}

pub const SOURCE1: [Switch; 19] = source_switches!("1");
pub const SOURCE2: [Switch; 21] = {
    let s = source_switches!("2");
    [
        s[0].copy(), s[1].copy(), s[2].copy(), s[3].copy(), s[4].copy(), s[5].copy(), s[6].copy(), s[7].copy(), s[8].copy(), s[9].copy(),
        s[10].copy(), s[11].copy(), s[12].copy(), s[13].copy(), s[14].copy(), s[15].copy(), s[16].copy(), s[17].copy(), s[18].copy(),
        Switch { name: "empty2", aliases: &["e2"], takes_value: false, repeatable: false, help: "Compare with an empty database" },
        Switch {
            name: "scriptsfolderxml",
            aliases: &["sfx", "scriptfolderxml"],
            takes_value: true,
            repeatable: false,
            help: "File with the scripts folder location XML (both sides)",
        },
    ]
};

impl Switch {
    const fn copy(&self) -> Switch {
        Switch { name: self.name, aliases: self.aliases, takes_value: self.takes_value, repeatable: self.repeatable, help: self.help }
    }
}

/// Switches for writing and running a deployment script.
pub const DEPLOY: [Switch; 21] = [
    Switch { name: "scriptfile", aliases: &["sf"], takes_value: true, repeatable: false, help: "Write the deployment script to a file" },
    Switch {
        name: "synchronize",
        aliases: &["sync", "synch", "synchronise"],
        takes_value: false,
        repeatable: false,
        help: "Deploy the differences to database 2",
    },
    Switch {
        name: "abortOnWarnings",
        aliases: &["aow", "abortOnWarning"],
        takes_value: true,
        repeatable: false,
        help: "None (default), Medium or High: stop before deploying when warnings this serious exist (exit 61)",
    },
    Switch { name: "showWarnings", aliases: &["warn"], takes_value: false, repeatable: false, help: "Print the deployment warnings" },
    Switch {
        name: "transactionIsolationLevel",
        aliases: &["til"],
        takes_value: true,
        repeatable: false,
        help: "Isolation level for the deployment transaction (default SERIALIZABLE)",
    },
    Switch {
        name: "syncScriptEncoding",
        aliases: &["senc", "scriptencoding"],
        takes_value: true,
        repeatable: false,
        help: "UTF8WithPreamble (default), UTF8, Unicode or ASCII",
    },
    Switch {
        name: "outputProject",
        aliases: &["outpr", "opr"],
        takes_value: true,
        repeatable: false,
        help: "Write the settings of this run to a project file (.scp/.sdc, or QuillSQL's .qsc/.qdc)",
    },
    Switch {
        name: "makescripts",
        aliases: &["mkscr"],
        takes_value: true,
        repeatable: false,
        help: "Create a scripts folder from database 1 (not supported yet)",
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
        name: "allowIdenticalDatabases",
        aliases: &["aidb", "aid"],
        takes_value: false,
        repeatable: false,
        help: "Deprecated: use /include:Identical",
    },
    Switch { name: "makebackup", aliases: &["mb"], takes_value: false, repeatable: false, help: "Back up database 2 before deploying (native SQL Server backup)" },
    Switch { name: "backupprovider", aliases: &["bpr"], takes_value: true, repeatable: false, help: "native or sqb" },
    Switch { name: "backuptype", aliases: &["bt"], takes_value: true, repeatable: false, help: "full or differential" },
    Switch { name: "backupfolder", aliases: &["bd"], takes_value: true, repeatable: false, help: "Folder for the backup" },
    Switch { name: "backupfile", aliases: &["bf"], takes_value: true, repeatable: false, help: "File name of the backup" },
    Switch { name: "backupoverwriteexisting", aliases: &["boe"], takes_value: false, repeatable: false, help: "Overwrite an existing backup file" },
    Switch { name: "backupcompression", aliases: &["bc"], takes_value: true, repeatable: false, help: "Compression level 0 to 4 (SQL Backup)" },
    Switch { name: "backupencryption", aliases: &["be"], takes_value: false, repeatable: false, help: "Encrypt the backup (SQL Backup)" },
    Switch { name: "backuppassword", aliases: &["bp"], takes_value: true, repeatable: false, help: "Password of the encrypted backup" },
    Switch { name: "backupnumberofthreads", aliases: &["bth"], takes_value: true, repeatable: false, help: "Backup threads, 0 to 32 (SQL Backup)" },
];

pub const GENERAL: [Switch; 15] = [
    Switch { name: "help", aliases: &["?"], takes_value: false, repeatable: false, help: "Show this help; with /verbose, also exit codes and values" },
    Switch { name: "html", aliases: &[], takes_value: false, repeatable: false, help: "With /help: print the help as HTML" },
    Switch {
        name: "project",
        aliases: &["pr"],
        takes_value: true,
        repeatable: false,
        help: "A .qsc/.qdc project (or a SQL Compare .scp / Data Compare .sdc); switches override its settings",
    },
    Switch { name: "quiet", aliases: &["q"], takes_value: false, repeatable: false, help: "Print errors only" },
    Switch { name: "verbose", aliases: &["v"], takes_value: false, repeatable: false, help: "Print every difference" },
    Switch { name: "force", aliases: &["f"], takes_value: false, repeatable: false, help: "Overwrite output files" },
    Switch { name: "argfile", aliases: &[], takes_value: true, repeatable: false, help: "Read the switches from an XML file" },
    Switch { name: "out", aliases: &[], takes_value: true, repeatable: false, help: "Write the console output to a file" },
    Switch { name: "outputwidth", aliases: &[], takes_value: true, repeatable: false, help: "Console width for the output, 80 to 1024" },
    Switch { name: "logLevel", aliases: &["log"], takes_value: true, repeatable: false, help: "None (default), Error, Warning or Verbose: write a log file" },
    Switch { name: "IAgreeToTheEULA", aliases: &[], takes_value: false, repeatable: false, help: "Accepted for compatibility; QuillSQL is MIT licensed" },
    Switch { name: "auth", aliases: &[], takes_value: false, repeatable: false, help: "Accepted for compatibility; QuillSQL needs no sign-in" },
    Switch { name: "logout", aliases: &[], takes_value: false, repeatable: false, help: "Accepted for compatibility" },
    Switch { name: "email", aliases: &[], takes_value: true, repeatable: false, help: "Accepted for compatibility" },
    Switch { name: "token", aliases: &[], takes_value: true, repeatable: false, help: "Accepted for compatibility" },
];

macro_rules! source_rules {
    ($n:literal) => {
        [
            Rule { name: concat!("server", $n), depends: &[concat!("database", $n)], ..Rule::NONE },
            Rule { name: concat!("username", $n), depends: &[concat!("database", $n)], ..Rule::NONE },
            Rule { name: concat!("snapshot", $n), path: true, ..Rule::NONE },
            Rule {
                name: concat!("backup", $n),
                excludes: &[concat!("database", $n), concat!("server", $n), concat!("username", $n), concat!("password", $n)],
                ..Rule::NONE
            },
            Rule { name: concat!("backuppasswords", $n), depends: &[concat!("backup", $n)], ..Rule::NONE },
            Rule { name: concat!("backupset", $n), depends: &[concat!("backup", $n)], ..Rule::NONE },
            Rule {
                name: concat!("scripts", $n),
                excludes: &[
                    concat!("backup", $n),
                    concat!("database", $n),
                    concat!("server", $n),
                    concat!("username", $n),
                    concat!("password", $n),
                    concat!("sca", $n),
                ],
                path: true,
                ..Rule::NONE
            },
            Rule {
                name: concat!("sca", $n),
                excludes: &[
                    concat!("backup", $n),
                    concat!("database", $n),
                    concat!("server", $n),
                    concat!("username", $n),
                    concat!("password", $n),
                    concat!("scripts", $n),
                    concat!("sourcecontrol", $n),
                ],
                ..Rule::NONE
            },
            Rule {
                name: concat!("sourcecontrol", $n),
                excludes: &[
                    concat!("backup", $n),
                    concat!("database", $n),
                    concat!("server", $n),
                    concat!("username", $n),
                    concat!("password", $n),
                    concat!("scripts", $n),
                    concat!("sca", $n),
                ],
                depends: &[concat!("revision", $n)],
                ..Rule::NONE
            },
            Rule { name: concat!("versionusername", $n), depends: &[concat!("revision", $n)], ..Rule::NONE },
            Rule { name: concat!("versionpassword", $n), depends: &[concat!("revision", $n)], ..Rule::NONE },
            Rule { name: concat!("scriptsfolderxml", $n), excludes: &["scriptsfolderxml"], path: true, ..Rule::NONE },
            Rule { name: concat!("encrypt", $n), values: &[&["true", "yes", "1", "on"], &["false", "no", "0", "off"]], ..Rule::NONE },
        ]
    };
}

const SOURCE1_RULES: [Rule; 13] = source_rules!("1");
const SOURCE2_RULES: [Rule; 13] = source_rules!("2");

const OTHER_RULES: [Rule; 26] = [
    Rule { name: "quiet", excludes: &["verbose"], ..Rule::NONE },
    Rule { name: "verbose", excludes: &["quiet"], ..Rule::NONE },
    Rule { name: "html", depends: &["help"], ..Rule::NONE },
    Rule { name: "argfile", excludes: &["*"], path: true, ..Rule::NONE },
    Rule { name: "out", path: true, ..Rule::NONE },
    Rule { name: "outputwidth", range: Some((80, 1024)), ..Rule::NONE },
    Rule { name: "logLevel", values: enums::LOG_LEVELS, ..Rule::NONE },
    Rule { name: "logout", depends: &["auth"], ..Rule::NONE },
    Rule { name: "email", depends: &["token"], ..Rule::NONE },
    Rule { name: "token", depends: &["email"], ..Rule::NONE },
    Rule { name: "project", excludes: &["revision1", "revision2"], path: true, ..Rule::NONE },
    Rule {
        name: "empty2",
        excludes: &["backup2", "database2", "revision2", "server2", "username2", "password2", "scripts2", "sourcecontrol2", "makescripts", "synchronize"],
        ..Rule::NONE
    },
    Rule {
        name: "makescripts",
        excludes: &["backup2", "database2", "server2", "username2", "password2", "project", "scripts2", "include", "exclude"],
        path: true,
        ..Rule::NONE
    },
    Rule { name: "ignoreSourceCaseSensitivity", depends: &["makescripts"], ..Rule::NONE },
    Rule { name: "synchronize", excludes: &["backup2"], ..Rule::NONE },
    Rule {
        name: "allowIdenticalDatabases",
        deprecated: Some(
            "/allowIdenticalDatabases has been deprecated. Please use /include:identical instead.  /Include:Identical suppresses the exit code if the two data sources are identical. If /Include:Identical  is not set, the exit code 63 is returned.",
        ),
        ..Rule::NONE
    },
    Rule { name: "abortOnWarnings", values: enums::WARNING_LEVELS, ..Rule::NONE },
    Rule { name: "scriptfile", path: true, ..Rule::NONE },
    Rule { name: "syncScriptEncoding", depends: &["scriptfile"], values: enums::ENCODINGS, ..Rule::NONE },
    Rule { name: "outputProject", path: true, ..Rule::NONE },
    Rule { name: "backupprovider", values: enums::BACKUP_PROVIDERS, ..Rule::NONE },
    Rule { name: "backuptype", depends: &["makebackup"], values: enums::BACKUP_TYPES, ..Rule::NONE },
    Rule { name: "backupcompression", depends: &["makebackup"], range: Some((0, 4)), ..Rule::NONE },
    Rule { name: "backupencryption", depends: &["makebackup", "backuppassword"], ..Rule::NONE },
    Rule { name: "backuppassword", depends: &["makebackup", "backupencryption"], ..Rule::NONE },
    Rule { name: "backupnumberofthreads", depends: &["makebackup"], range: Some((0, 32)), ..Rule::NONE },
];

const BACKUP_OUTPUT_RULES: [Rule; 3] = [
    Rule { name: "backupfolder", depends: &["makebackup"], path: true, ..Rule::NONE },
    Rule { name: "backupfile", depends: &["makebackup"], path: true, ..Rule::NONE },
    Rule { name: "backupoverwriteexisting", depends: &["makebackup"], ..Rule::NONE },
];

/// Rules of the shared switches.
pub fn shared_rules() -> Vec<Rule> {
    SOURCE1_RULES.into_iter().chain(SOURCE2_RULES).chain(OTHER_RULES).chain(BACKUP_OUTPUT_RULES).collect()
}

/// Switches with a default value: a dependency on them is always met.
const DEFAULTED: &[&str] = &["server1", "server2", "options", "datacompareoptions", "logLevel", "abortOnWarnings", "syncScriptEncoding"];

/// One command line: `quill schema` or `quill data`.
pub struct Cli<'a> {
    pub command: &'static str,
    /// Title line, without the version.
    pub product: &'static str,
    pub summary: &'static str,
    pub switches: &'a [Switch],
    pub rules: &'a [Rule],
    pub require_one_of: &'static [&'static str],
}

/// Set in the copy of the process that writes to an `/out` file.
const OUT_CHILD: &str = "QUILL_CLI_OUT_CHILD";

/// Parses `argv`, handles the switches every command shares, and runs `execute`.
pub fn run(cli: &Cli, argv: &[String], execute: impl Fn(&Args) -> Result<u8, ArgError>) -> u8 {
    let mut args = match args::parse(cli.switches, argv) {
        Ok(a) => a,
        Err(e) => return usage_error(cli, &e, std::env::var_os(OUT_CHILD).is_some()),
    };
    // No title in the `/out` copy (the first process printed it) or before JSON on standard output.
    let child = std::env::var_os(OUT_CHILD).is_some() || args.get("json") == Some("-");
    if args.has("help") {
        if !child {
            console::title(cli.product);
        }
        if args.has("verbose") {
            console::set_mode(Mode::Verbose);
        }
        print!("{}", if args.has("html") { help_html(cli) } else { help(cli) });
        return exit::SUCCESS;
    }
    console::set_mode(if args.has("verbose") {
        Mode::Verbose
    } else if args.has("quiet") {
        Mode::Quiet
    } else {
        Mode::Normal
    });
    if let Err(e) = args::validate(&mut args, cli.rules, cli.require_one_of, DEFAULTED) {
        return usage_error(cli, &e, child);
    }
    if let Some(file) = args.get("argfile").map(str::to_string) {
        args = match args::read_argfile(cli.switches, &file).and_then(|mut a| args::validate(&mut a, cli.rules, cli.require_one_of, DEFAULTED).map(|_| a)) {
            Ok(a) => a,
            Err(e) => return usage_error(cli, &e, child),
        };
        if args.has("verbose") {
            console::set_mode(Mode::Verbose);
        } else if args.has("quiet") {
            console::set_mode(Mode::Quiet);
        }
    }
    if let Some(w) = args.get("outputwidth") {
        console::set_width(w.trim().parse().unwrap_or(80));
    }
    if let Some(out) = args.get("out").filter(|_| !child) {
        console::title(cli.product);
        return redirect(cli, argv, &args, out);
    }
    if !child {
        console::title(cli.product);
    }
    let level = match args.get("logLevel").unwrap_or("None") {
        "Error" => LogLevel::Error,
        "Warning" => LogLevel::Warning,
        "Verbose" => LogLevel::Verbose,
        _ => LogLevel::None,
    };
    console::start_log(level, cli.product);
    for w in args::deprecations(&args, cli.rules) {
        console::warn(w);
    }
    if args.has("auth") || args.has("logout") {
        console::log("QuillSQL needs no sign-in or activation; nothing to do.");
        return exit::SUCCESS;
    }
    console::exit_on_ctrl_c();
    match unsupported(cli.command, &args).and_then(|_| execute(&args)) {
        Ok(code) => code,
        Err(e) => fail(e),
    }
}

/// A parse error: printed on standard output with a pointer to the help, as SQL Compare does.
fn usage_error(cli: &Cli, e: &ArgError, child: bool) -> u8 {
    if !child {
        console::title(cli.product);
    }
    if console::mode() != Mode::Quiet {
        let w = console::width();
        println!("{}", console::fixed_width(&e.message, w, "", true));
        println!("{}", console::fixed_width(&format!("Please use quill {0} /? or quill {0} /verbose /? for more information.", cli.command), w, "", true));
    }
    e.code
}

/// `/out`: runs the command again with standard output going to the file.
fn redirect(cli: &Cli, argv: &[String], args: &Args, out: &str) -> u8 {
    let path = Path::new(out);
    if path.exists() && !args.has("force") {
        return fail(ArgError::new(exit::IO_ERROR, format!("Cannot redirect output to file '{out}' because the file already exists.")));
    }
    console::verbose(&format!("Redirecting standard output to: {out}"));
    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => return fail(ArgError::new(exit::IO_ERROR, format!("Unable to create output file {out}. Error: {e}"))),
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return fail(ArgError::new(exit::GENERAL_ERROR, e.to_string())),
    };
    let status = std::process::Command::new(exe).arg(cli.command).args(argv).env(OUT_CHILD, "1").stdout(file).status();
    match status.map(|s| s.code()) {
        Ok(Some(code)) if (0..=255).contains(&code) => code as u8,
        Ok(Some(code)) => std::process::exit(code),
        Ok(None) => exit::GENERAL_ERROR,
        Err(e) => fail(ArgError::new(exit::GENERAL_ERROR, e.to_string())),
    }
}

/// Sources and actions QuillSQL does not have yet stop the run with exit 69.
/// "Back up target before deployment" from `/makebackup` or the project's backup settings
/// (`/backup…` switches override them, as SQL Compare's command line does): the backup becomes
/// the first batch of the script. `target` is database 2 when it is a live database.
pub fn add_backup(args: &Args, project: Option<&crate::schema::backup::BackupOptions>, target: Option<&crate::db::ConnectionSpec>, script: &mut crate::schema::deploy::DeploymentScript) -> Result<(), ArgError> {
    let mut o = project.filter(|b| b.enabled).cloned().unwrap_or_default();
    if !args.has("makebackup") && !o.enabled {
        return Ok(());
    }
    o.enabled = true;
    if let Some(p) = args.get("backupprovider") {
        o.provider = p.to_ascii_lowercase();
    }
    if let Some(t) = args.get("backuptype") {
        o.differential = t.eq_ignore_ascii_case("differential");
    }
    if let Some(f) = args.get("backupfolder") {
        o.folder = f.to_string();
    }
    match args.get("backupfile") {
        Some(f) => {
            o.file = f.to_string();
            o.name_automatically = false;
        }
        None => o.name_automatically = true,
    }
    if args.has("backupoverwriteexisting") {
        o.overwrite = true;
    }
    if let Some(c) = args.get("backupcompression").and_then(|c| c.parse().ok()) {
        o.compression_level = c;
    }
    if args.has("backupencryption") {
        o.encryption = "aes256".into();
    }
    o.password = args.get("backuppassword").map(str::to_string);
    if let Some(n) = args.get("backupnumberofthreads").and_then(|n| n.parse().ok()) {
        o.thread_count = n;
    }
    let Some(target) = target else {
        if args.has("makebackup") {
            return Err(ArgError::new(exit::USAGE, "/makebackup needs database 2 to be a live database."));
        }
        return Ok(());
    };
    let caps = crate::schema::backup::capabilities(target).map_err(|e| ArgError::new(exit::EXECUTION_FAILED, e))?;
    if o.sqb() && caps.sqb_version == 0 {
        return Err(ArgError::new(exit::UNAVAILABLE, format!("SQL Backup is not installed on {}; use /backupprovider:native.", target.server)));
    }
    if !o.sqb() {
        for name in ["backupcompression", "backupencryption", "backuppassword", "backupnumberofthreads"] {
            if args.has(name) {
                console::warn(&format!("/{name} applies to SQL Backup only and is ignored for a native backup."));
            }
        }
    }
    let plan = crate::schema::backup::Plan { options: &o, database: &target.database, utc: crate::schema::backup::utc_stamp(), caps: &caps };
    console::verbose(&format!("Backing up {} to {} before deploying", target.database, plan.file()));
    plan.add_to(script);
    Ok(())
}
pub fn unsupported(command: &str, args: &Args) -> Result<(), ArgError> {
    let _ = command;
    for name in ["backup1", "backup2", "sca1", "sca2"] {
        if args.has(name) {
            return Err(ArgError::new(exit::UNAVAILABLE, format!("/{name} is not supported by QuillSQL yet.")));
        }
    }
    Ok(())
}

fn rule_of<'a>(cli: &'a Cli, name: &str) -> Option<&'a Rule> {
    cli.rules.iter().find(|r| r.name.eq_ignore_ascii_case(name))
}

const EXIT_CODES: &[(u16, &str)] = &[
    (0, "Success."),
    (1, "General error code."),
    (3, "Illegal argument duplication."),
    (8, "Unsatisfied argument dependency or violated exclusion."),
    (11, "An argument that takes no value was given one."),
    (32, "Value out of range."),
    (33, "Value overflow."),
    (34, "Invalid value."),
    (60, "Nothing to compare: a data source does not exist."),
    (61, "Serious warnings apply to the deployment (/abortOnWarnings)."),
    (62, "Syntax errors were found in a scripts folder."),
    (63, "The databases are identical (use /include:Identical to suppress)."),
    (64, "General command line usage error."),
    (65, "Data error: an input file is invalid or corrupt."),
    (69, "A resource or service required to run is unavailable."),
    (73, "Failed to create the report or project file."),
    (74, "I/O error, for example an output file exists and /force was not given."),
    (77, "Permission denied."),
    (79, "With /assertidentical: the databases are not identical."),
    (126, "Execution failed because of an error."),
    (130, "Execution stopped because of Ctrl+C or Ctrl+Break."),
    (500, "An unhandled error occurred; its message is printed."),
];

fn switch_line(s: &Switch, r: Option<&Rule>) -> String {
    let mut line = format!("/{}", s.name);
    if s.takes_value {
        line.push_str(&format!(":<{}>", s.name));
    }
    if let Some(a) = s.aliases.first() {
        line.push_str(&format!(" (Alias: /{a})"));
    }
    if let Some(d) = DEFAULTS.iter().find(|(n, _)| *n == s.name) {
        line.push_str(&format!(" [Default: {}]", d.1));
    }
    if r.is_some_and(|r| r.deprecated.is_some()) {
        line.push_str(" (deprecated)");
    }
    line
}

const DEFAULTS: &[(&str, &str)] = &[
    ("server1", "(local)"),
    ("server2", "(local)"),
    ("options", "Default"),
    ("datacompareoptions", "Default"),
    ("logLevel", "None"),
    ("abortOnWarnings", "None"),
    ("syncScriptEncoding", "UTF8WithPreamble"),
];

/// Plain-text help in SQL Compare's layout; `/verbose` adds exit codes and values.
pub fn help(cli: &Cli) -> String {
    let w = console::width();
    let verbose = console::is_verbose();
    let mut out = format!("quill {} -\n{}\n\n", cli.command, console::fixed_width(cli.summary, w, "    ", true));
    if verbose {
        out.push_str("Exit Codes\n==========\n");
        for (code, text) in EXIT_CODES {
            out.push_str(&format!("{code:>5} - {text}\n"));
        }
        out.push('\n');
    }
    out.push_str("Options\n=======\n");
    for s in cli.switches {
        let r = rule_of(cli, s.name);
        out.push_str(&switch_line(s, r));
        out.push('\n');
        out.push_str(&console::fixed_width(s.help, w, "    ", true));
        out.push('\n');
        if verbose && let Some(r) = r.filter(|r| !r.values.is_empty()) {
            out.push_str("\n    Values -\n");
            for v in r.values {
                out.push_str(&format!("        {}{}\n", v[0], v.get(1).map(|a| format!(" (Alias: {a})")).unwrap_or_default()));
            }
            out.push('\n');
        }
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `/help /html`.
pub fn help_html(cli: &Cli) -> String {
    let mut rows = String::new();
    for s in cli.switches {
        rows.push_str(&format!("<tr><td><code>{}</code></td><td>{}</td></tr>\n", html_escape(&switch_line(s, rule_of(cli, s.name))), html_escape(s.help)));
    }
    let codes: String = EXIT_CODES.iter().map(|(c, t)| format!("<tr><td>{c}</td><td>{}</td></tr>\n", html_escape(t))).collect();
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>quill {cmd}</title></head><body>\n<h1>quill {cmd}</h1>\n<p>{summary}</p>\n<h2>Options</h2>\n<table>\n{rows}</table>\n<h2>Exit codes</h2>\n<table>\n{codes}</table>\n</body></html>\n",
        cmd = cli.command,
        summary = html_escape(cli.summary),
    )
}

/// Where one side of the comparison comes from.
pub enum Source {
    Live(ConnectionSpec),
    Snapshot(std::path::PathBuf),
    /// A scripts folder, at a source-control revision when one is given.
    Scripts { path: std::path::PathBuf, revision: Option<String> },
}

/// The project named by `/project`, if any.
pub fn project(args: &Args) -> Result<Option<crate::project::Project>, ArgError> {
    let Some(p) = args.get("project") else { return Ok(None) };
    let path = Path::new(p);
    if !path.exists() {
        return Err(ArgError::new(exit::IO_ERROR, format!("The specified project file '{p}' does not exist. Reported error: file not found.")));
    }
    crate::project::Project::load(path)
        .map(Some)
        .map_err(|e| ArgError::new(exit::DATA_ERROR, format!("Project file '{p}' is invalid or corrupt: {e}.")))
}

/// `/project`, refused when it is the other kind (a `.qdc` given to `quill schema`).
pub fn project_of(args: &Args, kind: crate::project::ProjectKind) -> Result<Option<crate::project::Project>, ArgError> {
    let p = project(args)?;
    if let Some(p) = &p
        && p.kind != kind
    {
        let (is, want) = match kind {
            crate::project::ProjectKind::Schema => ("a data compare project", "quill data"),
            crate::project::ProjectKind::Data => ("a schema compare project", "quill schema"),
        };
        return Err(ArgError::new(exit::DATA_ERROR, format!("{} is {is}; open it with {want}", args.get("project").unwrap_or_default())));
    }
    Ok(p)
}

/// `host;Encrypt=True;TrustServerCertificate=True`: SQL Compare reads these two settings from
/// the server name.
fn server_settings(server: &str) -> (String, Option<bool>, Option<bool>) {
    let mut parts = server.split(';');
    let host = parts.next().unwrap_or_default().trim().to_string();
    let (mut encrypt, mut trust) = (None, None);
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            let v = v.trim().eq_ignore_ascii_case("true");
            match k.trim().to_ascii_lowercase().as_str() {
                "encrypt" => encrypt = Some(v),
                "trustservercertificate" => trust = Some(v),
                _ => {}
            }
        }
    }
    (host, encrypt, trust)
}

/// Reads `/serverN`, `/databaseN`... into a source, or `None` when no database N was given.
/// Without them, side N of the `/project` is used.
pub fn source(args: &Args, n: u8) -> Result<Option<Source>, ArgError> {
    let key = |name: &str| format!("{name}{n}");
    if args.get(&key("password")).is_some() && !args.has(&key("username")) && !args.has("project") {
        return Err(ArgError::new(
            exit::UNSATISFIED_ARGUMENT,
            format!("The /password{n} switch cannot be used without the /username{n} or /project switches."),
        ));
    }
    if let Some(s) = super::sources::source(args, n)? {
        return Ok(Some(s));
    }
    let given = ["snapshot", "connection", "database", "server"].iter().any(|k| args.has(&key(k)));
    if !given && let Some(p) = project(args)? {
        let mut side = if n == 1 { p.source } else { p.target };
        if side.is_snapshot() {
            return Ok(Some(Source::Snapshot(side.path.into())));
        }
        if side.is_scripts() {
            let revision = (!side.revision.is_empty()).then(|| side.revision.clone());
            return Ok(Some(Source::Scripts { path: side.path.into(), revision }));
        }
        if side.server.is_empty() && side.database.is_empty() {
            return Ok(None);
        }
        side.password = args.get(&key("password")).map(str::to_string);
        if let Some(u) = args.get(&key("username")) {
            side.user = u.to_string();
            side.authentication = crate::project::Authentication::Sql;
        }
        let spec = side.connection(n).map_err(|e| ArgError::new(exit::UNSATISFIED_ARGUMENT, e))?;
        return Ok(Some(Source::Live(spec)));
    }
    if let Some(path) = args.get(&key("snapshot")) {
        return Ok(Some(Source::Snapshot(path.into())));
    }
    let connection = args.get(&key("connection"));
    let Some(database) = args.get(&key("database")).or(connection.map(|_| "")) else {
        return Ok(None);
    };
    if database.trim().is_empty() && connection.is_none() && n == 1 {
        return Err(ArgError::new(exit::USAGE, format!("The /database{n} switch requires a database name.")));
    }
    let (server, encrypt_in_name, trust_in_name) = server_settings(args.get(&key("server")).unwrap_or("(local)"));
    let user = args.get(&key("username"));
    if user.is_some_and(|u| u.chars().count() > 128) {
        return Err(ArgError::new(exit::EXECUTION_FAILED, "User name cannot be more than 128 characters long."));
    }
    let password = args.get(&key("password")).map(str::to_string);
    if password.as_ref().is_some_and(|p| p.chars().count() > 128) {
        return Err(ArgError::new(exit::EXECUTION_FAILED, "Password cannot be more than 128 characters long."));
    }
    let auth = if args.has(&key("activedirectory")) {
        let token = std::env::var(format!("QUILL_ACCESS_TOKEN{n}")).map_err(|_| {
            ArgError::new(
                exit::UNSATISFIED_ARGUMENT,
                format!("/activedirectory{n} needs an access token in QUILL_ACCESS_TOKEN{n} (az account get-access-token --resource https://database.windows.net/)"),
            )
        })?;
        Auth::AccessToken(token)
    } else if let Some(user) = user {
        let password = password
            .or_else(|| std::env::var(format!("QUILL_PASSWORD{n}")).ok())
            .ok_or_else(|| {
                ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("give /password{n} or set QUILL_PASSWORD{n} for /username{n}"))
            })?;
        Auth::Sql { user: user.to_string(), password }
    } else {
        Auth::Windows
    };
    let mut spec = ConnectionSpec::new(&server, database.trim(), auth);
    spec.connection_string = connection.map(str::to_string);
    if let Some(e) = args.get(&key("encrypt")) {
        spec.encrypt = parse_bool(e).ok_or_else(|| ArgError::new(exit::INVALID_VALUE, format!("/encrypt{n}: '{e}' is not true or false")))?;
    } else if let Some(e) = encrypt_in_name {
        spec.encrypt = e;
    }
    spec.trust_server_certificate = args.has(&key("trustServerCertificate")) || trust_in_name == Some(true);
    Ok(Some(Source::Live(spec)))
}

pub fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Some(true),
        "false" | "no" | "0" | "off" => Some(false),
        _ => None,
    }
}

/// Refuses to replace an existing file unless /force was given.
pub fn check_output(path: &Path, args: &Args) -> Result<(), ArgError> {
    if path.exists() && !args.has("force") {
        return Err(ArgError::new(exit::IO_ERROR, format!("{} exists; add /force to overwrite it", path.display())));
    }
    Ok(())
}

/// Prints an error the way the command line reports it and returns its exit code.
pub fn fail(e: ArgError) -> u8 {
    if !e.message.is_empty() {
        console::error(&e.message);
    }
    e.code
}

/// The script in the `/syncScriptEncoding`.
pub fn encode_script(sql: &str, encoding: &str) -> Vec<u8> {
    match encoding.to_ascii_lowercase().as_str() {
        "utf8" => sql.as_bytes().to_vec(),
        "unicode" => [0xFF, 0xFE].into_iter().chain(sql.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        "ascii" => sql.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect(),
        _ => [&[0xEF, 0xBB, 0xBF][..], sql.as_bytes()].concat(),
    }
}

pub fn write_script(args: &Args, script: &deploy::DeploymentScript) -> Result<(), ArgError> {
    let Some(name) = args.get("scriptfile") else { return Ok(()) };
    let path = Path::new(name);
    if path.exists() && !args.has("force") {
        return Err(ArgError::new(exit::IO_ERROR, format!("Unable to write migration script to file '{name}' because the file already exists.")));
    }
    // CRLF line ends, as SQL Compare writes them.
    let sql = script.sql.replace("\r\n", "\n").replace('\n', "\r\n");
    let bytes = encode_script(&sql, args.get("syncScriptEncoding").unwrap_or("UTF8WithPreamble"));
    std::fs::write(path, bytes).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{name}: {e}")))?;
    console::verbose(&format!("Deployment script written to {name}."));
    Ok(())
}

/// `/showWarnings`: a `Warnings` block.
pub fn report_warnings(args: &Args, script: &deploy::DeploymentScript) {
    if script.warnings.is_empty() || !args.has("showWarnings") {
        return;
    }
    console::log("");
    console::log("Warnings");
    console::log(&"=".repeat(console::width() - 1));
    for w in &script.warnings {
        let object = if w.object.is_empty() { String::new() } else { format!("{}: ", w.object) };
        console::warn(&format!("({:?}) {object}{}", w.severity, w.message));
        console::log("");
    }
}

/// `/abortOnWarnings`: High stops on High warnings, Medium on Medium or High.
pub fn check_warnings(args: &Args, script: &deploy::DeploymentScript) -> Result<(), ArgError> {
    let level = match enums::canonical(enums::WARNING_LEVELS, args.get("abortOnWarnings").unwrap_or("None")).ok().and_then(|v| v.first().copied()) {
        None | Some("None") => return Ok(()),
        Some("High") => deploy::Severity::High,
        _ => deploy::Severity::Medium,
    };
    if let Some(w) = script.warnings.iter().find(|w| w.severity >= level) {
        let object = if w.object.is_empty() { String::new() } else { format!("{}: ", w.object) };
        return Err(ArgError::new(exit::ABORTED_ON_WARNINGS, format!("Aborting due to {:?} severity warning: {object}{}", w.severity, w.message)));
    }
    Ok(())
}

/// Reads one side of the comparison; `decrypt` reads encrypted modules over the admin connection.
pub fn load(source: &Source, _args: &Args, decrypt: bool) -> Result<Database, ArgError> {
    match source {
        Source::Live(spec) => {
            console::verbose(&format!("Connecting to database '{}' on '{}'...", spec.database, spec.server));
            catalog::read_database_with(spec, decrypt).map_err(|e| ArgError::new(exit::EXECUTION_FAILED, format!("{}: {e}", spec.describe())))
        }
        Source::Snapshot(path) => {
            console::verbose(&format!("Loading database snapshot from file: {}...", path.display()));
            load_snapshot(path)
        }
        Source::Scripts { path, revision } => {
            super::sources::load(path, revision.as_deref(), _args, &crate::schema::Options::default_set()).map(|(db, _)| db)
        }
    }
}

/// Writes a SQL Compare snapshot (`.snp`), or QuillSQL's JSON model for a `.json` path.
pub fn save_snapshot(db: &Database, path: &Path) -> Result<(), ArgError> {
    let bytes = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        serde_json::to_vec_pretty(db).map_err(|e| ArgError::new(exit::GENERAL_ERROR, e.to_string()))?
    } else {
        crate::sources::snp_write::write(db)
    };
    std::fs::write(path, bytes).map_err(|e| ArgError::new(exit::IO_ERROR, format!("Error creating snapshot: {}: {e}", path.display())))
}

pub fn load_snapshot(path: &Path) -> Result<Database, ArgError> {
    let bytes = std::fs::read(path).map_err(|e| {
        ArgError::new(exit::IO_ERROR, format!("The specified snapshot file '{}' does not exist.  Reported error: {e}", path.display()))
    })?;
    if crate::sources::snp::is_snp(&bytes) {
        return crate::sources::snp::read(&bytes).map_err(|e| ArgError::new(exit::DATA_ERROR, format!("{}: {e}", path.display())));
    }
    let db: Database = serde_json::from_slice(crate::project::without_bom(&bytes)).map_err(|e| {
        ArgError::new(
            exit::DATA_ERROR,
            format!("Snapshot could not be loaded from file '{}'.  Either the file has been corrupted, is not a valid snapshot file, or cannot be accessed.  Reported error: {e}", path.display()),
        )
    })?;
    if db.model_version > catalog::MODEL_VERSION {
        return Err(ArgError::new(exit::DATA_ERROR, format!("The snapshot {} was created using a newer version of QuillSQL.", path.display())));
    }
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn switches() -> Vec<Switch> {
        GENERAL.into_iter().chain(SOURCE1).chain(SOURCE2).chain(DEPLOY).collect()
    }

    fn check(a: &[&str]) -> Result<Args, ArgError> {
        let mut args = args::parse(&switches(), &argv(a))?;
        args::validate(&mut args, &shared_rules(), &["database1"], DEFAULTED)?;
        Ok(args)
    }

    #[test]
    fn shared_rules_follow_sql_compare() {
        assert_eq!(check(&["/s1:x"]).unwrap_err().code, exit::USAGE);
        assert_eq!(check(&["/db1:x", "/s2:y"]).unwrap_err().code, exit::UNSATISFIED_ARGUMENT);
        assert_eq!(check(&["/db1:x", "/e2", "/db2:y"]).unwrap_err().code, exit::UNSATISFIED_ARGUMENT);
        assert_eq!(check(&["/db1:x", "/e2", "/sync"]).unwrap_err().code, exit::UNSATISFIED_ARGUMENT);
        assert_eq!(check(&["/db1:x", "/senc:UTF8"]).unwrap_err().code, exit::UNSATISFIED_ARGUMENT);
        assert_eq!(check(&["/db1:x", "/sf:a.sql", "/senc:utf16"]).unwrap_err().code, exit::INVALID_VALUE);
        assert_eq!(check(&["/db1:x", "/aow:m"]).unwrap().get("abortOnWarnings"), Some("Medium"));
        assert_eq!(check(&["/db1:x", "/log:v"]).unwrap().get("logLevel"), Some("Verbose"));
        assert_eq!(check(&["/db1:x", "/s1:y", "/synch", "/db2:z"]).unwrap().get("server1"), Some("y"));
        assert!(SOURCE1.iter().chain(&SOURCE2).all(|s| s.name.ends_with(['1', '2']) || s.name == "scriptsfolderxml"));
    }

    #[test]
    fn server_names_may_carry_settings() {
        assert_eq!(server_settings("srv,1433;TrustServerCertificate=True;Encrypt=false"), ("srv,1433".into(), Some(false), Some(true)));
        assert_eq!(server_settings("srv"), ("srv".into(), None, None));
    }
}
