//! The suggestions the user picks, so those picked often and lately come first among equally good
//! matches. Kept in `%LOCALAPPDATA%\QuillSQL\usage.json`. Keywords, built-in functions, types and
//! snippets count everywhere; a database's objects and columns count in that database only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A pick counts half as much after this many seconds (30 days).
const HALF_LIFE: f64 = 30.0 * 24.0 * 3600.0;
/// Entries kept in the file: the highest scores.
const KEEP: usize = 3000;

/// Kinds that mean the same in every database.
const GENERAL: &[&str] = &["keyword", "function", "type", "snippet", "global_variable"];
/// Kinds not worth remembering: written for the script at hand.
const NEVER: &[&str] = &["body", "picker", "join", "join_guess", "alias", "variable", "parameter", "cte", "loading", "value"];

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Entry {
    /// The decayed number of picks as of `at`.
    score: f64,
    /// Seconds since 1970 of the last pick.
    at: f64,
}

#[derive(Debug, Default)]
pub struct Usage {
    path: Option<PathBuf>,
    /// By `database \t kind \t label` (lowercased label; no database for the general kinds).
    entries: HashMap<String, Entry>,
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn decayed(e: &Entry, now: f64) -> f64 {
    e.score * 0.5f64.powf((now - e.at).max(0.0) / HALF_LIFE)
}

fn entry_key(database: &str, kind: &str, label: &str) -> Option<String> {
    if NEVER.contains(&kind) || label.is_empty() {
        return None;
    }
    let database = if GENERAL.contains(&kind) { "" } else { database };
    Some(format!("{database}\t{kind}\t{}", label.to_lowercase()))
}

impl Usage {
    /// The default file, or `QUILL_USAGE_FILE` when set.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("QUILL_USAGE_FILE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("QuillSQL").join("usage.json")))
    }

    pub fn load(path: Option<PathBuf>) -> Usage {
        let entries = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Usage { path, entries }
    }

    /// Notes a pick of `label` of `kind` in the database `database` (the connection key).
    pub fn record(&mut self, database: &str, kind: &str, label: &str, now: f64) {
        let Some(key) = entry_key(database, kind, label) else { return };
        let e = self.entries.entry(key).or_default();
        *e = Entry { score: decayed(e, now) + 1.0, at: now };
        if self.entries.len() > KEEP {
            let mut scores: Vec<(String, f64)> = self.entries.iter().map(|(k, e)| (k.clone(), decayed(e, now))).collect();
            scores.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (k, _) in scores.into_iter().skip(KEEP) {
                self.entries.remove(&k);
            }
        }
        self.save();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How much each `kind \t label` counts in `database`, in tenths of a pick.
    pub fn scores(&self, database: &str, now: f64) -> HashMap<String, u32> {
        let mut out = HashMap::new();
        for (k, e) in &self.entries {
            let mut parts = k.splitn(2, '\t');
            let (Some(db), Some(rest)) = (parts.next(), parts.next()) else { continue };
            if !db.is_empty() && db != database {
                continue;
            }
            let score = (decayed(e, now) * 10.0).round() as u32;
            if score > 0 {
                out.insert(rest.to_string(), score);
            }
        }
        out
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = Path::new(path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(&self.entries) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// The key [`Usage::scores`] gives an item under.
pub fn item_key(kind: &str, label: &str) -> String {
    format!("{kind}\t{}", label.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_count_and_fade() {
        let mut u = Usage::load(None);
        let t = 1_000_000_000.0;
        u.record("srv|Shop", "keyword", "SET", t);
        u.record("srv|Shop", "keyword", "SET", t);
        u.record("srv|Shop", "table", "Orders", t);
        let s = u.scores("srv|Other", t);
        // Keywords count in every database, tables only in their own.
        assert_eq!(s.get(&item_key("keyword", "set")), Some(&20));
        assert_eq!(s.get(&item_key("table", "orders")), None);
        assert_eq!(u.scores("srv|Shop", t).get(&item_key("table", "Orders")), Some(&10));
        // Half as much a month later.
        assert_eq!(u.scores("srv|Shop", t + HALF_LIFE).get(&item_key("keyword", "SET")), Some(&10));
        // What the script itself names is not remembered.
        u.record("srv|Shop", "variable", "@x", t);
        assert_eq!(u.len(), 2);
    }
}
