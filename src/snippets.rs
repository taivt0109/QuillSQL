//! Snippets: a short prefix that expands to a block of SQL.
//!
//! User snippets are JSON files with these fields
//! (`prefix`, `description`, `body`, `placeholders`), so existing ones can be reused.
//! Bodies may contain `$CURSOR$`, `$SELECTEDTEXT$`, `$SELECTIONSTART$`/`$SELECTIONEND$`,
//! `$DATE$`, `$TIME$`, `$USER$`, `$MACHINE$`, `$SERVER$`, `$DBNAME$`, `$PASTE$` and
//! `$name$` placeholders that are replaced by their default value.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Placeholder {
    pub name: String,
    pub default_value: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Snippet {
    pub prefix: String,
    pub description: String,
    pub body: String,
    pub placeholders: Vec<Placeholder>,
}

/// Values for the built-in placeholders, supplied by the editor.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Env {
    pub selected_text: String,
    pub date: String,
    pub time: String,
    pub user: String,
    pub machine: String,
    pub server: String,
    pub database: String,
    pub clipboard: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Expansion {
    pub text: String,
    /// UTF-16 offsets into `text` of the selection to leave; equal when it is just a caret.
    pub select_start: usize,
    pub select_end: usize,
}

const BUILTIN: &[(&str, &str, &str)] = &[
    ("ssf", "SELECT * FROM", "SELECT * FROM $CURSOR$"),
    ("st100", "SELECT TOP (100) rows", "SELECT TOP (100) * FROM $CURSOR$"),
    ("scf", "Count rows", "SELECT COUNT(*) FROM $CURSOR$"),
    ("ii", "INSERT INTO", "INSERT INTO $CURSOR$"),
    ("df", "DELETE FROM", "DELETE FROM $table$\nWHERE $CURSOR$"),
    ("ij", "INNER JOIN", "INNER JOIN $CURSOR$"),
    ("lj", "LEFT JOIN", "LEFT JOIN $CURSOR$"),
    ("loj", "LEFT OUTER JOIN", "LEFT OUTER JOIN $CURSOR$"),
    ("rj", "RIGHT JOIN", "RIGHT JOIN $CURSOR$"),
    ("roj", "RIGHT OUTER JOIN", "RIGHT OUTER JOIN $CURSOR$"),
    ("fj", "FULL JOIN", "FULL JOIN $CURSOR$"),
    ("foj", "FULL OUTER JOIN", "FULL OUTER JOIN $CURSOR$"),
    ("cj", "CROSS JOIN", "CROSS JOIN $CURSOR$"),
    ("gb", "GROUP BY", "GROUP BY $CURSOR$"),
    ("ob", "ORDER BY", "ORDER BY $CURSOR$"),
    ("is", "INFORMATION_SCHEMA", "INFORMATION_SCHEMA.$CURSOR$"),
    ("af", "ALTER FUNCTION", "ALTER FUNCTION $CURSOR$"),
    ("ap", "ALTER PROCEDURE", "ALTER PROCEDURE $CURSOR$"),
    ("at", "ALTER TABLE", "ALTER TABLE $CURSOR$"),
    ("ata", "Add a column", "ALTER TABLE $table$ ADD $column$ INT NULL$CURSOR$;"),
    ("atac", "Change a column", "ALTER TABLE $table$ ALTER COLUMN $CURSOR$;"),
    ("atd", "Drop a column", "ALTER TABLE $table$ DROP COLUMN $CURSOR$;"),
    ("atdc", "Drop a constraint", "ALTER TABLE $table$ DROP CONSTRAINT $CURSOR$;"),
    ("atdt", "Disable a trigger", "ALTER TABLE $table$ DISABLE TRIGGER $CURSOR$;"),
    ("atet", "Enable a trigger", "ALTER TABLE $table$ ENABLE TRIGGER $CURSOR$;"),
    ("atdta", "Disable all triggers on a table", "ALTER TABLE $CURSOR$ DISABLE TRIGGER ALL;"),
    ("ateta", "Enable all triggers on a table", "ALTER TABLE $CURSOR$ ENABLE TRIGGER ALL;"),
    ("be", "BEGIN...END block", "BEGIN\n    $SELECTEDTEXT$$CURSOR$\nEND"),
    ("bt", "Begin a transaction", "BEGIN TRANSACTION;\n$CURSOR$"),
    ("ctr", "Commit the transaction", "COMMIT TRANSACTION;$CURSOR$"),
    ("rt", "Roll back the transaction", "ROLLBACK TRANSACTION;$CURSOR$"),
    ("tc", "TRY...CATCH block", "BEGIN TRY\n    $SELECTEDTEXT$$CURSOR$\nEND TRY\nBEGIN CATCH\n    THROW;\nEND CATCH;"),
    ("ifs", "IF block", "IF $condition$\nBEGIN\n    $SELECTEDTEXT$$CURSOR$\nEND"),
    ("cte", "Common table expression", "WITH $cte_name$ AS\n(\n    $CURSOR$\n)\nSELECT * FROM $cte_name$;"),
    ("ct", "Create a table", "CREATE TABLE $table$\n(\n    Id INT NOT NULL IDENTITY(1, 1) PRIMARY KEY,\n    $CURSOR$\n);"),
    ("cv", "Create a view", "CREATE VIEW $view$\nAS\nSELECT $CURSOR$;\nGO"),
    ("cp", "Create a stored procedure", "CREATE PROCEDURE $procedure$\n    @parameter INT\nAS\nBEGIN\n    SET NOCOUNT ON;\n    $CURSOR$\nEND;\nGO"),
    ("csf", "Create a scalar function", "CREATE FUNCTION $function$ (@value INT)\nRETURNS INT\nAS\nBEGIN\n    RETURN $CURSOR$@value;\nEND;\nGO"),
    ("citf", "Create an inline table-valued function", "CREATE FUNCTION $function$ (@value INT)\nRETURNS TABLE\nAS\nRETURN\n(\n    SELECT $CURSOR$@value AS Value\n);\nGO"),
    ("ctf", "Create a multi-statement table-valued function", "CREATE FUNCTION $function$ (@value INT)\nRETURNS @result TABLE (Value INT NOT NULL)\nAS\nBEGIN\n    INSERT INTO @result (Value) VALUES ($CURSOR$@value);\n    RETURN;\nEND;\nGO"),
    ("cni", "Create a nonclustered index", "CREATE NONCLUSTERED INDEX $index$ ON $table$ ($CURSOR$);"),
    ("cuni", "Create a unique nonclustered index", "CREATE UNIQUE NONCLUSTERED INDEX $index$ ON $table$ ($CURSOR$);"),
    ("cci", "Create a clustered index", "CREATE CLUSTERED INDEX $index$ ON $table$ ($CURSOR$);"),
    ("cuci", "Create a unique clustered index", "CREATE UNIQUE CLUSTERED INDEX $index$ ON $table$ ($CURSOR$);"),
    ("curff", "Fast-forward read-only cursor", "DECLARE $cursor$ CURSOR LOCAL FAST_FORWARD READ_ONLY FOR\n    $CURSOR$;\nOPEN $cursor$;\nFETCH NEXT FROM $cursor$ INTO @value;\nWHILE @@FETCH_STATUS = 0\nBEGIN\n    FETCH NEXT FROM $cursor$ INTO @value;\nEND;\nCLOSE $cursor$;\nDEALLOCATE $cursor$;"),
    ("cu", "Create a database user", "CREATE USER $user$ FOR LOGIN $CURSOR$;"),
    ("cl", "Create a login", "CREATE LOGIN $login$ WITH PASSWORD = N'$CURSOR$', CHECK_POLICY = ON;"),
    ("cdb", "Create a database", "CREATE DATABASE $database$;$CURSOR$"),
    ("dd", "Drop a database", "DROP DATABASE $CURSOR$;"),
    ("dfn", "Drop a function", "DROP FUNCTION IF EXISTS $CURSOR$;"),
    ("di", "Drop an index", "DROP INDEX $CURSOR$ ON ;"),
    ("dl", "Drop a login", "DROP LOGIN $CURSOR$;"),
    ("dp", "Drop a procedure", "DROP PROCEDURE IF EXISTS $CURSOR$;"),
    ("dt", "Drop a table", "DROP TABLE IF EXISTS $CURSOR$;"),
    ("du", "Drop a user", "DROP USER IF EXISTS $CURSOR$;"),
    ("dv", "Drop a view", "DROP VIEW IF EXISTS $CURSOR$;"),
    ("err", "@@ERROR", "@@ERROR$CURSOR$"),
    ("foff", "Stop formatting from here", "-- quill-format-off\n$CURSOR$"),
    ("fon", "Resume formatting from here", "-- quill-format-on\n$CURSOR$"),
    ("san", "Set ANSI_NULLS", "SET ANSI_NULLS ON;$CURSOR$"),
    ("sqi", "Set QUOTED_IDENTIFIER", "SET QUOTED_IDENTIFIER ON;$CURSOR$"),
    ("sxa", "Set XACT_ABORT", "SET XACT_ABORT ON;$CURSOR$"),
    ("sph", "Run sp_help", "EXEC sp_help '$CURSOR$';"),
    ("spt", "Run sp_helptext", "EXEC sp_helptext '$CURSOR$';"),
    ("w2", "Run sp_who2", "EXEC sp_who2;$CURSOR$"),
    ("xpfd", "Free space on fixed drives", "EXEC master.sys.xp_fixeddrives;$CURSOR$"),
    ("mro", "20 most recently created objects", "SELECT TOP (20) SCHEMA_NAME(o.schema_id) AS SchemaName, o.name, o.type_desc, o.create_date\nFROM sys.objects AS o\nWHERE o.is_ms_shipped = 0\nORDER BY o.create_date DESC;$CURSOR$"),
    ("timings", "Time the selected code", "DECLARE @started DATETIME2 = SYSDATETIME();\n$SELECTEDTEXT$$CURSOR$\nSELECT DATEDIFF(MILLISECOND, @started, SYSDATETIME()) AS ElapsedMs;"),
];

