//! Rewrites the whitespace between tokens and nothing else. `format` re-lexes
//! the result and compares it to the input tokens to enforce that.

use std::collections::HashSet;

use crate::keywords::{is_call_like, is_reserved};
use crate::lexer::{Kind, Token};
use crate::options::{AndOrAlign, Layout, ListBreak, ParenStyle, SemicolonSpace};
use crate::transform::{missing_terminators, next_sig, prev_sig, upper_words};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// Statements and subqueries: clause rules apply.
    Query,
    /// Function arguments, IN lists and the like: kept on one line.
    Inline,
    /// Column definitions and INSERT column lists: one item per line.
    List,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    None,
    Select,
    From,
    Where,
    Set,
    Insert,
    GroupOrder,
    Other,
}

/// What the next token of a frame starts, so its position can be recorded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Awaiting {
    Nothing,
    ListItem,
    GroupItem,
    SetItem,
    Condition,
    OnCondition,
    FromTable,
    CteName,
}

/// State applied to the frame after the current word is written, once its column is known.
enum Mark {
    None,
    Clause(Clause, Awaiting, usize),
    Await(Awaiting),
    Join,
    On,
    Case { flat: bool, simple: bool },
    When,
}

struct CaseState {
    col: usize,
    when_col: Option<usize>,
    simple: bool,
    flat: bool,
}

struct Frame {
    kind: FrameKind,
    base: usize,
    /// Column for `)` on its own line, or `None` to keep it after the last token.
    close_col: Option<usize>,
    /// The first token after `(` stays on the same line.
    fresh: bool,
    clause: Clause,
    clause_col: usize,
    /// Column and length of the keyword that introduced the current conditions.
    cond_kw: (usize, usize),
    cond_col: Option<usize>,
    list_col: Option<usize>,
    awaiting: Awaiting,
    modifiers: bool,
    joined: bool,
    join_col: usize,
    between: usize,
    cases: Vec<CaseState>,
    cte_name_col: Option<usize>,
}

impl Frame {
    fn new(kind: FrameKind, base: usize, close_col: Option<usize>, fresh: bool) -> Self {
        Frame {
            kind,
            base,
            close_col,
            fresh,
            clause: Clause::None,
            clause_col: base,
            cond_kw: (base, 0),
            cond_col: None,
            list_col: None,
            awaiting: Awaiting::Nothing,
            modifiers: false,
            joined: false,
            join_col: base,
            between: 0,
            cases: Vec::new(),
            cte_name_col: None,
        }
    }
}

struct Writer<'a> {
    tokens: &'a [Token],
    up: Vec<String>,
    l: &'a Layout,
    nl: &'a str,
    out: String,
    col: usize,
    at_start: bool,
    line_indent: usize,
    pending: Option<usize>,
    /// No space before the next token.
    glue: bool,
    last: Option<usize>,
    last_start: usize,
    frames: Vec<Frame>,
    /// Columns of the open BEGIN keywords.
    blocks: Vec<usize>,
    extra: usize,
    if_header: bool,
    stmt: String,
    module: bool,
    create_table: bool,
    /// Token range of a statement printed on one line.
    flat: Option<(usize, usize)>,
    /// Last token of every statement.
    ends: HashSet<usize>,
    mark: Mark,
    after_go: bool,
}

const STATEMENTS: &[&str] = &[
    "DECLARE", "PRINT", "EXEC", "EXECUTE", "RETURN", "CREATE", "ALTER", "DROP", "TRUNCATE", "USE",
    "THROW", "RAISERROR", "COMMIT", "ROLLBACK", "BREAK", "CONTINUE", "WAITFOR", "GOTO", "OPEN",
    "CLOSE", "FETCH", "DEALLOCATE", "BACKUP", "RESTORE", "DBCC", "GRANT", "DENY", "REVOKE",
];

const JOIN_WORDS: &[&str] = &["INNER", "CROSS", "LEFT", "RIGHT", "FULL", "OUTER"];

/// Words that end a SELECT list or a condition when they appear at the same depth.
const LIST_ENDS: &[&str] = &[
    "FROM", "INTO", "WHERE", "GROUP", "ORDER", "HAVING", "UNION", "EXCEPT", "INTERSECT", "OPTION",
    "FOR", "WINDOW", "SELECT", "INSERT", "UPDATE", "DELETE", "SET", "BEGIN", "END", "IF", "ELSE",
    "WHILE", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "OUTER", "ON",
];

