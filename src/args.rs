//! Command-line switches in the style of SQL Compare and SQL Data Compare: `/name:value`,
//! `/name=value`, `/name value`, and the same with `-` or `--`. Names are case-insensitive and
//! every switch has its documented aliases, so existing pipeline scripts keep working when
//! `SQLCompare.exe` is replaced with `quill schema`. Messages and exit codes are SQL Compare's.

use std::collections::HashMap;

/// Exit codes shared with SQL Compare's command line.
pub mod exit {
    pub const SUCCESS: u8 = 0;
    pub const GENERAL_ERROR: u8 = 1;
    pub const DUPLICATE_ARGUMENT: u8 = 3;
    pub const UNSATISFIED_ARGUMENT: u8 = 8;
    /// A flag was given a value.
    pub const DOES_NOT_ACCEPT_VALUE: u8 = 11;
    pub const VALUE_OUT_OF_RANGE: u8 = 32;
    pub const VALUE_OVERFLOW: u8 = 33;
    pub const INVALID_VALUE: u8 = 34;
    /// A source to compare does not exist (a missing scripts folder).
    pub const NOTHING_TO_COMPARE: u8 = 60;
    pub const ABORTED_ON_WARNINGS: u8 = 61;
    /// A scripts folder has syntax errors.
    pub const PARSER_ERROR: u8 = 62;
    pub const IDENTICAL: u8 = 63;
    pub const USAGE: u8 = 64;
    pub const DATA_ERROR: u8 = 65;
    /// An input file (a scripts folder location XML) is missing or cannot be read.
    pub const NO_INPUT: u8 = 66;
    /// A resource or feature the run needs is unavailable.
    pub const UNAVAILABLE: u8 = 69;
    pub const REPORT_FAILED: u8 = 73;
    pub const IO_ERROR: u8 = 74;
    pub const NO_PERMISSION: u8 = 77;
    /// An `/argfile` element names no switch.
    pub const CONFIGURATION: u8 = 78;
    pub const NOT_IDENTICAL: u8 = 79;
    pub const EXECUTION_FAILED: u8 = 126;
    /// Stopped with Ctrl+C or Ctrl+Break.
    pub const CANCELLED: u8 = 130;
    /// An unexpected error (a panic); does not fit in a `u8`, so it is passed to `process::exit`.
    pub const UNHANDLED: i32 = 500;
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

/// Checks on a switch beyond its syntax, run after parsing in SQL Compare's order.
pub struct Rule {
    pub name: &'static str,
    /// Switches that cannot be given with this one.
    pub excludes: &'static [&'static str],
    /// Switches this one needs.
    pub depends: &'static [&'static str],
    /// Allowed values: the canonical name first, then its aliases. Empty = any value.
    pub values: &'static [&'static [&'static str]],
    /// The value is a `,` list of `values`.
    pub list: bool,
    /// Integer range.
    pub range: Option<(i64, i64)>,
    /// A file or folder name, which may not be blank.
    pub path: bool,
    /// Printed as a warning when the switch is used.
    pub deprecated: Option<&'static str>,
}

impl Rule {
    pub const NONE: Rule =
        Rule { name: "", excludes: &[], depends: &[], values: &[], list: false, range: None, path: false, deprecated: None };
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

    /// The canonical names of the switches given.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.values.keys().copied()
    }

    pub fn set(&mut self, name: &'static str, values: Vec<String>) {
        self.values.insert(name, values);
    }

    pub fn remove(&mut self, name: &str) {
        self.values.remove(name);
    }
}

fn lookup(switches: &[Switch]) -> HashMap<String, &Switch> {
    let mut map = HashMap::new();
    for s in switches {
        map.insert(s.name.to_ascii_lowercase(), s);
        for a in s.aliases {
            map.insert(a.to_ascii_lowercase(), s);
        }
    }
    map
}

/// One pair of surrounding quotes is not part of a value.
fn unquote(v: &str) -> String {
    let v = v.strip_prefix(['"', '\'']).unwrap_or(v);
    v.strip_suffix(['"', '\'']).unwrap_or(v).to_string()
}

