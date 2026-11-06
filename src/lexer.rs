//! T-SQL lexer. Every token keeps the whitespace in front of it verbatim, so
//! concatenating `ws + text` reproduces the input exactly.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Word,
    Bracket,
    Quoted,
    Str,
    Number,
    Variable,
    Op,
    Comma,
    Semi,
    Dot,
    LParen,
    RParen,
    LineComment,
    BlockComment,
    /// `GO` batch separator; must stay alone on its line.
    Go,
    /// sqlcmd directive such as `:setvar`; must stay alone on its line.
    Raw,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Kind,
    pub text: String,
    pub ws: String,
}

impl Token {
    pub fn synth(kind: Kind, text: &str, ws: &str) -> Self {
        Token { kind, text: text.to_string(), ws: ws.to_string() }
    }

    pub fn is_comment(&self) -> bool {
        matches!(self.kind, Kind::LineComment | Kind::BlockComment)
    }

    pub fn newlines_before(&self) -> usize {
        self.ws.matches('\n').count()
    }
}

#[derive(Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub trailing_ws: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct LexError {
    pub line: usize,
    pub what: &'static str,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unterminated {} starting on line {}", self.what, self.line)
    }
}

const TWO_CHAR_OPS: &[&str] = &[
    "<=", ">=", "<>", "!=", "!<", "!>", "+=", "-=", "*=", "/=", "%=", "&=", "^=", "|=", "::",
];

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '#' || c == '$'
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '#' | '$' | '@')
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek2(&self) -> Option<char> {
        let mut it = self.rest().chars();
        it.next();
        it.next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn at_line_start(&self) -> bool {
        let before = &self.src[..self.pos];
        match before.rfind('\n') {
            Some(i) => before[i + 1..].trim().is_empty(),
            None => before.trim().is_empty(),
        }
    }

    /// Consumes a delimited run where a doubled `close` is an escape.
    fn delimited(&mut self, close: char, what: &'static str) -> Result<(), LexError> {
        let line = self.line;
        loop {
            match self.bump() {
                None => return Err(LexError { line, what }),
                Some(c) if c == close => {
                    if self.peek() == Some(close) {
                        self.bump();
                    } else {
                        return Ok(());
                    }
                }
                Some(_) => {}
            }
        }
    }

    fn block_comment(&mut self) -> Result<(), LexError> {
        let line = self.line;
        self.bump();
        self.bump();
        let mut depth = 1;
        while depth > 0 {
            if self.rest().starts_with("/*") {
                self.bump();
                self.bump();
                depth += 1;
            } else if self.rest().starts_with("*/") {
                self.bump();
                self.bump();
                depth -= 1;
            } else if self.bump().is_none() {
                return Err(LexError { line, what: "block comment" });
            }
        }
        Ok(())
    }

    fn skip_to_end_of_line(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' || (c == '\r' && self.peek2() == Some('\n')) {
                break;
            }
            self.bump();
        }
    }

    /// Word body, including embedded sqlcmd `$(var)` references.
    fn word(&mut self) -> Result<(), LexError> {
        while let Some(c) = self.peek() {
            if c == '$' && self.peek2() == Some('(') {
                self.bump();
                self.bump();
                self.delimited(')', "sqlcmd variable")?;
            } else if is_word_char(c) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(())
    }

    fn number(&mut self) {
        if self.rest().starts_with("0x") || self.rest().starts_with("0X") {
            self.bump();
            self.bump();
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.bump();
            }
            return;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.bump();
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let mut it = self.rest().chars().skip(1);
            let first = it.next();
            let second = it.next();
            let has_exponent = match first {
                Some('+' | '-') => second.is_some_and(|c| c.is_ascii_digit()),
                Some(c) => c.is_ascii_digit(),
                None => false,
            };
            if has_exponent {
                self.bump();
                self.bump();
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.bump();
                }
            }
        }
    }

    /// For a line-initial `GO` ending at `after_word`: the end of its token when the rest
    /// of the line is only an optional repeat count and comment, else `None`.
    fn go_token_end(&self, after_word: usize) -> Option<usize> {
        let tail = &self.src[after_word..];
        let line = &tail[..tail.find('\n').unwrap_or(tail.len())];
        let trimmed = line.trim_start_matches([' ', '\t']);
        let count_len = trimmed.chars().take_while(char::is_ascii_digit).count();
        let rest = trimmed[count_len..].trim();
        if !rest.is_empty() && !rest.starts_with("--") {
            return None;
        }
        if count_len == 0 {
            Some(after_word)
        } else {
            Some(after_word + (line.len() - trimmed.len()) + count_len)
        }
    }
}

