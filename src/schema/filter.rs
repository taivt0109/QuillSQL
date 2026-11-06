//! SQL Compare filters: per object type, include or exclude objects by a boolean expression
//! over `@NAME` and `@SCHEMA` (`=`, `!=`, `LIKE`, `NOT LIKE ... ESCAPE`, `AND`, `OR`, `NOT`,
//! parentheses). Read from `.scpf` files (`/filter`) and from the filter section of `.scp`
//! projects.

use std::collections::HashMap;

use regex::{Regex, RegexBuilder};

use super::compare::{Difference, DiffKind, ObjectType};

/// The element name SQL Compare uses for each object type in a filter.
pub fn type_key(t: ObjectType) -> &'static str {
    super::report::type_name(t)
}

/// Object types that belong to a schema; `@SCHEMA` means nothing for the others.
fn has_schema(t: ObjectType) -> bool {
    t.schema_scoped()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum On {
    Name,
    Schema,
}

#[derive(Clone, Debug)]
enum Expr {
    True,
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Eq { on: On, value: String, negate: bool },
    Like { on: On, regex: Regex, negate: bool },
}

/// The object a condition is tested against.
struct Id<'a> {
    schema: &'a str,
    name: &'a str,
    ty: ObjectType,
}

impl Expr {
    /// `None` when the expression says nothing about the object (`@SCHEMA` of a user).
    fn eval(&self, a: Option<&Id>, b: Option<&Id>, case_sensitive: bool) -> Option<bool> {
        match self {
            Expr::True => Some(true),
            Expr::And(list) => {
                if list.is_empty() {
                    return Some(false);
                }
                let mut any = false;
                for e in list {
                    if let Some(v) = e.eval(a, b, case_sensitive) {
                        any = true;
                        if !v {
                            return Some(false);
                        }
                    }
                }
                any.then_some(true)
            }
            Expr::Or(list) => {
                let mut any = false;
                for e in list {
                    if let Some(v) = e.eval(a, b, case_sensitive) {
                        any = true;
                        if v {
                            return Some(true);
                        }
                    }
                }
                any.then_some(false)
            }
            Expr::Not(e) => e.eval(a, b, case_sensitive).map(|v| !v),
            Expr::Eq { .. } | Expr::Like { .. } => match (a, b) {
                (Some(x), None) | (None, Some(x)) => self.test(x, case_sensitive),
                // Either side matching includes the pair.
                (Some(x), Some(y)) => self.test(x, case_sensitive).map(|v| v || self.test(y, case_sensitive).unwrap_or(false)),
                (None, None) => None,
            },
        }
    }

    fn test(&self, id: &Id, case_sensitive: bool) -> Option<bool> {
        let (on, negate) = match self {
            Expr::Eq { on, negate, .. } | Expr::Like { on, negate, .. } => (*on, *negate),
            _ => return None,
        };
        let text = match on {
            On::Name => id.name,
            On::Schema if has_schema(id.ty) => id.schema,
            On::Schema => return None,
        };
        let hit = match self {
            Expr::Eq { value, .. } => if case_sensitive { text == value } else { text.to_lowercase() == value.to_lowercase() },
            Expr::Like { regex, .. } => regex.is_match(text),
            _ => false,
        };
        Some(hit != negate)
    }
}

/// One object type's rule: `filter = None` excludes the type.
#[derive(Clone, Debug)]
struct TypeFilter {
    include: bool,
    filter: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub name: String,
    case_sensitive: bool,
    /// Keyed by `type_key` names, plus `None` for the rule that applies to every type.
    types: HashMap<String, TypeFilter>,
}

impl Default for Filter {
    /// Includes everything.
    fn default() -> Self {
        Filter { name: String::new(), case_sensitive: false, types: HashMap::new() }
    }
}

