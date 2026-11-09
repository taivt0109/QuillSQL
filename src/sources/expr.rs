//! Check constraint and computed column text as the classic compare tool writes it. The classic compare tool does not
//! keep SQL Server's stored text for these: it parses it and prints it again, which adds or
//! drops parentheses (`(-100)` becomes `((-100))`, `([a]+[b]*(2))>(10)` becomes
//! `[a]+([b]*(2))>(10)`), expands IN and BETWEEN, and spells some functions differently.
//! Text that cannot be parsed is kept as it is.

use crate::schema::text::{Tok, tokenize};

/// Operator levels, tightest first; `None` for operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    None,
    Tilde,
    Multiplication,
    Addition,
    Boolean,
    BitwiseOr,
    Not,
    And,
    Or,
}

#[derive(Clone, Debug)]
enum E {
    /// Numbers and money, printed in parentheses.
    Number(String),
    /// Strings, NULL, magic words such as date parts.
    Plain(String),
    Column(Vec<String>),
    /// Parentheses that are kept: around a unary operator.
    Paren(Box<E>),
    Unary(String, Box<E>),
    Binary(String, Box<E>, Box<E>),
    Not(Box<E>),
    IsNull(Box<E>, bool),
    Like { not: bool, lhs: Box<E>, rhs: Box<E>, escape: Option<Box<E>> },
    In { not: bool, lhs: Box<E>, items: Vec<E> },
    Between { not: bool, e: Box<E>, low: Box<E>, high: Box<E> },
    Function { name: Vec<String>, args: Vec<E> },
    Cast { word: String, e: Box<E>, ty: String },
    Convert { word: String, ty: String, e: Box<E>, style: Option<Box<E>> },
    Case { input: Option<Box<E>>, whens: Vec<(E, E)>, otherwise: Option<Box<E>> },
    Collate(Box<E>, String),
    NextValue(Vec<String>),
}

/// The text the classic compare tool writes for a stored check or computed column definition.
pub fn rewrite(stored: &str) -> String {
    let mut p = P { t: tokens(stored), i: 0 };
    let Some(e) = p.root() else { return stored.to_string() };
    let mut out = String::from("(");
    print(&mut out, &e);
    out.push(')');
    out
}

#[derive(Clone, Debug)]
struct T {
    kind: Tok,
    text: String,
}

/// Significant tokens, with `1.5e+003` joined into one number.
fn tokens(s: &str) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let raw: Vec<_> = tokenize(s).into_iter().filter(|t| !matches!(t.kind, Tok::Space | Tok::LineComment | Tok::BlockComment)).collect();
    let mut k = 0;
    while k < raw.len() {
        let t = &raw[k];
        if t.kind == Tok::Word
            && is_number(&t.text)
            && t.text.ends_with(['e', 'E'])
            && raw.get(k + 1).is_some_and(|x| x.text == "+" || x.text == "-")
            && raw.get(k + 2).is_some_and(|x| x.text.chars().all(|c| c.is_ascii_digit()))
        {
            out.push(T { kind: Tok::Word, text: format!("{}{}{}", t.text, raw[k + 1].text, raw[k + 2].text) });
            k += 3;
            continue;
        }
        out.push(T { kind: t.kind, text: t.text.clone() });
        k += 1;
    }
    out
}

