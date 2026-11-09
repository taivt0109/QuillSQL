//! Built-in T-SQL functions: their parameters, what they return and what they do. SQL Server keeps
//! no catalog of them (DATEADD is not in sys.all_objects), so they come from `data/functions.json`,
//! built into the program, and from `%LOCALAPPDATA%\QuillSQL\functions.json` when it exists, whose
//! entries add functions or replace the built-in ones of the same name.

use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Function {
    pub name: String,
    /// Each parameter as it is shown: `datepart`, `[start_location]` when optional.
    #[serde(default)]
    pub params: Vec<String>,
    #[serde(default)]
    pub returns: String,
    /// What the function does, in a sentence.
    #[serde(default)]
    pub what: String,
}

static ALL: OnceLock<Vec<Function>> = OnceLock::new();

/// Every known function, by name.
pub fn all() -> &'static [Function] {
    ALL.get_or_init(|| {
        let mut list: Vec<Function> = serde_json::from_str(include_str!("data/functions.json")).expect("data/functions.json is valid");
        if let Some(path) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("QuillSQL").join("functions.json"))
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            match serde_json::from_str::<Vec<Function>>(&text) {
                Ok(own) => {
                    for mut f in own {
                        f.name = f.name.to_ascii_uppercase();
                        list.retain(|g| g.name != f.name);
                        list.push(f);
                    }
                }
                Err(e) => eprintln!("{}: {e}", path.display()),
            }
        }
        for f in &mut list {
            f.name = f.name.to_ascii_uppercase();
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list.dedup_by(|a, b| a.name == b.name);
        list
    })
}

/// The function named `name`, in any case.
pub fn find(name: &str) -> Option<&'static Function> {
    let upper = name.to_ascii_uppercase();
    let list = all();
    list.binary_search_by(|f| f.name.as_str().cmp(&upper)).ok().map(|i| &list[i])
}

/// The text beside the suggestion list: `DATEADD(datepart, number, date)`, what it returns, and
/// what it does.
pub fn describe(name: &str) -> Option<String> {
    let f = find(name)?;
    let mut out = format!("{}({})", f.name, f.params.join(", "));
    if !f.returns.is_empty() {
        out.push_str(&format!("\nreturns {}", f.returns));
    }
    if !f.what.is_empty() {
        out.push_str(&format!("\n\n{}", f.what));
    }
    Some(out)
}