impl Filter {
    /// Reads a `.scpf` file.
    pub fn load(path: &std::path::Path) -> Result<Filter, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("The specified filter file '{}' does not exist.  Reported error: {e}", path.display()))?;
        Filter::from_xml(&decode(&bytes)).map_err(|e| format!("Filter file '{}' is invalid or corrupt: {e}.", path.display()))
    }

    /// A `NamedFilter` document, or a `DifferenceFilter` element's XML.
    pub fn from_xml(text: &str) -> Result<Filter, String> {
        let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
        let root = doc.root_element();
        let (name, node) = match root.tag_name().name() {
            "NamedFilter" => (
                child(root, "FilterName").and_then(|n| n.text()).unwrap_or_default().to_string(),
                child(root, "Filter").ok_or("no Filter element")?,
            ),
            _ => (String::new(), root),
        };
        let mut f = Filter::from_node(node)?;
        f.name = name;
        Ok(f)
    }

    /// The filter section of a project: `<ProjectFilter type="DifferenceFilter">`.
    pub fn from_node(node: roxmltree::Node) -> Result<Filter, String> {
        let case_sensitive = child(node, "FilterCaseSensitive").and_then(|n| n.text()).is_some_and(|t| t.trim().eq_ignore_ascii_case("true"));
        let mut types = HashMap::new();
        if let Some(filters) = child(node, "Filters") {
            for el in filters.children().filter(|c| c.is_element()) {
                let include = child(el, "Include").and_then(|n| n.text()).is_none_or(|t| !t.trim().eq_ignore_ascii_case("false"));
                let expr = child(el, "Expression").and_then(|n| n.text()).map(str::trim).unwrap_or_default();
                let filter = match expr {
                    "" => None,
                    "TRUE" => Some(Expr::True),
                    e => Some(parse(e, case_sensitive).map_err(|m| format!("The filter expression {e} could not be successfully parsed: {m}"))?),
                };
                types.insert(el.tag_name().name().to_string(), TypeFilter { include, filter });
            }
        }
        Ok(Filter { name: String::new(), case_sensitive, types })
    }

    /// Whether a difference passes the filter: the rule for all types, then the one for its type.
    pub fn includes(&self, d: &Difference) -> bool {
        let n2 = d.target_name.as_ref().unwrap_or(&d.name);
        let id1 = Id { schema: &d.name.schema, name: &d.name.name, ty: d.object_type };
        let id2 = Id { schema: &n2.schema, name: &n2.name, ty: d.object_type };
        let (a, b) = match d.kind {
            DiffKind::OnlyIn1 => (Some(&id1), None),
            DiffKind::OnlyIn2 => (None, Some(&id2)),
            _ => (Some(&id1), Some(&id2)),
        };
        let all = self.types.get("None");
        let own = self.types.get(type_key(d.object_type));
        if let Some(TypeFilter { filter: None, .. }) = own {
            return false;
        }
        let global = all.and_then(|f| f.filter.as_ref().map(|e| (f.include, e.eval(a, b, self.case_sensitive))));
        if let Some((include, Some(v))) = global
            && v != include
        {
            return false;
        }
        match own {
            Some(TypeFilter { include, filter: Some(e) }) => e.eval(a, b, self.case_sensitive).is_none_or(|v| v == *include),
            _ => true,
        }
    }
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) || (bytes.len() > 1 && bytes[1] == 0 && bytes[0] != 0) {
        let start = if bytes.starts_with(&[0xFF, 0xFE]) { 2 } else { 0 };
        let units: Vec<u16> = bytes[start..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)).into_owned()
}

// ---------------------------------------------------------------------------------------------
// The expression language

#[derive(Clone, Debug, PartialEq)]
enum T {
    And,
    Or,
    Not,
    Like,
    Escape,
    Eq,
    Neq,
    Open,
    Close,
    Var(Option<On>),
    Str(String),
}

fn lex(s: &str) -> Result<Vec<T>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '(' {
            out.push(T::Open);
            i += 1;
        } else if ch == ')' {
            out.push(T::Close);
            i += 1;
        } else if ch == '=' {
            out.push(T::Eq);
            i += 1;
        } else if ch == '!' && c.get(i + 1) == Some(&'=') {
            out.push(T::Neq);
            i += 2;
        } else if ch == '\'' {
            // The text keeps doubled quotes; LIKE patterns read them as one quote.
            let mut v = String::new();
            i += 1;
            loop {
                match c.get(i) {
                    None => return Err("unterminated string".into()),
                    Some('\'') if c.get(i + 1) == Some(&'\'') => {
                        v.push_str("''");
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(&x) => {
                        v.push(x);
                        i += 1;
                    }
                }
            }
            out.push(T::Str(v));
        } else if ch.is_alphabetic() || ch == '@' {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let w: String = c[start..i].iter().collect::<String>().to_ascii_lowercase();
            out.push(match w.as_str() {
                "and" => T::And,
                "or" => T::Or,
                "not" => T::Not,
                "like" => T::Like,
                "escape" => T::Escape,
                "@name" => T::Var(Some(On::Name)),
                "@schema" => T::Var(Some(On::Schema)),
                "@type" => T::Var(None),
                _ => return Err(format!("unexpected '{w}'")),
            });
        } else {
            return Err(format!("unexpected '{ch}'"));
        }
    }
    Ok(out)
}

