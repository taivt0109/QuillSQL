//! Command-line switches in the style of SQL Compare and SQL Data Compare: `/name:value`,
//! `/name`, and also `--name value`, `--name=value` and `-name:value`. Names are
//! case-insensitive and every switch has its documented aliases, so existing pipeline scripts
//! keep working when `SQLCompare.exe` is replaced with `quill schema`.

use std::collections::HashMap;

/// Exit codes shared with SQL Compare's command line.
pub mod exit {
    pub const SUCCESS: u8 = 0;
    pub const GENERAL_ERROR: u8 = 1;
    pub const DUPLICATE_ARGUMENT: u8 = 3;
    pub const UNSATISFIED_ARGUMENT: u8 = 8;
    pub const INVALID_VALUE: u8 = 34;
    pub const ABORTED_ON_WARNINGS: u8 = 61;
    pub const IDENTICAL: u8 = 63;
    pub const USAGE: u8 = 64;
    pub const DATA_ERROR: u8 = 65;
    pub const REPORT_FAILED: u8 = 73;
    pub const IO_ERROR: u8 = 74;
    pub const NOT_IDENTICAL: u8 = 79;
    pub const EXECUTION_FAILED: u8 = 126;
}

/// One switch the command accepts.
pub struct Switch {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub takes_value: bool,
    /// May be given more than once (`/include`, `/exclude`).
    pub repeatable: bool,
    pub help: &'static str,
}

/// A failed parse, with the exit code SQL Compare would use.
#[derive(Debug, PartialEq, Eq)]
pub struct ArgError {
    pub code: u8,
    pub message: String,
}

impl ArgError {
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        ArgError { code, message: message.into() }
    }
}

/// Parsed switches, keyed by canonical name.
#[derive(Debug, Default)]
pub struct Args {
    values: HashMap<&'static str, Vec<String>>,
}

impl Args {
    pub fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(|v| v.last()).map(String::as_str)
    }

    pub fn all(&self, name: &str) -> &[String] {
        self.values.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// A switch whose value must be present.
    pub fn require(&self, name: &str) -> Result<&str, ArgError> {
        self.get(name).ok_or_else(|| ArgError::new(exit::USAGE, format!("/{name} is required")))
    }
}

pub fn parse(switches: &[Switch], argv: &[String]) -> Result<Args, ArgError> {
    let mut lookup: HashMap<String, &Switch> = HashMap::new();
    for s in switches {
        lookup.insert(s.name.to_ascii_lowercase(), s);
        for a in s.aliases {
            lookup.insert(a.to_ascii_lowercase(), s);
        }
    }
    let mut args = Args::default();
    let mut i = 0;
    while i < argv.len() {
        let raw = &argv[i];
        i += 1;
        let body = raw
            .strip_prefix("--")
            .or_else(|| raw.strip_prefix('/'))
            .or_else(|| raw.strip_prefix('-'))
            .ok_or_else(|| ArgError::new(exit::USAGE, format!("unexpected argument '{raw}'; switches start with /")))?;
        let (name, inline) = match body.find([':', '=']) {
            Some(p) => (&body[..p], Some(body[p + 1..].to_string())),
            None => (body, None),
        };
        let switch = lookup
            .get(&name.to_ascii_lowercase())
            .ok_or_else(|| ArgError::new(exit::USAGE, format!("unknown switch /{name}")))?;
        let value = if switch.takes_value {
            match inline {
                Some(v) => v,
                // `--name value` form.
                None if raw.starts_with("--") && i < argv.len() => {
                    i += 1;
                    argv[i - 1].clone()
                }
                None => return Err(ArgError::new(exit::USAGE, format!("/{} needs a value: /{}:<value>", switch.name, switch.name))),
            }
        } else {
            match inline.as_deref() {
                None | Some("") => String::new(),
                Some(v) => {
                    return Err(ArgError::new(exit::INVALID_VALUE, format!("/{} takes no value, got '{v}'", switch.name)));
                }
            }
        };
        let entry = args.values.entry(switch.name).or_default();
        if !entry.is_empty() && !switch.repeatable {
            return Err(ArgError::new(exit::DUPLICATE_ARGUMENT, format!("/{} is given more than once", switch.name)));
        }
        entry.push(value);
    }
    Ok(args)
}

/// Help text listing every switch.
pub fn usage(command: &str, summary: &str, switches: &[Switch]) -> String {
    let mut out = format!("{summary}\n\nUsage: quill {command} /switch[:value] ...\n\n");
    for s in switches {
        let mut names = format!("/{}", s.name);
        for a in s.aliases {
            names.push_str(&format!(", /{a}"));
        }
        if s.takes_value {
            names.push_str(":<value>");
        }
        out.push_str(&format!("  {names:<44} {}\n", s.help));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SW: &[Switch] = &[
        Switch { name: "server1", aliases: &["s1"], takes_value: true, repeatable: false, help: "" },
        Switch { name: "synchronize", aliases: &["sync"], takes_value: false, repeatable: false, help: "" },
        Switch { name: "include", aliases: &[], takes_value: true, repeatable: true, help: "" },
    ];

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn vendor_and_gnu_forms() {
        let a = parse(SW, &argv(&["/S1:dev,1433", "/sync", "/include:Table:^dbo\\.", "--include", "View"])).unwrap();
        assert_eq!(a.get("server1"), Some("dev,1433"));
        assert!(a.has("synchronize"));
        assert_eq!(a.all("include"), ["Table:^dbo\\.", "View"]);
        let a = parse(SW, &argv(&["--server1=prod"])).unwrap();
        assert_eq!(a.get("server1"), Some("prod"));
    }

    #[test]
    fn errors_use_vendor_exit_codes() {
        assert_eq!(parse(SW, &argv(&["/s1:a", "/server1:b"])).unwrap_err().code, exit::DUPLICATE_ARGUMENT);
        assert_eq!(parse(SW, &argv(&["/nope"])).unwrap_err().code, exit::USAGE);
        assert_eq!(parse(SW, &argv(&["/server1"])).unwrap_err().code, exit::USAGE);
        assert_eq!(parse(SW, &argv(&["/sync:yes"])).unwrap_err().code, exit::INVALID_VALUE);
        assert_eq!(parse(SW, &argv(&["dev"])).unwrap_err().code, exit::USAGE);
    }
}