/// Splits a token into switch name and inline value, or `None` when it is not a switch.
fn split_switch(raw: &str) -> Option<(&str, Option<&str>)> {
    let body = raw.strip_prefix("--").or_else(|| raw.strip_prefix('/')).or_else(|| raw.strip_prefix('-'))?;
    Some(match body.find([':', '=']) {
        Some(p) => (&body[..p], Some(&body[p + 1..])),
        None => (body, None),
    })
}

fn add(args: &mut Args, switch: &Switch, value: String) -> Result<(), ArgError> {
    let entry = args.values.entry(switch.name).or_default();
    if !entry.is_empty() && !switch.repeatable {
        return Err(ArgError::new(exit::DUPLICATE_ARGUMENT, format!("Duplicate switch: /{}.", switch.name)));
    }
    entry.push(value);
    Ok(())
}

/// Reads the switches; syntax errors only (see [`validate`] for the rules).
pub fn parse(switches: &[Switch], argv: &[String]) -> Result<Args, ArgError> {
    let lookup = lookup(switches);
    let mut args = Args::default();
    let mut i = 0;
    while i < argv.len() {
        let raw = &argv[i];
        i += 1;
        let Some((name, inline)) = split_switch(raw) else {
            if raw.contains([':', '=']) {
                return Err(ArgError::new(
                    exit::USAGE,
                    format!("Invalid command-line argument: \"{raw}\". Command-line arguments must begin with a forward slash."),
                ));
            }
            // A stray word, as in SQL Compare, is ignored.
            continue;
        };
        let switch = lookup.get(&name.to_ascii_lowercase()).ok_or_else(|| ArgError::new(exit::USAGE, format!("Invalid command-line argument: /{name}.")))?;
        // `/name value`: the next token, when it is not a switch itself.
        let next_value = |i: usize| argv.get(i).filter(|t| split_switch(t).is_none());
        let value = match (switch.takes_value, inline) {
            (true, Some(v)) => unquote(v),
            (true, None) => match next_value(i) {
                Some(v) => {
                    i += 1;
                    unquote(v)
                }
                None => {
                    return Err(ArgError::new(
                        exit::USAGE,
                        format!("Argument has been specified without value(s): /{}.  You must supply a value or value(s) to this argument.", switch.name),
                    ));
                }
            },
            (false, Some(v)) if !v.is_empty() => {
                return Err(ArgError::new(exit::DOES_NOT_ACCEPT_VALUE, format!("The argument /{} does not accept value(s).", switch.name)));
            }
            (false, _) => {
                if next_value(i).is_some_and(|v| !v.contains([':', '='])) {
                    return Err(ArgError::new(exit::DOES_NOT_ACCEPT_VALUE, format!("The argument /{} does not accept value(s).", switch.name)));
                }
                String::new()
            }
        };
        add(&mut args, switch, value)?;
    }
    Ok(args)
}