struct P {
    t: Vec<T>,
    i: usize,
    case_sensitive: bool,
}

impl P {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }

    fn next(&mut self) -> Option<T> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut list = vec![self.and()?];
        while self.peek() == Some(&T::Or) {
            self.i += 1;
            list.push(self.and()?);
        }
        Ok(if list.len() == 1 { list.pop().unwrap() } else { Expr::Or(list) })
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut list = vec![self.not()?];
        while self.peek() == Some(&T::And) {
            self.i += 1;
            list.push(self.not()?);
        }
        Ok(if list.len() == 1 { list.pop().unwrap() } else { Expr::And(list) })
    }

    fn not(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&T::Not) {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.atom()
    }

    fn string(&mut self) -> Result<String, String> {
        match self.next() {
            Some(T::Str(s)) => Ok(s),
            _ => Err("expected a string".into()),
        }
    }

    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(T::Open) => {
                let e = self.or()?;
                if self.next() != Some(T::Close) {
                    return Err("expected )".into());
                }
                Ok(e)
            }
            Some(T::Var(on)) => {
                let Some(on) = on else { return Err("@TYPE conditions are not supported in a filter".into()) };
                match self.next() {
                    Some(T::Eq) => Ok(Expr::Eq { on, value: self.string()?, negate: false }),
                    Some(T::Neq) => Ok(Expr::Eq { on, value: self.string()?, negate: true }),
                    Some(T::Like) => self.like(on, false),
                    Some(T::Not) if self.peek() == Some(&T::Like) => {
                        self.i += 1;
                        self.like(on, true)
                    }
                    _ => Err("expected =, !=, LIKE or NOT LIKE".into()),
                }
            }
            _ => Err("expected a condition".into()),
        }
    }

    fn like(&mut self, on: On, negate: bool) -> Result<Expr, String> {
        let pattern = self.string()?;
        let escape = if self.peek() == Some(&T::Escape) {
            self.i += 1;
            let e = self.string()?;
            let mut chars = e.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => return Err("ESCAPE arguments can only be one character long".into()),
            }
        } else {
            None
        };
        let regex = RegexBuilder::new(&like_regex(&pattern, escape))
            .case_insensitive(!self.case_sensitive)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Expr::Like { on, regex, negate })
    }
}

fn parse(s: &str, case_sensitive: bool) -> Result<Expr, String> {
    let mut p = P { t: lex(s)?, i: 0, case_sensitive };
    let e = p.or()?;
    if p.i < p.t.len() {
        return Err("unexpected text after the expression".into());
    }
    Ok(e)
}