pub fn builtins() -> Vec<Snippet> {
    let mut out: Vec<Snippet> = Vec::new();
    for (prefix, description, body) in BUILTIN {
        if out.iter().any(|s| s.prefix == *prefix) {
            continue;
        }
        let mut placeholders = Vec::new();
        for name in placeholder_names(body) {
            placeholders.push(Placeholder { default_value: name.clone(), name });
        }
        out.push(Snippet { prefix: prefix.to_string(), description: description.to_string(), body: body.to_string(), placeholders });
    }
    out
}

const RESERVED: &[&str] = &[
    "CURSOR", "SELECTEDTEXT", "SELECTIONSTART", "SELECTIONEND", "DATE", "TIME", "USER", "MACHINE",
    "SERVER", "DBNAME", "PASTE",
];

/// `$name$` placeholders in order of first appearance, excluding the built-in ones.
fn placeholder_names(body: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('$') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('$') else { break };
        let name = &after[..close];
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            if !RESERVED.contains(&name) && !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
            rest = &after[close + 1..];
        } else {
            rest = after;
        }
    }
    names
}

/// Loads `*.json` snippets from a folder. Unreadable files are reported, not fatal.
pub fn load_dir(dir: &Path, warnings: &mut Vec<String>) -> Vec<Snippet> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json"))).collect();
    paths.sort();
    for path in paths {
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Snippet>(t.trim_start_matches('\u{feff}')).map_err(|e| e.to_string()));
        match parsed {
            Ok(s) if !s.prefix.is_empty() => out.push(s),
            Ok(_) => warnings.push(format!("{}: no prefix", path.display())),
            Err(e) => warnings.push(format!("{}: {e}", path.display())),
        }
    }
    out
}