fn is_number(s: &str) -> bool {
    let s = s.strip_prefix('$').unwrap_or(s);
    s.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

fn unquote(t: &T) -> String {
    match t.kind {
        Tok::Quoted => {
            let inner = &t.text[1..t.text.len().saturating_sub(1)];
            if t.text.starts_with('[') { inner.replace("]]", "]") } else { inner.replace("\"\"", "\"") }
        }
        _ => t.text.clone(),
    }
}

fn quote(s: &str) -> String {
    format!("[{}]", s.replace(']', "]]"))
}

const DATE_PARTS: &[&str] = &[
    "year", "yy", "yyyy", "quarter", "qq", "q", "month", "mm", "m", "dayofyear", "dy", "y", "day", "dd", "d", "week", "wk", "ww", "weekday", "dw", "w", "hour", "hh",
    "minute", "mi", "n", "second", "ss", "s", "millisecond", "ms", "microsecond", "mcs", "nanosecond", "ns", "tzoffset", "tz", "iso_week", "isowk", "isoww",
];
const DATE_FUNCTIONS: &[&str] = &["datediff", "dateadd", "datepart", "datename", "datediff_big", "datetrunc", "date_bucket"];

struct P {
    t: Vec<T>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }

    fn word(&self, w: &str) -> bool {
        self.peek().is_some_and(|t| t.kind == Tok::Word && t.text.eq_ignore_ascii_case(w))
    }

    fn word_at(&self, k: usize, w: &str) -> bool {
        self.t.get(self.i + k).is_some_and(|t| t.kind == Tok::Word && t.text.eq_ignore_ascii_case(w))
    }

    fn eat(&mut self, w: &str) -> bool {
        if self.word(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn punct(&self, p: &str) -> bool {
        self.peek().is_some_and(|t| t.kind == Tok::Punct && t.text == p)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if self.punct(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: &str) -> Option<()> {
        self.eat_punct(p).then_some(())
    }

    /// The whole text: one level of parentheses around everything is not part of the expression.
    fn root(&mut self) -> Option<E> {
        let e = if self.punct("(") && self.closes_at_end(0) {
            self.i += 1;
            let e = self.or()?;
            self.expect_punct(")")?;
            e
        } else {
            self.or()?
        };
        (self.i == self.t.len()).then_some(e)
    }

    fn closes_at_end(&self, from: usize) -> bool {
        let mut depth = 0;
        for (k, t) in self.t.iter().enumerate().skip(self.i + from) {
            if t.kind == Tok::Punct && t.text == "(" {
                depth += 1;
            } else if t.kind == Tok::Punct && t.text == ")" {
                depth -= 1;
                if depth == 0 {
                    return k == self.t.len() - 1;
                }
            }
        }
        false
    }

    // OR and AND chains nest to the right.
    fn or(&mut self) -> Option<E> {
        let l = self.and()?;
        if self.eat("OR") {
            return Some(E::Binary("OR".into(), Box::new(l), Box::new(self.or()?)));
        }
        Some(l)
    }

    fn and(&mut self) -> Option<E> {
        let l = self.not()?;
        if self.eat("AND") {
            return Some(E::Binary("AND".into(), Box::new(l), Box::new(self.and()?)));
        }
        Some(l)
    }

    fn not(&mut self) -> Option<E> {
        if self.eat("NOT") {
            return Some(E::Not(Box::new(self.not()?)));
        }
        self.predicate()
    }

    fn predicate(&mut self) -> Option<E> {
        let l = self.additive()?;
        let not = self.word("NOT") && (self.word_at(1, "LIKE") || self.word_at(1, "IN") || self.word_at(1, "BETWEEN"));
        if not {
            self.i += 1;
        }
        if self.eat("LIKE") {
            let rhs = self.additive()?;
            let escape = if self.eat("ESCAPE") { Some(Box::new(self.additive()?)) } else { None };
            return Some(E::Like { not, lhs: Box::new(l), rhs: Box::new(rhs), escape });
        }
        if self.eat("IN") {
            self.expect_punct("(")?;
            if self.word("SELECT") {
                return None;
            }
            let mut items = vec![self.or()?];
            while self.eat_punct(",") {
                items.push(self.or()?);
            }
            self.expect_punct(")")?;
            return Some(E::In { not, lhs: Box::new(l), items });
        }
        if self.eat("BETWEEN") {
            let low = self.additive()?;
            if !self.eat("AND") {
                return None;
            }
            let high = self.additive()?;
            return Some(E::Between { not, e: Box::new(l), low: Box::new(low), high: Box::new(high) });
        }
        if self.eat("IS") {
            let not = self.eat("NOT");
            if self.eat("NULL") {
                return Some(E::IsNull(Box::new(l), not));
            }
            return None;
        }
        // Comparison operators, possibly split into punctuation tokens: >=, <>, !=, !<, !>.
        let ops = [">=", "<=", "<>", "!=", "!<", "!>", "=", "<", ">"];
        for op in ops {
            if self.at_op(op) {
                self.i += op.len();
                let r = self.additive()?;
                return Some(E::Binary(op.into(), Box::new(unwrap_left(l)), Box::new(r)));
            }
        }
        Some(l)
    }

    fn at_op(&self, op: &str) -> bool {
        op.chars().enumerate().all(|(k, c)| self.t.get(self.i + k).is_some_and(|t| t.kind == Tok::Punct && t.text == c.to_string()))
    }

    /// A sign binds like addition in T-SQL: `-a*2` is `-(a*2)`.
    fn additive(&mut self) -> Option<E> {
        let mut l = match ["-", "+"].into_iter().find(|o| self.punct(o)) {
            Some(op) => {
                self.i += 1;
                E::Unary(op.into(), Box::new(self.multiplicative()?))
            }
            None => self.multiplicative()?,
        };
        loop {
            let Some(op) = ["+", "-", "&", "^", "|"].into_iter().find(|o| self.punct(o)) else { return Some(l) };
            self.i += 1;
            let r = self.multiplicative()?;
            l = E::Binary(op.into(), Box::new(unwrap_left(l)), Box::new(r));
        }
    }

    fn multiplicative(&mut self) -> Option<E> {
        let mut l = self.unary()?;
        loop {
            let Some(op) = ["*", "/", "%"].into_iter().find(|o| self.punct(o)) else { return Some(l) };
            self.i += 1;
            let r = self.unary()?;
            l = E::Binary(op.into(), Box::new(unwrap_left(l)), Box::new(r));
        }
    }

    fn unary(&mut self) -> Option<E> {
        if let Some(op) = ["-", "+", "~"].into_iter().find(|o| self.punct(o)) {
            self.i += 1;
            return Some(E::Unary(op.into(), Box::new(self.unary()?)));
        }
        let mut e = self.primary()?;
        while self.eat("COLLATE") {
            let name = self.peek()?.text.clone();
            self.i += 1;
            e = E::Collate(Box::new(e), name);
        }
        Some(e)
    }

    fn name(&mut self) -> Option<Vec<String>> {
        let t = self.peek()?;
        if !matches!(t.kind, Tok::Word | Tok::Quoted) {
            return None;
        }
        let mut parts = vec![unquote(t)];
        self.i += 1;
        while self.punct(".") && self.t.get(self.i + 1).is_some_and(|t| matches!(t.kind, Tok::Word | Tok::Quoted)) {
            self.i += 1;
            parts.push(unquote(&self.t[self.i]));
            self.i += 1;
        }
        Some(parts)
    }

    /// `[varchar](10)`, `decimal(10,2)`, `nvarchar(max)` as `[varchar](10)`.
    fn data_type(&mut self) -> Option<String> {
        let parts = self.name()?;
        let mut s = parts.iter().map(|p| quote(p)).collect::<Vec<_>>().join(".");
        if self.eat_punct("(") {
            let mut args = Vec::new();
            while !self.punct(")") {
                let t = self.peek()?;
                args.push(if t.text.eq_ignore_ascii_case("max") { "MAX".to_string() } else { t.text.clone() });
                self.i += 1;
                self.eat_punct(",");
            }
            self.expect_punct(")")?;
            s.push_str(&format!("({})", args.join(",")));
        }
        Some(s)
    }

    fn primary(&mut self) -> Option<E> {
        let t = self.peek()?.clone();
        if t.kind == Tok::Punct && t.text == "(" {
            self.i += 1;
            if self.word("SELECT") {
                return None;
            }
            let e = self.or()?;
            self.expect_punct(")")?;
            return Some(match e {
                E::Unary(..) => E::Paren(Box::new(e)),
                other => other,
            });
        }
        if t.kind == Tok::Str {
            self.i += 1;
            return Some(E::Plain(t.text));
        }
        if t.kind == Tok::Word && is_number(&t.text) {
            self.i += 1;
            return Some(E::Number(t.text));
        }
        if t.kind == Tok::Word && t.text.eq_ignore_ascii_case("NULL") {
            self.i += 1;
            return Some(E::Plain("NULL".into()));
        }
        if t.kind == Tok::Word && t.text.eq_ignore_ascii_case("CASE") {
            self.i += 1;
            let input = if self.word("WHEN") { None } else { Some(Box::new(self.or()?)) };
            let mut whens = Vec::new();
            while self.eat("WHEN") {
                let w = self.or()?;
                if !self.eat("THEN") {
                    return None;
                }
                whens.push((w, self.or()?));
            }
            let otherwise = if self.eat("ELSE") { Some(Box::new(self.or()?)) } else { None };
            if !self.eat("END") {
                return None;
            }
            return Some(E::Case { input, whens, otherwise });
        }
        if t.kind == Tok::Word && t.text.eq_ignore_ascii_case("NEXT") && self.word_at(1, "VALUE") && self.word_at(2, "FOR") {
            self.i += 3;
            return Some(E::NextValue(self.name()?));
        }
        if t.kind == Tok::Word && (t.text.eq_ignore_ascii_case("CURRENT_TIMESTAMP") || t.text.eq_ignore_ascii_case("CURRENT_USER")) && !self.t.get(self.i + 1).is_some_and(|x| x.text == "(") {
            self.i += 1;
            return Some(E::Function { name: vec![t.text.to_ascii_lowercase()], args: Vec::new() });
        }
        let name = self.name()?;
        if self.punct(":") {
            // Static method calls (geometry::Point...) are left alone.
            return None;
        }
        if !self.eat_punct("(") {
            return Some(E::Column(name));
        }
        // [dbo].[f](...) and [col].Method(...) look alike; both are kept as stored.
        if name.len() > 1 {
            return None;
        }
        let fname = name.last().cloned().unwrap_or_default().to_ascii_lowercase();
        if name.len() == 1 && matches!(fname.as_str(), "cast" | "try_cast") {
            let e = self.or()?;
            if !self.eat("AS") {
                return None;
            }
            let ty = self.data_type()?;
            self.expect_punct(")")?;
            return Some(E::Cast { word: fname.to_ascii_uppercase(), e: Box::new(e), ty });
        }
        if name.len() == 1 && matches!(fname.as_str(), "convert" | "try_convert") {
            let ty = self.data_type()?;
            self.expect_punct(",")?;
            let e = self.or()?;
            let style = if self.eat_punct(",") { Some(Box::new(self.or()?)) } else { None };
            self.expect_punct(")")?;
            return Some(E::Convert { word: fname.to_ascii_uppercase(), ty, e: Box::new(e), style });
        }
        let mut args = Vec::new();
        if !self.punct(")") {
            loop {
                if args.is_empty() && name.len() == 1 && DATE_FUNCTIONS.contains(&fname.as_str())
                    && let Some(t) = self.peek()
                    && t.kind == Tok::Word
                    && DATE_PARTS.iter().any(|d| t.text.eq_ignore_ascii_case(d))
                    && self.t.get(self.i + 1).is_some_and(|x| x.text == ",")
                {
                    args.push(E::Plain(t.text.clone()));
                    self.i += 1;
                } else {
                    args.push(self.or()?);
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect_punct(")")?;
        Some(E::Function { name, args })
    }
}

/// Parentheses kept around a unary operator are dropped when it is a left operand.
fn unwrap_left(e: E) -> E {
    match e {
        E::Paren(inner) => *inner,
        other => other,
    }
}

fn level_of(op: &str) -> Level {
    match op.to_ascii_uppercase().as_str() {
        "AND" => Level::And,
        "OR" => Level::Or,
        "*" | "/" | "%" => Level::Multiplication,
        "+" | "-" | "&" => Level::Addition,
        "|" | "^" => Level::BitwiseOr,
        _ => Level::Boolean,
    }
}

/// Prints `e` and returns its operator level.
fn print(out: &mut String, e: &E) -> Level {
    match e {
        E::Number(n) => out.push_str(&format!("({n})")),
        E::Plain(s) => out.push_str(s),
        E::Column(parts) => out.push_str(&parts.iter().map(|p| quote(p)).collect::<Vec<_>>().join(".")),
        E::Paren(inner) => {
            out.push('(');
            print(out, inner);
            out.push(')');
        }
        E::Unary(op, operand) => {
            let level = if op == "~" { Level::Tilde } else { Level::Addition };
            if let E::Number(n) = operand.as_ref() {
                out.push_str(&format!("({op}{n})"));
            } else {
                out.push_str(op);
                let at = out.len();
                let inner = print(out, operand);
                if level <= inner {
                    out.insert(at, '(');
                    out.push(')');
                }
            }
            return level;
        }
        E::Binary(op, l, r) => return binary(out, op, l, r),
        E::Not(inner) => {
            let group = matches!(inner.as_ref(), E::Binary(op, ..) if op == "AND" || op == "OR");
            out.push_str("NOT ");
            if group {
                out.push('(');
            }
            print(out, inner);
            if group {
                out.push(')');
            }
            return Level::Not;
        }
        E::IsNull(l, not) => {
            print(out, l);
            out.push_str(if *not { " IS NOT NULL" } else { " IS NULL" });
        }
        E::Like { not, lhs, rhs, escape } => {
            if *not {
                out.push_str("NOT ");
            }
            print(out, lhs);
            out.push_str(" like ");
            print(out, rhs);
            if let Some(x) = escape {
                out.push_str(" escape ");
                print(out, x);
            }
        }
        E::In { not, lhs, items } => {
            if *not {
                out.push_str("NOT ");
            }
            let mut left = String::new();
            print(&mut left, lhs);
            out.push('(');
            for (k, item) in items.iter().rev().enumerate() {
                if k > 0 {
                    out.push_str(" OR ");
                }
                out.push_str(&left);
                out.push('=');
                print(out, item);
            }
            out.push(')');
        }
        E::Between { not, e, low, high } => {
            if *not {
                out.push_str("NOT (");
            }
            print(out, e);
            out.push_str(">=");
            print(out, low);
            out.push_str(" AND ");
            print(out, e);
            out.push_str("<=");
            print(out, high);
            if *not {
                out.push(')');
            }
        }
        E::Function { name, args } => {
            let lower = name.last().map(|n| n.to_ascii_lowercase()).unwrap_or_default();
            let bare = name.len() == 1 && (lower == "current_timestamp" || lower == "current_user");
            let datepart = name.len() == 1 && matches!(lower.as_str(), "day" | "month" | "year");
            if datepart {
                out.push_str(&format!("datepart({lower},"));
            } else if name.len() == 1 {
                out.push_str(&lower);
            } else {
                out.push_str(&name.iter().map(|p| quote(p)).collect::<Vec<_>>().join("."));
            }
            if !datepart && !bare {
                out.push('(');
            }
            for (k, a) in args.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                print(out, a);
            }
            if !bare {
                out.push(')');
            }
        }
        E::Cast { word, e, ty } => {
            out.push_str(&format!("{word}("));
            print(out, e);
            out.push_str(&format!(" AS {ty})"));
        }
        E::Convert { word, ty, e, style } => {
            out.push_str(&format!("{word}({ty},"));
            print(out, e);
            out.push(',');
            match style {
                Some(s) => {
                    print(out, s);
                }
                None => out.push_str("(0)"),
            }
            out.push(')');
        }
        E::Case { input, whens, otherwise } => {
            out.push_str("case ");
            if let Some(i) = input {
                print(out, i);
            }
            for (w, t) in whens {
                out.push_str(" when ");
                print(out, w);
                out.push_str(" then ");
                print(out, t);
            }
            if let Some(x) = otherwise {
                out.push_str(" else ");
                print(out, x);
            }
            out.push_str(" end");
        }
        E::Collate(e, name) => {
            print(out, e);
            out.push_str(&format!(" COLLATE {name}"));
        }
        E::NextValue(name) => out.push_str(&format!("NEXT VALUE FOR {}", name.iter().map(|p| quote(p)).collect::<Vec<_>>().join("."))),
    }
    Level::None
}

/// A binary operator with the parentheses its operands need. AND and OR chains nest to the
/// right; the other operators to the left.
fn binary(out: &mut String, op: &str, l: &E, r: &E) -> Level {
    let level = level_of(op);
    let chain = matches!(level, Level::And | Level::Or);
    let start = out.len();
    let lp = print(out, l);
    if (!chain || lp < Level::And) && (lp > level || (lp == level && level < Level::Not)) {
        out.insert(start, '(');
        out.push(')');
    }
    let after_left = out.len();
    if chain {
        out.push_str(&format!(" {} ", op.to_ascii_uppercase()));
    } else {
        out.push_str(op);
    }
    let at = out.len();
    let rp = print(out, r);
    if chain {
        if rp > level {
            out.insert(at, '(');
            out.push(')');
        }
        if (lp >= Level::And && rp < Level::And) || (lp >= rp && lp > Level::And) {
            out.insert(start, '(');
            out.insert(after_left + 1, ')');
        }
    } else if ((lp <= rp && rp > Level::None && rp != Level::Boolean) || rp >= level) && (rp < Level::And || lp == rp) {
        out.insert(at, '(');
        out.push(')');
    }
    level
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stored text (sys.check_constraints / sys.computed_columns) → what the classic compare tool 15 wrote.
    const CASES: &[(&str, &str)] = &[
        ("([a]>=(-100))", "([a]>=((-100)))"),
        ("(([a]+[b]*(2))>(10) AND [c] like 'x%')", "([a]+([b]*(2))>(10) AND [c] like 'x%')"),
        ("([a]=(3) OR [a]=(2) OR [a]=(1))", "([a]=(3) OR [a]=(2) OR [a]=(1))"),
        ("(NOT ([a]=(2) OR [a]=(1)))", "(NOT ([a]=(2) OR [a]=(1)))"),
        ("([c] IS NOT NULL OR [a] IS NULL)", "([c] IS NOT NULL OR [a] IS NULL)"),
        ("(([a]=(1) OR [b]=(2)) AND ([c]='q' OR [c]='r'))", "(([a]=(1) OR [b]=(2)) AND ([c]='q' OR [c]='r'))"),
        ("(NOT ([a]=(1) AND [b]=(2)))", "(NOT ([a]=(1) AND [b]=(2)))"),
        ("(len([c])>(2) AND upper([c])=[c])", "(len([c])>(2) AND upper([c])=[c])"),
        ("(datepart(year,[d])>(2000) AND datepart(month,[d])<>(2) AND datepart(day,[d])<(31))", "(datepart(year,[d])>(2000) AND datepart(month,[d])<>(2) AND datepart(day,[d])<(31))"),
        ("(CONVERT([varchar](10),[a])<>'' AND CONVERT([varchar](10),[d],(112))>'2000')", "(CONVERT([varchar](10),[a],(0))<>'' AND CONVERT([varchar](10),[d],(112))>'2000')"),
        ("(case when [a]>(0) then (1) else (0) end=(1))", "(case  when [a]>(0) then (1) else (0) end=(1))"),
        ("(case [a] when (1) then 'x' when (2) then 'y'  end IS NOT NULL)", "(case [a] when (1) then 'x' when (2) then 'y' end IS NOT NULL)"),
        ("([m]>($1.5000) AND [f]<(1.5000000000000000e+003) AND [x]<>(0.25))", "([m]>($1.5000) AND [f]<(1.5000000000000000e+003) AND [x]<>(0.25))"),
        ("(([a]-([b]-(1)))>(0) AND [a]/([b]+(1))<(5) AND [a]%(2)=(0))", "([a]-([b]-(1))>(0) AND [a]/([b]+(1))<(5) AND [a]%(2)=(0))"),
        ("([c] like '[a-z]%' escape '!'  OR NOT [c] like 'zz%')", "([c] like '[a-z]%' escape '!' OR NOT [c] like 'zz%')"),
        ("(( -[a])<(0) AND ~[b]<>(0) AND ([a]&(1))=(0) AND ([a]|[b])>(0))", "(-[a]<(0) AND ~[b]<>(0) AND [a]&(1)=(0) AND ([a]|[b])>(0))"),
        ("([a]=(1) OR [b]=(2) OR [c]='z' AND [a]>(0))", "([a]=(1) OR [b]=(2) OR [c]='z' AND [a]>(0))"),
        ("((coalesce([a],(0))+isnull([b],(0)))>=(0) AND datediff(day,[d],getdate())<(365))", "(coalesce([a],(0))+isnull([b],(0))>=(0) AND datediff(day,[d],getdate())<(365))"),
        ("(([c]) collate Latin1_General_BIN='A' AND N'abc'<>[c])", "([c] COLLATE Latin1_General_BIN='A' AND N'abc'<>[c])"),
        ("([a]>( -[b]) AND [a]>( -([b]+(1))) AND [a]<>(-1.5))", "([a]>(-[b]) AND [a]>(-([b]+(1))) AND [a]<>((-1.5)))"),
        ("(getdate()>[d] AND user_name()<>'x' AND sysdatetime()>[d])", "(getdate()>[d] AND user_name()<>'x' AND sysdatetime()>[d])"),
        ("([a]+[b])", "([a]+[b])"),
        ("(case when [a]>[b] then [a] else [b] end)", "(case  when [a]>[b] then [a] else [b] end)"),
        ("(CONVERT([varchar](10),[d],(120)))", "(CONVERT([varchar](10),[d],(120)))"),
        ("( -([a]*(2)))", "(-[a]*(2))"),
        ("(isnull([c],'')+'x')", "(isnull([c],'')+'x')"),
        ("([a]*([b]+(1))-(2))", "([a]*([b]+(1))-(2))"),
        ("([Balance]>=(0))", "([Balance]>=(0))"),
        ("([Status]='S' OR [Status]='P' OR [Status]='N')", "([Status]='S' OR [Status]='P' OR [Status]='N')"),
        ("([Note]<>N'')", "([Note]<>N'')"),
        ("(len([Name]))", "(len([Name]))"),
    ];

    #[test]
    fn matches_sql_compare() {
        for (stored, expected) in CASES {
            assert_eq!(rewrite(stored), *expected, "{stored}");
        }
    }

    #[test]
    fn rewriting_twice_changes_nothing() {
        for (_, expected) in CASES {
            assert_eq!(rewrite(expected), *expected, "{expected}");
        }
    }

    #[test]
    fn keeps_what_it_cannot_parse() {
        assert_eq!(rewrite("([a] IN (SELECT 1))"), "([a] IN (SELECT 1))");
        assert_eq!(rewrite("([g].STArea()>(0))"), "([g].STArea()>(0))");
        assert_eq!(rewrite("(geometry::Point((1),(2),(0)))"), "(geometry::Point((1),(2),(0)))");
    }
}