/// Runs the rules in SQL Compare's order: at least one of `require_one_of`, exclusions,
/// dependencies, enumerated values (replacing aliases), blank paths, numeric ranges.
/// `defaults` are switches that count as given for dependencies (`/server1` has a default).
pub fn validate(args: &mut Args, rules: &[Rule], require_one_of: &[&str], defaults: &[&str]) -> Result<(), ArgError> {
    fn rules_of<'a>(rules: &'a [Rule], name: &'a str) -> impl Iterator<Item = &'a Rule> + 'a {
        rules.iter().filter(move |r| r.name.eq_ignore_ascii_case(name))
    }
    if !args.has("argfile") && !require_one_of.is_empty() && !require_one_of.iter().any(|n| args.has(n)) {
        let list: Vec<String> = require_one_of.iter().map(|n| format!("/{n}")).collect();
        return Err(ArgError::new(exit::USAGE, format!("You are required to specify at least one of the following parameters: {}.", list.join(", "))));
    }
    let mut given: Vec<&'static str> = args.names().collect();
    given.sort_unstable();
    for name in &given {
        for x in rules_of(rules, name).flat_map(|r| r.excludes) {
            if *x == "*" {
                if let Some(other) = given.iter().find(|g| *g != name && !matches!(**g, "verbose" | "quiet")) {
                    return Err(ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("The /{name} switch cannot be used in conjunction with the /{other} switch.")));
                }
            } else if args.has(x) {
                return Err(ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("The /{name} switch cannot be used in conjunction with the /{x} switch.")));
            }
        }
    }
    for name in &given {
        if let Some(d) = rules_of(rules, name).flat_map(|r| r.depends).find(|d| !args.has(d) && !defaults.contains(d)) {
            return Err(ArgError::new(exit::UNSATISFIED_ARGUMENT, format!("The /{name} switch cannot be used without the /{d} switch.")));
        }
    }
    for (name, r) in given.iter().flat_map(|n| rules_of(rules, n).map(move |r| (n, r))) {
        if !r.values.is_empty() {
            let mut canonical = Vec::new();
            for raw in args.all(name) {
                let parts: Vec<&str> = if r.list { raw.split(',').map(str::trim).filter(|p| !p.is_empty()).collect() } else { vec![raw.trim()] };
                let mut out = Vec::new();
                for p in parts {
                    let v = r
                        .values
                        .iter()
                        .find(|names| names.iter().any(|n| n.eq_ignore_ascii_case(p)))
                        .ok_or_else(|| ArgError::new(exit::INVALID_VALUE, format!("Invalid value for switch /{name}: {p}.")))?;
                    out.push(v[0]);
                }
                canonical.push(out.join(","));
            }
            args.set(name, canonical);
        }
        if r.path && args.all(name).iter().any(|v| v.trim().is_empty()) {
            return Err(ArgError::new(exit::USAGE, format!("Blank or empty path names are not permitted.  Syntax error in argument /{name}.")));
        }
        if let Some((min, max)) = r.range {
            let v = args.get(name).unwrap_or_default().trim();
            let range_error = |code| ArgError::new(code, format!("The specified value for /{name} is invalid: {v}.  Must be in range {min} to {max}."));
            if !v.trim_start_matches(['-', '+']).chars().all(|c| c.is_ascii_digit()) || v.trim_start_matches(['-', '+']).is_empty() {
                return Err(range_error(exit::INVALID_VALUE));
            }
            let n: i32 = v.parse().map_err(|_| {
                ArgError::new(exit::VALUE_OVERFLOW, format!("The value specified for /{name} has caused an overflow error: {v}.  Must be in range {min} to {max}."))
            })?;
            if (n as i64) < min || (n as i64) > max {
                return Err(range_error(exit::VALUE_OUT_OF_RANGE));
            }
        }
    }
    Ok(())
}

/// The deprecation warnings for the switches given.
pub fn deprecations<'a>(args: &Args, rules: &'a [Rule]) -> Vec<&'a str> {
    rules.iter().filter(|r| args.has(r.name)).filter_map(|r| r.deprecated).collect()
}