pub fn lex(src: &str) -> Result<Lexed, LexError> {
    let mut tokens = Vec::new();
    let trailing_ws = lex_into(src, &mut tokens)?;
    Ok(Lexed { tokens, trailing_ws })
}

/// Lexes as far as possible. On an unterminated literal, comment or identifier it returns
/// the tokens before it, the error, and the byte offset where the bad token starts.
pub fn lex_partial(src: &str) -> (Vec<Token>, Option<(LexError, usize)>) {
    let mut tokens = Vec::new();
    match lex_into(src, &mut tokens) {
        Ok(_) => (tokens, None),
        Err(e) => {
            let consumed: usize = tokens.iter().map(|t| t.ws.len() + t.text.len()).sum();
            let rest = &src[consumed..];
            let offset = consumed + (rest.len() - rest.trim_start().len());
            (tokens, Some((e, offset)))
        }
    }
}

fn lex_into(src: &str, tokens: &mut Vec<Token>) -> Result<String, LexError> {
    let mut lx = Lexer { src, pos: 0, line: 1 };
    loop {
        let ws_start = lx.pos;
        while lx.peek().is_some_and(char::is_whitespace) {
            lx.bump();
        }
        let ws = src[ws_start..lx.pos].to_string();
        let Some(c) = lx.peek() else {
            return Ok(ws);
        };
        let start = lx.pos;
        let c2 = lx.peek2();
        let line_start = lx.at_line_start();
        let kind = match c {
            '-' if c2 == Some('-') => {
                lx.skip_to_end_of_line();
                Kind::LineComment
            }
            '/' if c2 == Some('*') => {
                lx.block_comment()?;
                Kind::BlockComment
            }
            ':' if line_start && c2.is_some_and(char::is_alphabetic) => {
                lx.skip_to_end_of_line();
                Kind::Raw
            }
            '!' if line_start && c2 == Some('!') => {
                lx.skip_to_end_of_line();
                Kind::Raw
            }
            '\'' => {
                lx.bump();
                lx.delimited('\'', "string literal")?;
                Kind::Str
            }
            'N' | 'n' if c2 == Some('\'') => {
                lx.bump();
                lx.bump();
                lx.delimited('\'', "string literal")?;
                Kind::Str
            }
            '[' => {
                lx.bump();
                lx.delimited(']', "bracketed identifier")?;
                Kind::Bracket
            }
            '"' => {
                lx.bump();
                lx.delimited('"', "quoted identifier")?;
                Kind::Quoted
            }
            '@' => {
                lx.bump();
                lx.word()?;
                Kind::Variable
            }
            ',' => {
                lx.bump();
                Kind::Comma
            }
            ';' => {
                lx.bump();
                Kind::Semi
            }
            '(' => {
                lx.bump();
                Kind::LParen
            }
            ')' => {
                lx.bump();
                Kind::RParen
            }
            '.' if c2.is_some_and(|d| d.is_ascii_digit())
                && !matches!(
                    tokens.last().map(|t| t.kind),
                    Some(Kind::Word | Kind::Bracket | Kind::Quoted)
                ) =>
            {
                lx.number();
                Kind::Number
            }
            '.' => {
                lx.bump();
                Kind::Dot
            }
            '$' if c2.is_some_and(|d| d.is_ascii_digit() || d == '.') => {
                lx.bump();
                lx.number();
                Kind::Number
            }
            c if c.is_ascii_digit() => {
                lx.number();
                Kind::Number
            }
            c if is_word_start(c) => {
                lx.word()?;
                let is_go = line_start && src[start..lx.pos].eq_ignore_ascii_case("GO");
                match is_go.then(|| lx.go_token_end(lx.pos)).flatten() {
                    Some(end) => {
                        while lx.pos < end {
                            lx.bump();
                        }
                        Kind::Go
                    }
                    None => Kind::Word,
                }
            }
            _ => {
                let two = TWO_CHAR_OPS.iter().any(|op| lx.rest().starts_with(op));
                lx.bump();
                if two {
                    lx.bump();
                }
                Kind::Op
            }
        };
        tokens.push(Token { kind, text: src[start..lx.pos].to_string(), ws });
    }
}
