//! Switches and helpers shared by `quill schema` and `quill data`.

use std::path::Path;

use crate::args::{ArgError, Args, Switch, exit};
use crate::db::{Auth, ConnectionSpec};

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

pub const GENERAL: [Switch; 4] = [
    Switch { name: "help", aliases: &["?"], takes_value: false, repeatable: false, help: "Show this help" },
    Switch { name: "quiet", aliases: &["q"], takes_value: false, repeatable: false, help: "Print errors only" },
    Switch { name: "verbose", aliases: &["v"], takes_value: false, repeatable: false, help: "Print every difference" },
    Switch { name: "force", aliases: &["f"], takes_value: false, repeatable: false, help: "Overwrite output files" },
];

/// Where one side of the comparison comes from.
pub enum Source {
    Live(ConnectionSpec),
    Snapshot(std::path::PathBuf),
}

/// Reads `/serverN`, `/databaseN`... into a source, or `None` when no database N was given.
pub fn source(args: &Args, n: u8) -> Result<Option<Source>, ArgError> {
    let key = |name: &str| format!("{name}{n}");
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