impl Writer<'_> {
    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("root frame is never popped")
    }

    fn top(&self) -> &Frame {
        self.frames.last().expect("root frame is never popped")
    }

    fn is_root(&self) -> bool {
        self.frames.len() == 1
    }

    fn block_content(&self) -> usize {
        self.blocks.last().map_or(0, |c| c + self.l.indent)
    }

    fn base(&self) -> usize {
        if self.is_root() { self.block_content() + self.extra } else { self.top().base }
    }

    /// Column of a clause keyword, right-aligned to `SELECT` when the style asks for it.
    fn clause_col(&self, word: &str) -> usize {
        let shift = if self.l.right_align_clauses { 6usize.saturating_sub(word.len()) } else { 0 };
        self.base() + shift
    }

    fn newline(&mut self, blanks: usize) {
        if !self.at_start {
            self.out.push_str(self.nl);
            self.at_start = true;
        }
        if !self.out.is_empty() {
            for _ in 0..blanks {
                self.out.push_str(self.nl);
            }
        }
    }

    fn write(&mut self, i: usize, space: bool) {
        if self.at_start {
            self.out.extend(std::iter::repeat_n(' ', self.line_indent));
            self.col = self.line_indent;
            self.at_start = false;
        } else if space {
            self.out.push(' ');
            self.col += 1;
        }
        self.last_start = self.col;
        let text = &self.tokens[i].text;
        self.out.push_str(text);
        self.col = match text.rfind('\n') {
            Some(p) => text[p + 1..].chars().count(),
            None => self.col + text.chars().count(),
        };
        self.last = Some(i);
        self.glue = false;
    }

    /// Empty lines to keep or insert before token `i`.
    fn blank_lines(&mut self, i: usize) -> usize {
        let existing = self.tokens[i].newlines_before().saturating_sub(1);
        if self.out.is_empty() {
            return 0;
        }
        if std::mem::take(&mut self.after_go) {
            return if self.l.preserve_blank_lines_after_go {
                existing.min(self.l.blank_lines_after_go.max(1))
            } else {
                self.l.blank_lines_after_go
            };
        }
        let closes_block = matches!(self.up[i].as_str(), "END" | "ELSE") || self.tokens[i].kind != Kind::Word && !self.tokens[i].is_comment();
        let between_statements = self.last.is_some_and(|j| self.ends.contains(&j)) && !closes_block;
        match (between_statements, self.l.preserve_blank_lines) {
            (true, true) => existing.min(self.l.blank_lines_between_statements.max(1)),
            (true, false) => self.l.blank_lines_between_statements,
            (false, true) => existing.min(1),
            (false, false) => 0,
        }
    }

    fn in_flat(&self, i: usize) -> bool {
        self.flat.is_some_and(|(start, end)| i > start && i <= end)
    }

    /// Writes a token, on a new line at `brk` or at any column queued by the previous token.
    fn emit(&mut self, i: usize, brk: Option<usize>) {
        let first = self.take_awaiting(i);
        let mut brk = brk.or(self.pending.take());
        if brk.is_none()
            && let Some(a) = first
        {
            brk = self.first_item_break(a, i);
        }
        if std::mem::take(&mut self.frame().fresh) || self.in_flat(i) {
            brk = None;
        }
        let space = self.space_before(i);
        let blanks = self.blank_lines(i);
        if let Some(col) = brk {
            self.newline(blanks);
            self.line_indent = col;
        } else if self.at_start && blanks > 0 {
            self.newline(blanks);
        }
        self.write(i, space);
        if let Some(a) = first {
            self.record_first_item(a);
        }
        if self.flat.is_some_and(|(_, end)| i >= end) {
            self.flat = None;
        }
    }

    fn is_select_modifier(&self, i: usize) -> bool {
        let prev = prev_sig(self.tokens, i).map_or("", |p| self.up[p].as_str());
        let next = next_sig(self.tokens, i).map_or("", |n| self.up[n].as_str());
        match self.tokens[i].kind {
            Kind::Word => match self.up[i].as_str() {
                "DISTINCT" | "ALL" | "TOP" | "PERCENT" | "TIES" => true,
                "WITH" => next == "TIES",
                _ => false,
            },
            Kind::Number | Kind::Variable | Kind::LParen => prev == "TOP",
            Kind::RParen => self.top().modifiers,
            _ => false,
        }
    }

    fn take_awaiting(&mut self, i: usize) -> Option<Awaiting> {
        let awaiting = self.top().awaiting;
        if awaiting == Awaiting::Nothing {
            return None;
        }
        if awaiting == Awaiting::ListItem && self.is_select_modifier(i) {
            self.frame().modifiers = true;
            return None;
        }
        self.frame().awaiting = Awaiting::Nothing;
        Some(awaiting)
    }

    /// True when a comma (or AND/OR for conditions) follows `i` before the clause ends.
    fn has_more_items(&self, i: usize, conditions: bool) -> bool {
        let mut depth = 0usize;
        let mut between = 0usize;
        for j in i..self.tokens.len() {
            let t = &self.tokens[j];
            match t.kind {
                Kind::LParen => depth += 1,
                Kind::RParen if depth == 0 => return false,
                Kind::RParen => depth -= 1,
                Kind::Semi | Kind::Go | Kind::Raw => return false,
                Kind::Comma if depth == 0 && !conditions => return true,
                Kind::Word if depth == 0 && j > i => match self.up[j].as_str() {
                    "BETWEEN" => between += 1,
                    "AND" if between > 0 => between -= 1,
                    "AND" | "OR" if conditions => return true,
                    w if LIST_ENDS.contains(&w) => return false,
                    _ => {}
                },
                _ => {}
            }
        }
        false
    }

    fn wants_break(&self, rule: ListBreak, i: usize, conditions: bool) -> bool {
        match rule {
            ListBreak::Never => false,
            ListBreak::Always => true,
            ListBreak::IfMultiple => self.has_more_items(i, conditions),
        }
    }

    fn first_item_break(&self, awaiting: Awaiting, i: usize) -> Option<usize> {
        let f = self.top();
        let inner = f.clause_col + self.l.indent;
        let wanted = match awaiting {
            Awaiting::ListItem => {
                self.wants_break(self.l.list_first_item, i, false) || (self.l.newline_after_distinct_top && f.modifiers)
            }
            Awaiting::GroupItem => self.wants_break(self.l.group_order_items, i, false),
            Awaiting::Condition => self.wants_break(self.l.where_condition, i, true),
            Awaiting::FromTable => self.wants_break(self.l.from_table, i, false),
            Awaiting::OnCondition => return self.l.on_condition_on_new_line.then_some(f.cond_kw.0 + self.l.indent),
            Awaiting::CteName => self.l.cte_name_on_new_line,
            Awaiting::SetItem | Awaiting::Nothing => false,
        };
        wanted.then_some(inner)
    }

    fn record_first_item(&mut self, awaiting: Awaiting) {
        let at = self.last_start;
        let f = self.frame();
        match awaiting {
            Awaiting::ListItem | Awaiting::GroupItem | Awaiting::SetItem => f.list_col = Some(at),
            Awaiting::Condition | Awaiting::OnCondition => f.cond_col = Some(at),
            Awaiting::CteName => f.cte_name_col = Some(at),
            Awaiting::FromTable | Awaiting::Nothing => {}
        }
    }

    fn apply_mark(&mut self) {
        let at = self.last_start;
        let mark = std::mem::replace(&mut self.mark, Mark::None);
        let f = self.frame();
        match mark {
            Mark::None => {}
            Mark::Clause(clause, awaiting, keyword_len) => {
                f.clause = clause;
                f.clause_col = at;
                f.cond_kw = (at, keyword_len);
                f.cond_col = None;
                f.list_col = None;
                f.awaiting = awaiting;
                f.modifiers = false;
            }
            Mark::Await(awaiting) => f.awaiting = awaiting,
            Mark::Join => f.join_col = at,
            Mark::On => {
                f.cond_kw = (at, 2);
                f.cond_col = None;
                f.awaiting = Awaiting::OnCondition;
            }
            Mark::Case { flat, simple } => f.cases.push(CaseState { col: at, when_col: None, simple, flat }),
            Mark::When => {
                if let Some(c) = f.cases.last_mut() {
                    c.when_col.get_or_insert(at);
                }
            }
        }
    }

    fn space_before(&self, i: usize) -> bool {
        let Some(last) = self.last else { return false };
        if self.glue {
            return false;
        }
        let prev = &self.tokens[last];
        let cur = &self.tokens[i];
        match cur.kind {
            Kind::Comma => self.l.space_before_comma,
            Kind::Semi => self.l.semicolon_space == SemicolonSpace::SpaceBefore,
            Kind::RParen | Kind::Dot => false,
            Kind::LParen => self.space_before_paren(last),
            _ => match prev.kind {
                Kind::LParen | Kind::Dot => false,
                Kind::Op if prev.text == "::" => false,
                _ => cur.text != "::",
            },
        }
    }

    fn space_before_paren(&self, last: usize) -> bool {
        let prev = &self.tokens[last];
        let names_a_list = self.top().clause == Clause::Insert || self.create_table;
        match prev.kind {
            Kind::Word if is_reserved(&self.up[last]) => !is_call_like(&self.up[last]),
            Kind::Word | Kind::Bracket | Kind::Quoted => names_a_list,
            Kind::LParen | Kind::Dot => false,
            Kind::Op => prev.text != "::",
            _ => true,
        }
    }

    /// Text length of `from..=to` on one line, or `usize::MAX` if it cannot be one line.
    fn flat_len(&self, from: usize, to: usize) -> usize {
        let mut len = 0usize;
        for j in from..=to.min(self.tokens.len() - 1) {
            let t = &self.tokens[j];
            if t.is_comment() || t.text.contains('\n') || matches!(t.kind, Kind::Go | Kind::Raw) {
                return usize::MAX;
            }
            len += t.text.chars().count() + usize::from(j > from);
        }
        len
    }

    fn matching_paren(&self, open: usize) -> usize {
        let mut depth = 0usize;
        for j in open..self.tokens.len() {
            match self.tokens[j].kind {
                Kind::LParen => depth += 1,
                Kind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
        self.tokens.len() - 1
    }

    fn case_end(&self, case: usize) -> usize {
        let mut depth = 0usize;
        for j in case..self.tokens.len() {
            match self.up[j].as_str() {
                "CASE" => depth += 1,
                "END" => {
                    depth -= 1;
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
        self.tokens.len() - 1
    }

    fn statement_end(&self, i: usize) -> usize {
        (i..self.tokens.len())
            .find(|&j| {
                self.ends.contains(&j)
                    || self.tokens.get(j + 1).is_some_and(|t| matches!(t.kind, Kind::Go | Kind::Raw))
            })
            .unwrap_or(self.tokens.len() - 1)
    }

    /// Prints the DML statement starting at `i` on one line when it is short enough.
    fn collapse_statement(&mut self, i: usize) {
        if !self.is_root() || self.flat.is_some() {
            return;
        }
        if let Some(width) = self.l.collapse_statements {
            let end = self.statement_end(i);
            if self.flat_len(i, end) < width {
                self.flat = Some((i, end));
            }
        }
    }

    fn start_statement(&mut self, word: &str) {
        if self.is_root() {
            self.extra = if self.if_header && word != "BEGIN" { self.l.indent } else { 0 };
            self.if_header = false;
            self.stmt = word.to_string();
            self.module = false;
            self.create_table = false;
        }
        let f = self.frame();
        f.clause = Clause::None;
        f.joined = false;
        f.between = 0;
        f.awaiting = Awaiting::Nothing;
    }

    fn end_statement(&mut self) {
        if self.is_root() {
            self.stmt.clear();
            self.extra = 0;
            self.module = false;
            self.if_header = false;
            self.create_table = false;
        }
        let f = self.frame();
        f.clause = Clause::None;
        f.joined = false;
        f.awaiting = Awaiting::Nothing;
    }

    fn case_rules(&mut self, i: usize, w: &str) -> Option<usize> {
        let indent = self.l.indent;
        let case = self.top().cases.last().expect("caller checked");
        let (col, when_col, simple, flat) = (case.col, case.when_col, case.simple, case.flat);
        let when_or_default = when_col.unwrap_or(col + indent);
        match w {
            "CASE" => {
                self.mark_case(i);
                None
            }
            "WHEN" => {
                self.mark = Mark::When;
                if flat {
                    return None;
                }
                match when_col {
                    Some(c) => Some(c),
                    None => {
                        let first = match self.l.case_first_when {
                            ListBreak::Always => true,
                            ListBreak::Never => false,
                            ListBreak::IfMultiple => simple,
                        };
                        first.then_some(col + if self.l.case_when_indented { indent } else { 0 })
                    }
                }
            }
            "THEN" if !flat && self.l.case_then_on_new_line => {
                Some(when_or_default + if self.l.case_then_indented { indent } else { 0 })
            }
            "ELSE" if !flat && self.l.case_else_on_new_line => {
                Some(if self.l.case_else_to_when { when_or_default } else { col + indent })
            }
            "END" => {
                self.frame().cases.pop();
                (!flat && self.l.case_end_on_new_line)
                    .then_some(if self.l.case_end_to_when { when_or_default } else { col })
            }
            _ => None,
        }
    }

    fn mark_case(&mut self, i: usize) {
        let simple = next_sig(self.tokens, i).is_some_and(|n| self.up[n] != "WHEN");
        let flat = self.l.collapse_case.is_some_and(|w| self.flat_len(i, self.case_end(i)) < w);
        self.mark = Mark::Case { flat, simple };
    }

    /// Updates statement state for a word and returns the column to break at, if any.
    fn word_rules(&mut self, i: usize) -> Option<usize> {
        if self.top().kind != FrameKind::Query {
            return None;
        }
        let w = self.up[i].clone();
        let p = prev_sig(self.tokens, i).map_or(String::new(), |j| self.up[j].clone());
        let n = next_sig(self.tokens, i).map_or(String::new(), |j| self.up[j].clone());
        if w == "TABLE" && p == "CREATE" {
            self.create_table = true;
        }
        if !self.top().cases.is_empty() {
            return self.case_rules(i, &w);
        }
        let indent = self.l.indent;

        match w.as_str() {
            "CASE" => {
                self.mark_case(i);
                None
            }
            "SELECT" => {
                let continues = matches!(p.as_str(), "UNION" | "ALL" | "EXCEPT" | "INTERSECT" | "FOR" | "AS")
                    || matches!(self.stmt.as_str(), "INSERT" | "WITH");
                if !continues {
                    self.start_statement(&w);
                    self.collapse_statement(i);
                } else if self.is_root() {
                    self.stmt = w.clone();
                }
                self.mark = Mark::Clause(Clause::Select, Awaiting::ListItem, 6);
                Some(self.clause_col(&w))
            }
            "INSERT" | "UPDATE" | "DELETE" | "MERGE" => {
                let guarded = matches!(p.as_str(), "ON" | "AFTER" | "FOR" | "OF" | "THEN" | "," | "GRANT" | "DENY" | "REVOKE" | "BULK")
                    || JOIN_WORDS.contains(&p.as_str())
                    || (n == "(" && w == "UPDATE")
                    || n == "JOIN";
                if !guarded {
                    if self.stmt == "WITH" && self.is_root() {
                        self.stmt = w.clone();
                    } else {
                        self.start_statement(&w);
                        self.collapse_statement(i);
                    }
                }
                let clause = if w == "INSERT" { Clause::Insert } else { Clause::Other };
                if guarded {
                    if w == "INSERT" {
                        self.frame().clause = Clause::Insert;
                    }
                    return None;
                }
                self.mark = Mark::Clause(clause, Awaiting::Nothing, w.len());
                Some(self.clause_col(&w))
            }
            "SET" => {
                if matches!(p.as_str(), "DELETE" | "UPDATE") && self.stmt != "UPDATE" {
                    return None;
                }
                if matches!(self.stmt.as_str(), "ALTER" | "CREATE") {
                    return None;
                }
                if !(self.stmt == "UPDATE" || !self.is_root()) {
                    self.start_statement(&w);
                }
                self.mark = Mark::Clause(Clause::Set, Awaiting::SetItem, 3);
                Some(self.clause_col(&w))
            }
            "FROM" => {
                self.mark = Mark::Clause(Clause::From, Awaiting::FromTable, 4);
                (p != "DELETE").then(|| self.clause_col(&w))
            }
            "WHERE" | "HAVING" => {
                self.frame().joined = false;
                self.mark = Mark::Clause(Clause::Where, Awaiting::Condition, w.len());
                Some(self.clause_col(&w))
            }
            "GROUP" | "ORDER" if n == "BY" => {
                self.mark = Mark::Clause(Clause::GroupOrder, Awaiting::Nothing, w.len());
                Some(self.clause_col(&w))
            }
            "BY" if matches!(p.as_str(), "GROUP" | "ORDER") && self.top().clause == Clause::GroupOrder => {
                self.mark = Mark::Await(Awaiting::GroupItem);
                None
            }
            "UNION" | "EXCEPT" | "INTERSECT" => {
                self.mark = Mark::Clause(Clause::None, Awaiting::Nothing, w.len());
                Some(self.clause_col(&w))
            }
            "VALUES" if p != "DEFAULT" => {
                self.mark = Mark::Clause(Clause::Other, Awaiting::Nothing, w.len());
                Some(self.clause_col(&w))
            }
            "OPTION" => {
                self.mark = Mark::Clause(Clause::Other, Awaiting::Nothing, w.len());
                Some(self.clause_col(&w))
            }
            "INTO" if self.top().clause == Clause::Select => {
                self.mark = Mark::Clause(Clause::Other, Awaiting::Nothing, w.len());
                Some(self.clause_col(&w))
            }
            "FOR" if matches!(n.as_str(), "XML" | "JSON" | "BROWSE") => Some(self.base()),
            _ if JOIN_WORDS.contains(&w.as_str())
                && n != "("
                && self.top().clause == Clause::From
                && !JOIN_WORDS.contains(&p.as_str()) =>
            {
                self.join_break()
            }
            "JOIN" if self.top().clause == Clause::From => {
                let hinted = JOIN_WORDS.contains(&p.as_str())
                    || matches!(p.as_str(), "HASH" | "MERGE" | "LOOP" | "REMOTE");
                if hinted {
                    self.frame().joined = true;
                    return None;
                }
                self.join_break()
            }
            "USING" if self.stmt == "MERGE" => {
                self.frame().clause = Clause::From;
                self.frame().joined = true;
                self.mark = Mark::Join;
                Some(self.base())
            }
            "ON" if self.top().joined => {
                self.mark = Mark::On;
                let join_col = self.top().join_col;
                self.l.on_on_new_line.then_some(join_col + if self.l.indent_on { indent } else { 0 })
            }
            "WHEN" if self.stmt == "MERGE" => {
                self.frame().clause = Clause::Other;
                self.frame().joined = false;
                Some(self.base())
            }
            "BETWEEN" => {
                self.frame().between += 1;
                None
            }
            "AND" if self.top().between > 0 => {
                self.frame().between -= 1;
                None
            }
            "AND" | "OR" => self.and_or_break(&w),
            "AS" if self.is_root() && self.stmt == "WITH" && n == "(" && self.top().clause == Clause::None => {
                let name_col = self.top().cte_name_col.unwrap_or(self.base());
                self.l.cte_as_on_new_line.then_some(name_col)
            }
            "AS" if self.is_root() && self.module && !matches!(p.as_str(), "EXEC" | "EXECUTE") => {
                self.module = false;
                self.stmt.clear();
                self.pending = Some(self.base());
                Some(self.base())
            }
            "WITH"
                if n != "("
                    && (matches!(p.as_str(), "" | ";" | "BEGIN" | "AS" | "ELSE")
                        || (self.is_root() && self.stmt.is_empty())) =>
            {
                self.start_statement(&w);
                self.collapse_statement(i);
                self.mark = Mark::Clause(Clause::None, Awaiting::CteName, 4);
                Some(self.base())
            }
            _ => self.control_flow(&w, &p, &n),
        }
    }

    fn join_break(&mut self) -> Option<usize> {
        self.frame().joined = true;
        self.mark = Mark::Join;
        let col = self.top().clause_col + if self.l.indent_join { self.l.indent } else { 0 };
        self.l.join_on_new_line.then_some(col)
    }

    fn and_or_break(&self, w: &str) -> Option<usize> {
        let f = self.top();
        let on_condition = f.clause == Clause::From && f.joined;
        if !self.l.and_or_on_new_line || !(f.clause == Clause::Where || on_condition) {
            return None;
        }
        let align = if on_condition { self.l.on_and_or_align } else { self.l.and_or_align };
        let (kw_col, kw_len) = f.cond_kw;
        let cond = f.cond_col.unwrap_or(kw_col + kw_len + 1);
        Some(match align {
            AndOrAlign::Left => kw_col,
            AndOrAlign::Indented => kw_col + self.l.indent,
            AndOrAlign::Right => (kw_col + kw_len).saturating_sub(w.len()),
            AndOrAlign::BeforeFirstItem => cond.saturating_sub(w.len() + 1),
            AndOrAlign::ToFirstItem => cond,
        })
    }

    fn control_flow(&mut self, w: &str, p: &str, n: &str) -> Option<usize> {
        if !self.is_root() {
            return None;
        }
        let indent = self.l.indent;
        match w {
            "BEGIN" if matches!(n, "TRAN" | "TRANSACTION" | "DISTRIBUTED" | "DIALOG" | "CONVERSATION") => {
                self.start_statement(w);
                Some(self.base())
            }
            "BEGIN" => {
                let shift = if self.if_header {
                    if self.l.indent_begin_end { indent } else { 0 }
                } else {
                    self.extra
                };
                let col = self.block_content() + shift;
                self.end_statement();
                self.blocks.push(col);
                if !matches!(n, "TRY" | "CATCH") {
                    self.pending = Some(self.base());
                }
                Some(col)
            }
            "TRY" | "CATCH" if matches!(p, "BEGIN" | "END") => {
                self.pending = Some(self.base());
                None
            }
            "END" if n != "CONVERSATION" => {
                let col = self.blocks.pop().unwrap_or(0);
                self.end_statement();
                if !matches!(n, "TRY" | "CATCH") {
                    self.pending = Some(self.base());
                }
                Some(col)
            }
            "ELSE" => {
                self.end_statement();
                self.if_header = true;
                let col = self.base();
                if n != "IF" {
                    self.pending = Some(col);
                }
                Some(col)
            }
            "IF" if n == "EXISTS" && matches!(self.stmt.as_str(), "DROP" | "ALTER") => None,
            "IF" | "WHILE" if p == "ELSE" => {
                self.stmt = w.to_string();
                self.if_header = true;
                None
            }
            "IF" | "WHILE" => {
                self.start_statement(w);
                self.if_header = true;
                Some(self.base())
            }
            _ if STATEMENTS.contains(&w) => {
                let guarded = matches!(p, "," | "GRANT" | "DENY" | "REVOKE" | "WITH" | "ROW" | "ROWS" | "OR")
                    || (matches!(w, "ALTER" | "DROP") && self.stmt == "ALTER");
                if guarded {
                    return None;
                }
                self.start_statement(w);
                if matches!(w, "CREATE" | "ALTER") {
                    self.module = matches!(n, "PROC" | "PROCEDURE" | "FUNCTION" | "VIEW" | "TRIGGER" | "OR");
                }
                Some(self.base())
            }
            _ => None,
        }
    }

    fn open_paren(&mut self, i: usize) {
        let prev = prev_sig(self.tokens, i);
        let prev_up = prev.map_or("", |p| self.up[p].as_str());
        let next_is_select = next_sig(self.tokens, i).is_some_and(|n| self.up[n] == "SELECT");
        let in_query = self.top().kind == FrameKind::Query;
        let cte_body = in_query && self.is_root() && self.stmt == "WITH" && prev_up == "AS";
        let names_table = prev.is_some_and(|p| {
            matches!(self.tokens[p].kind, Kind::Bracket | Kind::Quoted)
                || (self.tokens[p].kind == Kind::Word && !is_reserved(&self.up[p]))
        });
        let insert_columns = in_query && self.top().clause == Clause::Insert && names_table;
        let definitions = in_query && (self.create_table || prev_up == "TABLE");
        let short = |limit: Option<usize>| limit.is_some_and(|w| self.flat_len(i, self.matching_paren(i)) < w);

        let (kind, style) = if next_is_select {
            if cte_body {
                (FrameKind::Query, self.l.cte_parens)
            } else if short(self.l.collapse_subqueries) {
                (FrameKind::Inline, ParenStyle::Compact)
            } else {
                (FrameKind::Query, self.l.subquery_parens)
            }
        } else if definitions {
            if short(self.l.collapse_ddl) {
                (FrameKind::Inline, ParenStyle::Compact)
            } else {
                (FrameKind::List, self.l.ddl_parens)
            }
        } else if insert_columns {
            match self.l.insert_columns {
                Some(style) => (FrameKind::List, style),
                None => (FrameKind::Inline, ParenStyle::Compact),
            }
        } else {
            (FrameKind::Inline, ParenStyle::Compact)
        };

        self.emit(i, None);
        self.create_table = false;
        let open_indent = self.line_indent;
        let inner = open_indent + self.l.indent;
        let (base, close_col, pending) = match (kind, style) {
            (FrameKind::Inline, _) | (_, ParenStyle::Compact) => (self.col, None, None),
            (_, ParenStyle::ExpandedToStatement) => (inner, Some(open_indent), Some(inner)),
            (_, ParenStyle::ExpandedIndented) => (inner, Some(inner), Some(inner)),
        };
        let fresh = kind != FrameKind::Inline && style == ParenStyle::Compact;
        self.frames.push(Frame::new(kind, base, close_col, fresh));
        self.pending = pending;
    }

    fn close_paren(&mut self, i: usize) {
        if self.is_root() {
            self.emit(i, None);
            return;
        }
        let frame = self.frames.pop().expect("checked above");
        self.pending = None;
        self.emit(i, frame.close_col);
    }

    fn comma(&mut self, i: usize) {
        let f = self.top();
        let indent = self.l.indent;
        let list_col = |fallback: usize| {
            if self.l.align_items_with_first { f.list_col.unwrap_or(fallback) } else { f.clause_col + indent }
        };
        let col = if !f.cases.is_empty() {
            None
        } else {
            match (f.kind, f.clause) {
                (FrameKind::Query, Clause::Select) if self.l.list_items_on_new_lines => Some(list_col(f.clause_col + 7)),
                (FrameKind::Query, Clause::GroupOrder) if self.l.break_group_order_lists && self.l.list_items_on_new_lines => {
                    Some(list_col(f.clause_col + 9))
                }
                (FrameKind::Query, Clause::Set) => Some(f.list_col.unwrap_or(f.clause_col + 4)),
                (FrameKind::Query, Clause::None)
                    if self.is_root() && self.stmt == "WITH" && self.l.cte_name_on_new_line =>
                {
                    f.cte_name_col
                }
                (FrameKind::List, _) => Some(f.base),
                _ => None,
            }
        };
        match col {
            Some(col) if self.l.leading_commas => self.emit(i, Some(col.saturating_sub(2))),
            Some(col) => {
                self.emit(i, None);
                self.pending = Some(col);
            }
            None => {
                self.emit(i, None);
                self.glue = !self.l.space_after_comma;
            }
        }
    }

    fn operator(&mut self, i: usize) {
        let text = self.tokens[i].text.as_str();
        let prev = self.last.map(|l| (&self.tokens[l], self.up[l].as_str()));
        let unary = matches!(text, "-" | "+" | "~")
            && prev.is_none_or(|(t, up)| {
                matches!(t.kind, Kind::Op | Kind::LParen | Kind::Comma) || (t.kind == Kind::Word && is_reserved(up))
            })
            && self.tokens.get(i + 1).is_some_and(|n| n.kind != Kind::Op && !n.is_comment());
        let after_operand = prev.is_some_and(|(t, up)| {
            matches!(t.kind, Kind::Number | Kind::Variable | Kind::RParen | Kind::Bracket | Kind::Quoted | Kind::Str)
                || (t.kind == Kind::Word && !is_reserved(up))
        });
        // `=` after a variable or in SET/DECLARE assigns rather than compares.
        let assignment = text == "="
            && (prev.is_some_and(|(t, _)| t.kind == Kind::Variable)
                || self.top().clause == Clause::Set
                || (self.is_root() && self.stmt == "DECLARE"));
        let comparison = !assignment && matches!(text, "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" | "!<" | "!>");
        let arithmetic = !unary && after_operand && matches!(text, "+" | "-" | "*" | "/" | "%");
        let tight = (comparison && !self.l.spaces_around_comparison) || (arithmetic && !self.l.spaces_around_arithmetic);
        // Never glue two operators: `- -1` must not become a `--` comment.
        let next_is_op = self.tokens.get(i + 1).is_some_and(|n| n.kind == Kind::Op);
        if tight {
            self.glue = true;
        }
        self.emit(i, None);
        self.glue = (unary || tight) && !next_is_op;
    }

    fn comment(&mut self, i: usize) {
        let t = &self.tokens[i];
        if t.newlines_before() > 0 || self.last.is_none() {
            let col = self.pending.unwrap_or(if self.is_root() { self.base() } else { self.line_indent });
            let blanks = self.blank_lines(i);
            self.newline(blanks);
            self.line_indent = col;
            self.write(i, false);
        } else {
            self.write(i, true);
        }
        if t.kind == Kind::LineComment {
            self.newline(0);
        } else if self.pending.is_none() && self.tokens.get(i + 1).is_some_and(|n| n.newlines_before() > 0) {
            self.pending = Some(self.line_indent);
        }
    }

    fn own_line_directive(&mut self, i: usize) {
        self.pending = None;
        let existing = self.tokens[i].newlines_before().saturating_sub(1);
        let blanks = if self.l.preserve_blank_lines { existing.min(1) } else { 0 };
        self.newline(blanks);
        self.line_indent = 0;
        self.write(i, false);
        self.frames.truncate(1);
        self.frames[0] = Frame::new(FrameKind::Query, 0, None, false);
        self.blocks.clear();
        self.extra = 0;
        self.if_header = false;
        self.stmt.clear();
        self.module = false;
        self.create_table = false;
        self.flat = None;
        self.after_go = true;
        self.pending = Some(0);
    }

    fn semicolon(&mut self, i: usize) {
        self.pending = None;
        let brk = (self.l.semicolon_space == SemicolonSpace::NewLineBefore).then(|| self.base());
        self.emit(i, brk);
        if self.top().kind == FrameKind::Query {
            self.end_statement();
            self.pending = Some(self.base());
        }
    }

    fn run(mut self, trailing_ws: &str) -> String {
        for i in 0..self.tokens.len() {
            match self.tokens[i].kind {
                Kind::Go | Kind::Raw => self.own_line_directive(i),
                Kind::LineComment | Kind::BlockComment => self.comment(i),
                Kind::LParen => self.open_paren(i),
                Kind::RParen => self.close_paren(i),
                Kind::Comma => self.comma(i),
                Kind::Op => self.operator(i),
                Kind::Semi => self.semicolon(i),
                Kind::Word => {
                    // `pending` holds the break queued for this word; a rule may queue one for the next.
                    let queued = self.pending.take();
                    let brk = self.word_rules(i);
                    let next = self.pending.take();
                    self.emit(i, brk.or(queued));
                    self.apply_mark();
                    if next.is_some() {
                        self.pending = next;
                    }
                }
                _ => self.emit(i, None),
            }
        }
        // Keep the input's trailing newline, or lack of one.
        let wants_newline = trailing_ws.contains('\n');
        if wants_newline && !self.at_start {
            self.out.push_str(self.nl);
        } else if !wants_newline && self.at_start && self.out.ends_with(self.nl) {
            self.out.truncate(self.out.len() - self.nl.len());
        }
        self.out
    }
}

pub fn render(tokens: &[Token], trailing_ws: &str, layout: &Layout, nl: &str) -> String {
    let mut ends: HashSet<usize> = missing_terminators(tokens).into_iter().collect();
    ends.extend(tokens.iter().enumerate().filter(|(_, t)| t.kind == Kind::Semi).map(|(i, _)| i));
    let writer = Writer {
        tokens,
        up: upper_words(tokens),
        l: layout,
        nl,
        out: String::new(),
        col: 0,
        at_start: true,
        line_indent: 0,
        pending: None,
        glue: false,
        last: None,
        last_start: 0,
        frames: vec![Frame::new(FrameKind::Query, 0, None, false)],
        blocks: Vec::new(),
        extra: 0,
        if_header: false,
        stmt: String::new(),
        module: false,
        create_table: false,
        flat: None,
        ends,
        mark: Mark::None,
        after_go: false,
    };
    writer.run(trailing_ws)
}