/// Later lists override earlier ones with the same prefix.
pub fn merge(lists: Vec<Vec<Snippet>>) -> Vec<Snippet> {
    let mut out: Vec<Snippet> = Vec::new();
    for s in lists.into_iter().flatten() {
        match out.iter_mut().find(|o| o.prefix.eq_ignore_ascii_case(&s.prefix)) {
            Some(existing) => *existing = s,
            None => out.push(s),
        }
    }
    out
}

pub fn find<'a>(snippets: &'a [Snippet], prefix: &str) -> Option<&'a Snippet> {
    snippets.iter().find(|s| s.prefix.eq_ignore_ascii_case(prefix))
}

/// Expands a snippet inserted at a column whose line starts with `indent`.
/// Continuation lines are indented to match, and `nl` is the document's line break.
pub fn expand(snippet: &Snippet, env: &Env, indent: &str, nl: &str) -> Expansion {
    let mut text = String::new();
    let mut caret: Option<usize> = None;
    let mut sel_start: Option<usize> = None;
    let mut sel_end: Option<usize> = None;
    let mut first_placeholder: Option<(usize, usize)> = None;
    let body = snippet.body.replace("\r\n", "\n");
    let mut rest = body.as_str();
    let push = |text: &mut String, s: &str| {
        for (i, line) in s.split('\n').enumerate() {
            if i > 0 {
                text.push_str(nl);
                if !line.is_empty() {
                    text.push_str(indent);
                }
            }
            text.push_str(line);
        }
    };
    while let Some(open) = rest.find('$') {
        push(&mut text, &rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('$');
        let name = close.map(|c| &after[..c]).unwrap_or("");
        let valid = close.is_some() && !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
        if !valid {
            text.push('$');
            rest = after;
            continue;
        }
        let at = text.encode_utf16().count();
        match name {
            "CURSOR" => caret = Some(at),
            "SELECTIONSTART" => sel_start = Some(at),
            "SELECTIONEND" => sel_end = Some(at),
            "SELECTEDTEXT" => push(&mut text, env.selected_text.trim_end_matches(['\r', '\n'])),
            "DATE" => text.push_str(&env.date),
            "TIME" => text.push_str(&env.time),
            "USER" => text.push_str(&env.user),
            "MACHINE" => text.push_str(&env.machine),
            "SERVER" => text.push_str(&env.server),
            "DBNAME" => text.push_str(&env.database),
            "PASTE" => push(&mut text, &env.clipboard),
            _ => match snippet.placeholders.iter().find(|p| p.name == name) {
                Some(p) => {
                    text.push_str(&p.default_value);
                    first_placeholder.get_or_insert((at, text.encode_utf16().count()));
                }
                None => {
                    // Not a placeholder: keep it literally, e.g. a `$` inside a string.
                    text.push('$');
                    text.push_str(name);
                    text.push('$');
                }
            },
        }
        rest = &after[close.expect("valid") + 1..];
    }
    push(&mut text, rest);
    let end = text.encode_utf16().count();
    let (select_start, select_end) = match (sel_start, sel_end, first_placeholder, caret) {
        (Some(s), Some(e), _, _) => (s, e),
        // The first placeholder is selected so it can be typed over.
        (_, _, Some(range), _) => range,
        (_, _, _, Some(c)) => (c, c),
        _ => (end, end),
    };
    Expansion { text, select_start, select_end }
}
