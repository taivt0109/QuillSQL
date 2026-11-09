//! `/include` and `/exclude` rules: `type[:regex]` where type is an object type, `All`, or a
//! difference kind (`Identical`, `Different`, `Missing` = only in database 1, `Additional` =
//! only in database 2). Excludes win over includes.

use regex::{Regex, RegexBuilder};

use super::compare::{Comparison, DiffKind, Difference, ObjectType};

#[derive(Debug)]
enum Rule {
    Kind(DiffKind),
    Object { ty: Option<ObjectType>, pattern: Option<Regex> },
    /// A classic compare tool object type QuillSQL does not compare (`Assembly`): matches nothing.
    Unknown,
}

#[derive(Debug, Default)]
pub struct Selection {
    includes: Vec<Rule>,
    excludes: Vec<Rule>,
    /// `/include:Identical`: identical objects are listed and do not make the run exit with 63.
    pub include_identical: bool,
}

fn parse_rule(spec: &str) -> Result<Vec<Rule>, String> {
    let (ty, pattern) = match spec.split_once(':') {
        Some((t, p)) => (t.trim(), Some(p)),
        None => (spec.trim(), None),
    };
    let pattern = pattern
        .map(|p| RegexBuilder::new(p).case_insensitive(true).build().map_err(|e| format!("'{p}': {e}")))
        .transpose()?;
    // A comma list of kinds: /include:Different,Missing
    let mut rules = Vec::new();
    for part in ty.split(',') {
        let lower = part.trim().to_ascii_lowercase();
        let kind = match lower.as_str() {
            "identical" => Some(DiffKind::Equal),
            "different" => Some(DiffKind::Different),
            "missing" => Some(DiffKind::OnlyIn1),
            "additional" => Some(DiffKind::OnlyIn2),
            _ => None,
        };
        if let Some(k) = kind {
            rules.push(Rule::Kind(k));
        } else if lower == "all" || lower == "*" {
            rules.push(Rule::Object { ty: None, pattern: pattern.clone() });
        } else if lower == "staticdata" {
            continue;
        } else if let Some(t) = ObjectType::parse(part) {
            rules.push(Rule::Object { ty: Some(t), pattern: pattern.clone() });
        } else if crate::commands::enums::SCHEMA_OBJECT_TYPES.iter().any(|n| n.eq_ignore_ascii_case(part.trim())) {
            rules.push(Rule::Unknown);
        } else {
            return Err(format!("'{part}' is not an object type or difference type"));
        }
    }
    Ok(rules)
}

impl Selection {
    pub fn new(includes: &[String], excludes: &[String]) -> Result<Selection, String> {
        let mut s = Selection::default();
        for i in includes {
            for r in parse_rule(i)? {
                match r {
                    Rule::Kind(DiffKind::Equal) => s.include_identical = true,
                    r => s.includes.push(r),
                }
            }
        }
        for e in excludes {
            s.excludes.extend(parse_rule(e)?);
        }
        Ok(s)
    }

    fn matches(rule: &Rule, d: &Difference) -> bool {
        match rule {
            Rule::Kind(k) => d.kind == *k,
            Rule::Unknown => false,
            Rule::Object { ty, pattern } => {
                ty.is_none_or(|t| t == d.object_type)
                    && pattern.as_ref().is_none_or(|p| {
                        let names = [&d.name, d.target_name.as_ref().unwrap_or(&d.name)];
                        names.iter().any(|n| {
                            p.is_match(&n.to_string()) || p.is_match(&format!("{}.{}", n.schema, n.name)) || p.is_match(&n.name)
                        })
                    })
            }
        }
    }

    /// Whether the difference is part of the run (listed, and deployed when not identical).
    pub fn includes(&self, d: &Difference) -> bool {
        if self.excludes.iter().any(|r| Self::matches(r, d)) {
            return false;
        }
        let kinds: Vec<&Rule> = self.includes.iter().filter(|r| matches!(r, Rule::Kind(_))).collect();
        let objects: Vec<&Rule> = self.includes.iter().filter(|r| !matches!(r, Rule::Kind(_))).collect();
        (kinds.is_empty() || d.kind == DiffKind::Equal || kinds.iter().any(|r| Self::matches(r, d)))
            && (objects.is_empty() || objects.iter().any(|r| Self::matches(r, d)))
    }

    /// Deselects what the rules leave out; returns the differences that are in the run.
    pub fn apply<'a>(&self, c: &'a mut Comparison) -> Vec<&'a Difference> {
        for d in &mut c.differences {
            if !self.includes(d) {
                d.selected = false;
            }
        }
        c.differences
            .iter()
            .filter(|d| self.includes(d) && (d.kind != DiffKind::Equal || self.include_identical))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ObjectName;

    fn diff(ty: ObjectType, schema: &str, name: &str, kind: DiffKind) -> Difference {
        Difference {
            object_type: ty,
            name: ObjectName::new(schema, name),
            target_name: None,
            kind,
            properties: Vec::new(),
            selected: kind != DiffKind::Equal,
            trivial: false,
        }
    }

    #[test]
    fn include_and_exclude() {
        let t = diff(ObjectType::Table, "dbo", "Customer", DiffKind::Different);
        let v = diff(ObjectType::View, "dbo", "CustomerView", DiffKind::OnlyIn1);
        let p = diff(ObjectType::StoredProcedure, "audit", "Log", DiffKind::OnlyIn2);
        let s = Selection::new(&["Table".into()], &[]).unwrap();
        assert!(s.includes(&t) && !s.includes(&v));
        let s = Selection::new(&[], &["View".into(), "StoredProcedure:^\\[audit\\]".into()]).unwrap();
        assert!(s.includes(&t) && !s.includes(&v) && !s.includes(&p));
        let s = Selection::new(&["Missing".into()], &[]).unwrap();
        assert!(!s.includes(&t) && s.includes(&v) && !s.includes(&p));
        let s = Selection::new(&["All:customer".into()], &["Additional".into()]).unwrap();
        assert!(s.includes(&t) && s.includes(&v) && !s.includes(&p));
        assert!(Selection::new(&["Gadget".into()], &[]).is_err());
        assert!(Selection::new(&["Identical".into()], &[]).unwrap().include_identical);
        let s = Selection::new(&["Assembly".into()], &[]).unwrap();
        assert!(!s.includes(&t), "a type QuillSQL does not compare matches nothing");
        assert!(Selection::new(&[], &["XmlSchemaCollection".into()]).unwrap().includes(&t));
    }
}
