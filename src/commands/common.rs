//! Switches and helpers shared by `quill schema` and `quill data`.

use std::path::Path;

use crate::args::{ArgError, Args, Switch, exit};
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
        ]
    };
}

pub const SOURCE1: [Switch; 9] = source_switches!("1");
pub const SOURCE2: [Switch; 9] = source_switches!("2");

/// Switches for writing and running a deployment script.
pub const DEPLOY: [Switch; 6] = [
    Switch { name: "scriptfile", aliases: &["sf"], takes_value: true, repeatable: false, help: "Write the deployment script to a file" },
    Switch { name: "synchronize", aliases: &["sync"], takes_value: false, repeatable: false, help: "Deploy the differences to database 2" },
    Switch {
        name: "abortOnWarnings",
        aliases: &["aow"],
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
        aliases: &["senc"],
        takes_value: true,
        repeatable: false,
        help: "UTF8WithPreamble (default), UTF8, Unicode or ASCII",
    },
];

pub const GENERAL: [Switch; 5] = [
    Switch { name: "help", aliases: &["?"], takes_value: false, repeatable: false, help: "Show this help" },
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
];

/// Where one side of the comparison comes from.
pub enum Source {
    Live(ConnectionSpec),
    Snapshot(std::path::PathBuf),
}

/// The project named by `/project`, if any.
pub fn project(args: &Args) -> Result<Option<crate::project::Project>, ArgError> {
    args.get("project")
        .map(|p| crate::project::Project::load(Path::new(p)).map_err(|e| ArgError::new(exit::DATA_ERROR, e)))
        .transpose()
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

/// Reads `/serverN`, `/databaseN`... into a source, or `None` when no database N was given.
/// Without them, side N of the `/project` is used.
pub fn source(args: &Args, n: u8) -> Result<Option<Source>, ArgError> {
    let key = |name: &str| format!("{name}{n}");
    let given = ["snapshot", "connection", "database", "server"].iter().any(|k| args.has(&key(k)));
    if !given && let Some(p) = project(args)? {
        let mut side = if n == 1 { p.source } else { p.target };
        if side.is_snapshot() {
            return Ok(Some(Source::Snapshot(side.path.into())));
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
    let server = args.get(&key("server")).unwrap_or("(local)");
    let user = args.get(&key("username"));
    let password = args.get(&key("password")).map(str::to_string);
    if password.is_some() && user.is_none() {
        return Err(ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("/password{n} needs /username{n}")));
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
    let mut spec = ConnectionSpec::new(server, database, auth);
    spec.connection_string = connection.map(str::to_string);
    if let Some(e) = args.get(&key("encrypt")) {
        spec.encrypt = parse_bool(e).ok_or_else(|| ArgError::new(exit::INVALID_VALUE, format!("/encrypt{n}: '{e}' is not true or false")))?;
    }
    spec.trust_server_certificate = args.has(&key("trustServerCertificate"));
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
    eprintln!("error: {}", e.message);
    e.code
}

pub fn write_script(args: &Args, script: &deploy::DeploymentScript) -> Result<(), ArgError> {
    let Some(path) = args.get("scriptfile") else { return Ok(()) };
    let path = Path::new(path);
    check_output(path, args)?;
    let bytes = match args.get("syncScriptEncoding").unwrap_or("UTF8WithPreamble").to_ascii_lowercase().as_str() {
        "utf8withpreamble" => [&[0xEF, 0xBB, 0xBF][..], script.sql.as_bytes()].concat(),
        "utf8" => script.sql.as_bytes().to_vec(),
        "unicode" => [0xFF, 0xFE].into_iter().chain(script.sql.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        "ascii" => script.sql.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect(),
        other => return Err(ArgError::new(exit::INVALID_VALUE, format!("/syncScriptEncoding: unknown encoding '{other}'"))),
    };
    std::fs::write(path, bytes).map_err(|e| ArgError::new(exit::IO_ERROR, format!("{}: {e}", path.display())))?;
    if !args.has("quiet") {
        println!("Deployment script written to {}.", path.display());
    }
    Ok(())
}

pub fn report_warnings(args: &Args, script: &deploy::DeploymentScript) {
    if script.warnings.is_empty() || args.has("quiet") {
        return;
    }
    if args.has("showWarnings") || args.has("verbose") {
        for w in &script.warnings {
            let object = if w.object.is_empty() { String::new() } else { format!("{}: ", w.object) };
            println!("({:?}) {object}{}", w.severity, w.message);
        }
    } else {
        println!("{} deployment warning(s); add /showWarnings to list them.", script.warnings.len());
    }
}

/// `/abortOnWarnings`: High stops on High warnings, Medium on Medium or High.
pub fn check_warnings(args: &Args, script: &deploy::DeploymentScript) -> Result<(), ArgError> {
    let level = match args.get("abortOnWarnings").map(str::to_ascii_lowercase).as_deref() {
        None | Some("none") => return Ok(()),
        Some("high") => deploy::Severity::High,
        Some("medium") | Some("true") => deploy::Severity::Medium,
        Some(other) => return Err(ArgError::new(exit::INVALID_VALUE, format!("/abortOnWarnings: unknown level '{other}'"))),
    };
    if let Some(w) = script.warnings.iter().find(|w| w.severity >= level) {
        return Err(ArgError::new(
            exit::ABORTED_ON_WARNINGS,
            format!("stopped because of a {:?} warning: {} {}", w.severity, w.object, w.message),
        ));
    }
    Ok(())
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
    let db: Database = serde_json::from_slice(crate::project::without_bom(&bytes))
        .map_err(|e| ArgError::new(exit::DATA_ERROR, format!("{} is not a QuillSQL snapshot: {e}", path.display())))?;
    if db.model_version > catalog::MODEL_VERSION {
        return Err(ArgError::new(
            exit::DATA_ERROR,
            format!("{} was made by a newer QuillSQL (format {}); update quill", path.display(), db.model_version),
        ));
    }
    Ok(db)
}