/// Reads an `/argfile`: `<commandline>` with one element per switch. The file's switches replace
/// the command line's.
pub fn read_argfile(switches: &[Switch], path: &str) -> Result<Args, ArgError> {
    let lookup = lookup(switches);
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ArgError::new(exit::IO_ERROR, format!("Unable to find XML argument file: {path}")),
        std::io::ErrorKind::PermissionDenied => ArgError::new(exit::NO_PERMISSION, format!("Access was denied when trying to access file '{path}': {e}")),
        _ => ArgError::new(exit::IO_ERROR, format!("I/O error trying to reading arguments from file '{path}': {e}")),
    })?;
    let text = crate::project::decode_text(&bytes);
    let doc = roxmltree::Document::parse(&text).map_err(|e| {
        ArgError::new(exit::DATA_ERROR, format!("Unable to execute command because an error occurred whilst parsing the XML file {path}: {e}"))
    })?;
    let root = doc.root_element();
    if root.tag_name().name() != "commandline" {
        return Err(ArgError::new(exit::DATA_ERROR, format!("Unrecognised XML document type (unrecognised root element): {}.", root.tag_name().name())));
    }
    let mut args = Args::default();
    for el in root.children().filter(|c| c.is_element()) {
        let tag = el.tag_name().name();
        let switch = lookup.get(&tag.to_ascii_lowercase()).ok_or_else(|| ArgError::new(exit::CONFIGURATION, format!("Invalid command-line argument: /{tag}.")))?;
        if switch.name == "argfile" {
            return Err(ArgError::new(exit::USAGE, "You may not use the /argfile argument in an XML argument file."));
        }
        let text_of = |n: roxmltree::Node| n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>();
        let value = if matches!(switch.name, "include" | "exclude") && el.has_attribute("objecttype") {
            let ty = el.attribute("objecttype").unwrap_or_default();
            match el.attribute("matches").map(str::trim).filter(|m| !m.is_empty()) {
                Some(m) => format!("{ty}:{m}"),
                None => ty.to_string(),
            }
        } else if el.children().any(|c| c.is_element()) {
            let values: Vec<String> = el.children().filter(|c| c.is_element() && c.tag_name().name() == "value").map(text_of).collect();
            if values.is_empty() {
                return Err(ArgError::new(
                    exit::USAGE,
                    format!("Argument has been specified without value(s): /{}.  You must supply a value or value(s) to this argument.", switch.name),
                ));
            }
            values.join(",")
        } else {
            text_of(el)
        };
        let value = if switch.name == "options" { value.chars().filter(|c| !c.is_whitespace()).collect() } else { value.trim().to_string() };
        if !switch.takes_value && !value.is_empty() && !value.eq_ignore_ascii_case("true") {
            return Err(ArgError::new(exit::DOES_NOT_ACCEPT_VALUE, format!("The argument /{} does not accept value(s).", switch.name)));
        }
        if switch.takes_value && value.is_empty() {
            return Err(ArgError::new(
                exit::USAGE,
                format!("Argument has been specified without value(s): /{}.  You must supply a value or value(s) to this argument.", switch.name),
            ));
        }
        add(&mut args, switch, if switch.takes_value { value } else { String::new() })?;
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
        Switch { name: "database1", aliases: &["db1"], takes_value: true, repeatable: false, help: "" },
        Switch { name: "synchronize", aliases: &["sync"], takes_value: false, repeatable: false, help: "" },
        Switch { name: "include", aliases: &[], takes_value: true, repeatable: true, help: "" },
        Switch { name: "reporttype", aliases: &["rt"], takes_value: true, repeatable: false, help: "" },
        Switch { name: "report", aliases: &["r"], takes_value: true, repeatable: false, help: "" },
        Switch { name: "outputwidth", aliases: &[], takes_value: true, repeatable: false, help: "" },
        Switch { name: "quiet", aliases: &["q"], takes_value: false, repeatable: false, help: "" },
        Switch { name: "verbose", aliases: &["v"], takes_value: false, repeatable: false, help: "" },
        Switch { name: "argfile", aliases: &[], takes_value: true, repeatable: false, help: "" },
        Switch { name: "out", aliases: &[], takes_value: true, repeatable: false, help: "" },
    ];

    const RULES: &[Rule] = &[
        Rule { name: "server1", depends: &["database1"], ..Rule::NONE },
        Rule { name: "reporttype", depends: &["report"], values: &[&["Xml"], &["Html", "interactive"]], ..Rule::NONE },
        Rule { name: "report", path: true, ..Rule::NONE },
        Rule { name: "outputwidth", range: Some((80, 1024)), ..Rule::NONE },
        Rule { name: "quiet", excludes: &["verbose"], ..Rule::NONE },
        Rule { name: "argfile", excludes: &["*"], ..Rule::NONE },
    ];

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn check(a: &[&str]) -> Result<Args, ArgError> {
        let mut args = parse(SW, &argv(a))?;
        validate(&mut args, RULES, &["database1", "argfile"], &[])?;
        Ok(args)
    }

    #[test]
    fn slash_and_gnu_forms() {
        let a = parse(SW, &argv(&["/S1:dev,1433", "/sync", "/include:Table:^dbo\\.", "--include", "View", "-db1=\"Shop\"", "/rt", "Xml"])).unwrap();
        assert_eq!(a.get("server1"), Some("dev,1433"));
        assert!(a.has("synchronize"));
        assert_eq!(a.all("include"), ["Table:^dbo\\.", "View"]);
        assert_eq!((a.get("database1"), a.get("reporttype")), (Some("Shop"), Some("Xml")));
        let a = parse(SW, &argv(&["--server1=prod", "stray", "/out", "x.txt", "/q"])).unwrap();
        assert_eq!((a.get("server1"), a.get("out"), a.has("quiet")), (Some("prod"), Some("x.txt"), true));
    }

    #[test]
    fn syntax_errors_use_compare_exit_codes_and_messages() {
        let e = parse(SW, &argv(&["/s1:a", "/server1:b"])).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (exit::DUPLICATE_ARGUMENT, "Duplicate switch: /server1."));
        let e = parse(SW, &argv(&["/nope"])).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (exit::USAGE, "Invalid command-line argument: /nope."));
        assert_eq!(parse(SW, &argv(&["/server1"])).unwrap_err().code, exit::USAGE);
        assert_eq!(parse(SW, &argv(&["/sync:yes"])).unwrap_err().code, exit::DOES_NOT_ACCEPT_VALUE);
        assert_eq!(parse(SW, &argv(&["/sync", "yes"])).unwrap_err().code, exit::DOES_NOT_ACCEPT_VALUE);
        assert_eq!(parse(SW, &argv(&["db1:x"])).unwrap_err().code, exit::USAGE);
    }

    #[test]
    fn rules_use_compare_exit_codes_and_messages() {
        let e = check(&[]).unwrap_err();
        assert_eq!(e.code, exit::USAGE);
        assert!(e.message.starts_with("You are required to specify at least one of the following parameters: /database1, /argfile"));
        let e = check(&["/s1:x", "/argfile:a.xml"]).unwrap_err();
        assert_eq!(e.code, exit::UNSATISFIED_ARGUMENT);
        let e = check(&["/db1:x", "/q", "/v"]).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (exit::UNSATISFIED_ARGUMENT, "The /quiet switch cannot be used in conjunction with the /verbose switch."));
        let e = check(&["/db1:x", "/rt:Xml"]).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (exit::UNSATISFIED_ARGUMENT, "The /reporttype switch cannot be used without the /report switch."));
        let e = check(&["/db1:x", "/r:a", "/rt:Pdf"]).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (exit::INVALID_VALUE, "Invalid value for switch /reporttype: Pdf."));
        assert_eq!(check(&["/db1:x", "/r:a", "/rt:INTERACTIVE"]).unwrap().get("reporttype"), Some("Html"));
        assert_eq!(check(&["/db1:x", "/r:  "]).unwrap_err().code, exit::USAGE);
        assert_eq!(check(&["/db1:x", "/outputwidth:40"]).unwrap_err().code, exit::VALUE_OUT_OF_RANGE);
        assert_eq!(check(&["/db1:x", "/outputwidth:wide"]).unwrap_err().code, exit::INVALID_VALUE);
        assert_eq!(check(&["/db1:x", "/outputwidth:99999999999"]).unwrap_err().code, exit::VALUE_OVERFLOW);
        assert!(check(&["/db1:x", "/outputwidth:132", "/argfile:a"]).is_err());
        assert!(check(&["/argfile:a", "/v"]).is_ok());
    }

    #[test]
    fn reads_argfiles() {
        let dir = std::env::temp_dir().join(format!("quill-argfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("args.xml");
        std::fs::write(
            &path,
            "<?xml version=\"1.0\"?><commandline><database1>Shop</database1><s1>dev</s1><synchronize/>\
             <include objecttype=\"Table\" matches=\"^dbo\"/><include>View</include><reporttype><value>Xml</value></reporttype></commandline>",
        )
        .unwrap();
        let a = read_argfile(SW, path.to_str().unwrap()).unwrap();
        assert_eq!((a.get("database1"), a.get("server1"), a.has("synchronize")), (Some("Shop"), Some("dev"), true));
        assert_eq!(a.all("include"), ["Table:^dbo", "View"]);
        std::fs::write(&path, "<commandline><bogus/></commandline>").unwrap();
        assert_eq!(read_argfile(SW, path.to_str().unwrap()).unwrap_err().code, exit::CONFIGURATION);
        std::fs::write(&path, "<args/>").unwrap();
        assert_eq!(read_argfile(SW, path.to_str().unwrap()).unwrap_err().code, exit::DATA_ERROR);
        assert_eq!(read_argfile(SW, dir.join("none.xml").to_str().unwrap()).unwrap_err().code, exit::IO_ERROR);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