/// A LIKE pattern as an anchored regex: `%` any run, `_` one character, `[abc]` one of the
/// characters listed (taken literally, no ranges), `[]` or a lone `[` never match, `''` a quote.
fn like_regex(p: &str, escape: Option<char>) -> String {
    let c: Vec<char> = p.chars().collect();
    let mut out = String::from("^");
    let mut i = 0;
    let lit = |s: &str| regex::escape(s);
    while i < c.len() {
        let ch = c[i];
        if Some(ch) == escape && matches!(c.get(i + 1), Some('%' | '_')) {
            out.push_str(&lit(&c[i + 1].to_string()));
            i += 2;
        } else if ch == '%' {
            out.push_str(".*");
            i += 1;
        } else if ch == '_' {
            out.push('.');
            i += 1;
        } else if ch == '[' {
            match c[i + 1..].iter().position(|&x| x == ']') {
                Some(0) => {
                    out.push_str("$^");
                    i += 2;
                }
                Some(n) => {
                    let inner: String = c[i + 1..i + 1 + n].iter().collect();
                    out.push_str(&format!("[{}]", lit(&inner)));
                    i += n + 2;
                }
                None => {
                    out.push_str("$^");
                    i += 1;
                }
            }
        } else if ch == '\'' && c.get(i + 1) == Some(&'\'') {
            out.push('\'');
            i += 2;
        } else {
            out.push_str(&lit(&ch.to_string()));
            i += 1;
        }
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ObjectName;

    fn diff(ty: ObjectType, schema: &str, name: &str, kind: DiffKind) -> Difference {
        Difference { object_type: ty, name: ObjectName::new(schema, name), target_name: None, kind, properties: Vec::new(), selected: true, trivial: false }
    }

    fn filter(xml_types: &str) -> Filter {
        let xml = format!(
            r#"<?xml version="1.0" encoding="utf-16" standalone="yes"?>
<!--
SQL Compare
Version:15.4.17.28422-->
<NamedFilter version="1" type="SQLCompareFilter">
  <!--This filter can be loaded using the SQL Compare UI or used with a SQL Compare command line session using the /filter switch-->
  <FilterName>Sales only</FilterName>
  <Filter version="1" type="DifferenceFilter">
    <FilterCaseSensitive>False</FilterCaseSensitive>
    <Filters version="1">
      <None version="1"><Include>True</Include><Expression>TRUE</Expression></None>
      {xml_types}
    </Filters>
  </Filter>
</NamedFilter>"#
        );
        Filter::from_xml(&xml).unwrap()
    }

    #[test]
    fn per_type_rules() {
        let f = filter(
            "<Table version=\"1\"><Include>True</Include><Expression>(@SCHEMA = 'sales') AND (@NAME NOT LIKE 'tmp%')</Expression></Table>
             <View version=\"1\"><Include>True</Include><Expression /></View>
             <StoredProcedure version=\"1\"><Include>False</Include><Expression>@NAME LIKE '[_]%'</Expression></StoredProcedure>
             <User version=\"1\"><Include>True</Include><Expression>@SCHEMA = 'x'</Expression></User>",
        );
        assert_eq!(f.name, "Sales only");
        assert!(f.includes(&diff(ObjectType::Table, "Sales", "Order", DiffKind::Different)));
        assert!(!f.includes(&diff(ObjectType::Table, "Sales", "tmpOrder", DiffKind::OnlyIn1)));
        assert!(!f.includes(&diff(ObjectType::Table, "dbo", "Order", DiffKind::OnlyIn2)));
        assert!(!f.includes(&diff(ObjectType::View, "Sales", "V", DiffKind::Different)));
        assert!(!f.includes(&diff(ObjectType::StoredProcedure, "dbo", "_internal", DiffKind::Different)));
        assert!(f.includes(&diff(ObjectType::StoredProcedure, "dbo", "GetOrders", DiffKind::Different)));
        // @SCHEMA says nothing about users: they stay.
        assert!(f.includes(&diff(ObjectType::User, "", "app", DiffKind::Different)));
        assert!(f.includes(&diff(ObjectType::Sequence, "dbo", "S", DiffKind::Different)));
    }

    #[test]
    fn global_rule_and_escape() {
        let f = filter("<Synonym version=\"1\"><Include>True</Include><Expression>@NAME LIKE 'a!%%' ESCAPE '!' OR @NAME = 'it''s'</Expression></Synonym>")
            .with_global(false, "@SCHEMA = 'audit'");
        assert!(f.includes(&diff(ObjectType::Synonym, "dbo", "a%b", DiffKind::Different)));
        assert!(!f.includes(&diff(ObjectType::Synonym, "dbo", "ab", DiffKind::Different)));
        assert!(!f.includes(&diff(ObjectType::Synonym, "audit", "a%b", DiffKind::Different)));
        assert!(f.includes(&diff(ObjectType::Synonym, "dbo", "it''s", DiffKind::Different)));
    }

    #[test]
    fn like_patterns() {
        assert_eq!(like_regex("a%b_c", None), "^a.*b.c$");
        assert!(Regex::new(&like_regex("[a-c]x", None)).unwrap().is_match("-x"));
        assert!(!Regex::new(&like_regex("[]x", None)).unwrap().is_match("x"));
        assert!(parse("@NAME LIKE 'a' ESCAPE 'ab'", false).is_err());
        assert!(parse("@TYPE = 'table'", false).is_err());
        assert!(parse("NOT (@name = 'a' or @schema != 'b')", false).is_ok());
    }

    impl Filter {
        fn with_global(mut self, include: bool, e: &str) -> Filter {
            self.types.insert("None".into(), TypeFilter { include, filter: Some(parse(e, false).unwrap()) });
            self
        }
    }
}
