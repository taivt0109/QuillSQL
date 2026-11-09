//! Filters as the Compare window edits them: one rule per object type (and `None` for all
//! types), each an Include flag and an expression of the filter language (`filter.rs`), written
//! back as a project's `DifferenceFilter` element or as an `.scpf` file.

use serde::{Deserialize, Serialize};

use super::compare::ObjectType;
use super::filter::{Filter, type_key};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Rule {
    /// `None` (every type) or a `type_key` name.
    #[serde(rename = "type")]
    pub kind: String,
    pub include: bool,
    /// `None` leaves the type out altogether; `TRUE` takes every object.
    pub expression: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FilterModel {
    pub name: String,
    pub case_sensitive: bool,
    pub rules: Vec<Rule>,
}

/// The types a filter has rules for, in the classic compare tool's order, after `None`.
pub fn types() -> Vec<&'static str> {
    std::iter::once("None").chain(ObjectType::ALL.iter().map(|&t| type_key(t))).collect()
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl FilterModel {
    /// Reads a `NamedFilter` (.scpf) or `DifferenceFilter` document.
    pub fn from_xml(text: &str) -> Result<FilterModel, String> {
        let doc = roxmltree::Document::parse(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        let root = doc.root_element();
        let (name, node) = if root.tag_name().name() == "NamedFilter" {
            (child(root, "FilterName").and_then(|n| n.text()).unwrap_or_default().to_string(), child(root, "Filter").ok_or("no Filter element")?)
        } else {
            (String::new(), root)
        };
        let case_sensitive = child(node, "FilterCaseSensitive").and_then(|n| n.text()).is_some_and(|t| t.trim().eq_ignore_ascii_case("true"));
        let mut rules = Vec::new();
        if let Some(filters) = child(node, "Filters") {
            for el in filters.children().filter(|c| c.is_element()) {
                let include = child(el, "Include").and_then(|n| n.text()).is_none_or(|t| !t.trim().eq_ignore_ascii_case("false"));
                let expression = child(el, "Expression").and_then(|n| n.text()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                rules.push(Rule { kind: el.tag_name().name().to_string(), include, expression });
            }
        }
        Ok(FilterModel { name, case_sensitive, rules })
    }

    /// Reads an `.scpf` file (UTF-16 or UTF-8).
    pub fn load(path: &std::path::Path) -> Result<FilterModel, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = if bytes.starts_with(&[0xFF, 0xFE]) {
            let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            String::from_utf16_lossy(&units)
        } else {
            String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)).into_owned()
        };
        let mut m = FilterModel::from_xml(&text).map_err(|e| format!("Filter file '{}' is invalid or corrupt: {e}.", path.display()))?;
        if m.name.is_empty() {
            m.name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        }
        Ok(m)
    }

    fn filters_xml(&self, indent: &str) -> String {
        let mut s = format!("{indent}<FilterCaseSensitive>{}</FilterCaseSensitive>\r\n{indent}<Filters version=\"1\">\r\n", if self.case_sensitive { "True" } else { "False" });
        for t in types() {
            let rule = self.rules.iter().find(|r| r.kind == t);
            let (include, expression) = match rule {
                Some(r) => (r.include, r.expression.clone().unwrap_or_default()),
                None => (true, "TRUE".to_string()),
            };
            s.push_str(&format!(
                "{indent}  <{t} version=\"1\">\r\n{indent}    <Include>{}</Include>\r\n{indent}    <Expression>{}</Expression>\r\n{indent}  </{t}>\r\n",
                if include { "True" } else { "False" },
                esc(&expression)
            ));
        }
        s.push_str(&format!("{indent}</Filters>\r\n"));
        s
    }

    /// The project's `ProjectFilter` element.
    pub fn project_xml(&self) -> String {
        format!("<ProjectFilter version=\"1\" type=\"DifferenceFilter\">\r\n{}</ProjectFilter>", self.filters_xml("  "))
    }

    /// An `.scpf` file as the classic compare tool writes it: UTF-16 with a byte order mark.
    pub fn scpf(&self) -> Vec<u8> {
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"utf-16\" standalone=\"yes\"?>\r\n<!--\r\nQuillSQL Compare\r\nVersion:{}-->\r\n<NamedFilter version=\"1\" type=\"QuillSQLFilter\">\r\n  <!--This filter can be loaded in QuillSQL Compare or used on the command line with the /filter switch-->\r\n  <FilterName>{}</FilterName>\r\n  <Filter version=\"1\" type=\"DifferenceFilter\">\r\n{}  </Filter>\r\n</NamedFilter>\r\n",
            env!("CARGO_PKG_VERSION"),
            esc(&self.name),
            self.filters_xml("    ")
        );
        [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect()
    }

    /// The filter itself, which also checks every expression.
    pub fn filter(&self) -> Result<Filter, String> {
        Filter::from_xml(&self.project_xml())
    }

    /// Whether the filter leaves anything out.
    pub fn is_trivial(&self) -> bool {
        self.rules.iter().all(|r| r.include && r.expression.as_deref() == Some("TRUE"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ObjectName;
    use crate::schema::{DiffKind, Difference};

    fn diff(t: ObjectType, schema: &str, name: &str) -> Difference {
        Difference { object_type: t, name: ObjectName::new(schema, name), target_name: None, kind: DiffKind::Different, properties: Vec::new(), selected: true, trivial: false }
    }

    #[test]
    fn round_trips_through_scpf() {
        let m = FilterModel {
            name: "Sales & more".into(),
            case_sensitive: false,
            rules: vec![
                Rule { kind: "Table".into(), include: false, expression: Some("@SCHEMA = 'audit' OR @NAME LIKE 'tmp%'".into()) },
                Rule { kind: "View".into(), include: true, expression: None },
            ],
        };
        let dir = std::env::temp_dir().join(format!("quill-filter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.scpf");
        std::fs::write(&path, m.scpf()).unwrap();
        let back = FilterModel::load(&path).unwrap();
        assert_eq!(back.name, "Sales & more");
        assert_eq!(back.rules.iter().find(|r| r.kind == "Table"), m.rules.first());
        assert_eq!(back.rules.iter().find(|r| r.kind == "View").unwrap().expression, None);
        let f = back.filter().unwrap();
        assert!(!f.includes(&diff(ObjectType::Table, "audit", "Log")));
        assert!(!f.includes(&diff(ObjectType::Table, "dbo", "tmpX")));
        assert!(f.includes(&diff(ObjectType::Table, "dbo", "Orders")));
        assert!(!f.includes(&diff(ObjectType::View, "dbo", "V")));
        assert!(f.includes(&diff(ObjectType::StoredProcedure, "dbo", "P")));
        assert!(Filter::load(&path).is_ok(), "the CLI reads it too");
        let bad = FilterModel { rules: vec![Rule { kind: "Table".into(), include: true, expression: Some("@NAME LIKE".into()) }], ..Default::default() };
        assert!(bad.filter().is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
