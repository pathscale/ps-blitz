use std::char;

const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOKENS: usize = 1_000_000;
const MAX_NESTING: usize = 512;

#[derive(Clone, Debug)]
pub enum Literal {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Vec<u16>),
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Ident(String),
    Array(Vec<Option<Pattern>>),
    Object(Vec<(String, Pattern)>),
    Rest(Box<Pattern>),
    Default(Box<Pattern>, Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct FunctionExpr {
    pub params: Vec<Pattern>,
    pub body: Vec<Stmt>,
    pub arrow: bool,
    pub asynchronous: bool,
    pub name: Option<String>,
    pub source_offset: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ClassExpr {
    pub name: Option<String>,
    pub extends: Option<Box<Expr>>,
    pub methods: Vec<ClassMethod>,
    pub fields: Vec<(String, Option<Expr>, bool)>,
}

#[derive(Clone, Debug)]
pub struct ClassMethod {
    pub name: String,
    pub function: FunctionExpr,
    pub getter: bool,
    pub setter: bool,
    pub is_static: bool,
}

#[derive(Clone, Debug)]
pub enum ObjectEntry {
    Property(String, Expr),
    Computed(Expr, Expr),
    Spread(Expr),
    Method(String, FunctionExpr),
    Getter(String, FunctionExpr),
    Setter(String, FunctionExpr),
}

#[derive(Clone, Debug)]
pub enum TemplatePart {
    Text(Vec<u16>),
    Expression(Expr),
}

#[derive(Clone, Debug)]
pub enum ChainPart {
    Member(Expr, bool),
    Call(Vec<Expr>, bool),
}

#[derive(Clone, Debug)]
pub enum Expr {
    Literal(Literal),
    Ident(String),
    This,
    Array(Vec<Option<Expr>>),
    Object(Vec<ObjectEntry>),
    Function(FunctionExpr),
    Class(ClassExpr),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Assign(String, Box<Expr>, Box<Expr>),
    Update(String, Box<Expr>, bool),
    Member(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    New(Box<Expr>, Vec<Expr>),
    Sequence(Vec<Expr>),
    Await(Box<Expr>),
    Spread(Box<Expr>),
    Template(Vec<TemplatePart>),
    Chain(Box<Expr>, Vec<ChainPart>),
}

#[derive(Clone, Debug)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Empty,
    Expr(Expr),
    Block(Vec<Stmt>),
    Var(VarKind, Vec<(Pattern, Option<Expr>)>),
    FunctionDecl(String, FunctionExpr),
    ClassDecl(String, ClassExpr),
    Return(Option<Expr>),
    Throw(Expr),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    For {
        init: Option<Box<Stmt>>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    ForEach {
        pattern: Pattern,
        kind: Option<VarKind>,
        iterable: Expr,
        body: Box<Stmt>,
        keys: bool,
    },
    Try {
        body: Box<Stmt>,
        catch: Option<(Option<Pattern>, Box<Stmt>)>,
        finally: Option<Box<Stmt>>,
    },
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Break,
    Continue,
}

#[derive(Clone, Debug)]
enum TemplateLex {
    Text(Vec<u16>),
    Expr(String, usize),
}

#[derive(Clone, Debug)]
enum Kind {
    Ident(String),
    Number(f64),
    String(Vec<u16>),
    Template(Vec<TemplateLex>),
    Regex(String, String),
    Punct(String),
    Eof,
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    at: usize,
    line_break_before: bool,
}

struct Lexer<'a> {
    source: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    previous_line: bool,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            tokens: Vec::new(),
            previous_line: false,
        }
    }

    fn tokenize(mut self) -> Result<Vec<Token>, String> {
        if self.source.len() > MAX_SOURCE_BYTES {
            return Err(String::from(
                "SyntaxError at byte 0: source size limit exceeded",
            ));
        }
        while self.pos < self.source.len() {
            self.skip_space_and_comments()?;
            if self.pos == self.source.len() {
                break;
            }
            let at = self.pos;
            let ch = self
                .peek()
                .ok_or_else(|| self.error(at, "invalid character boundary"))?;
            let kind = if ch == '\'' || ch == '"' {
                Kind::String(self.read_string(ch)?)
            } else if ch == '`' {
                Kind::Template(self.read_template()?)
            } else if ch.is_ascii_digit()
                || (ch == '.' && self.peek_after(1).is_some_and(|c| c.is_ascii_digit()))
            {
                Kind::Number(self.read_number()?)
            } else if ch == '/' && self.regex_allowed() {
                let (source, flags) = self.read_regex()?;
                Kind::Regex(source, flags)
            } else if is_ident_start(ch) {
                Kind::Ident(self.read_identifier())
            } else {
                Kind::Punct(self.read_punct()?)
            };
            self.tokens.push(Token {
                kind,
                at,
                line_break_before: std::mem::take(&mut self.previous_line),
            });
            if self.tokens.len() > MAX_TOKENS {
                return Err(self.error(at, "token limit exceeded"));
            }
        }
        self.tokens.push(Token {
            kind: Kind::Eof,
            at: self.source.len(),
            line_break_before: self.previous_line,
        });
        Ok(self.tokens)
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.pos..)?.chars().next()
    }

    fn regex_allowed(&self) -> bool {
        match self.tokens.last().map(|token| &token.kind) {
            None => true,
            Some(Kind::Punct(token)) => matches!(
                token.as_str(),
                "(" | "["
                    | "{"
                    | ","
                    | ":"
                    | ";"
                    | "="
                    | "=>"
                    | "?"
                    | "!"
                    | "~"
                    | "&&"
                    | "||"
                    | "??"
                    | "+"
                    | "-"
                    | "*"
                    | "%"
                    | "=="
                    | "==="
                    | "!="
                    | "!=="
                    | "&"
                    | "|"
                    | "^"
            ),
            Some(Kind::Ident(token)) => matches!(
                token.as_str(),
                "return" | "throw" | "case" | "typeof" | "void" | "delete" | "in" | "of" | "await"
            ),
            _ => false,
        }
    }

    fn read_regex(&mut self) -> Result<(String, String), String> {
        let at = self.pos;
        self.bump();
        let mut source = String::new();
        let mut class = false;
        while let Some(ch) = self.bump() {
            match ch {
                '\n' | '\r' => return Err(self.error(at, "unterminated regular expression")),
                '\\' => {
                    source.push(ch);
                    source.push(
                        self.bump().ok_or_else(|| {
                            self.error(at, "unterminated regular expression escape")
                        })?,
                    );
                }
                '[' => {
                    class = true;
                    source.push(ch);
                }
                ']' => {
                    class = false;
                    source.push(ch);
                }
                '/' if !class => {
                    let mut flags = String::new();
                    while self.peek().is_some_and(is_ident_start) {
                        if let Some(flag) = self.bump() {
                            flags.push(flag);
                        }
                    }
                    return Ok((source, flags));
                }
                _ => source.push(ch),
            }
        }
        Err(self.error(at, "unterminated regular expression"))
    }

    fn peek_after(&self, chars: usize) -> Option<char> {
        self.source.get(self.pos..)?.chars().nth(chars)
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn starts(&self, text: &str) -> bool {
        self.source
            .get(self.pos..)
            .is_some_and(|tail| tail.starts_with(text))
    }

    fn skip_space_and_comments(&mut self) -> Result<(), String> {
        loop {
            let Some(ch) = self.peek() else { return Ok(()) };
            if ch.is_whitespace() || ch == '\u{feff}' {
                self.previous_line |=
                    ch == '\n' || ch == '\r' || ch == '\u{2028}' || ch == '\u{2029}';
                self.bump();
            } else if self.starts("//") {
                self.pos += 2;
                while let Some(next) = self.peek() {
                    if next == '\n' || next == '\r' || next == '\u{2028}' || next == '\u{2029}' {
                        break;
                    }
                    self.bump();
                }
            } else if self.starts("/*") {
                let at = self.pos;
                self.pos += 2;
                let mut closed = false;
                while self.pos < self.source.len() {
                    if self.starts("*/") {
                        self.pos += 2;
                        closed = true;
                        break;
                    }
                    if let Some(next) = self.bump() {
                        self.previous_line |= next == '\n'
                            || next == '\r'
                            || next == '\u{2028}'
                            || next == '\u{2029}';
                    }
                }
                if !closed {
                    return Err(self.error(at, "unterminated block comment"));
                }
            } else {
                return Ok(());
            }
        }
    }

    fn read_identifier(&mut self) -> String {
        let start = self.pos;
        self.bump();
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        self.source[start..self.pos].to_owned()
    }

    fn read_number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        if self.starts("0x")
            || self.starts("0X")
            || self.starts("0b")
            || self.starts("0B")
            || self.starts("0o")
            || self.starts("0O")
        {
            let radix = match self.source.as_bytes()[self.pos + 1] {
                b'x' | b'X' => 16,
                b'b' | b'B' => 2,
                _ => 8,
            };
            self.pos += 2;
            let digits = self.pos;
            while self
                .peek()
                .is_some_and(|c| c.is_ascii_hexdigit() || c == '_')
            {
                self.bump();
            }
            let clean: String = self.source[digits..self.pos]
                .chars()
                .filter(|c| *c != '_')
                .collect();
            if clean.is_empty() || !clean.chars().all(|c| c.is_digit(radix)) {
                return Err(self.error(start, "malformed numeric literal"));
            }
            let value = u64::from_str_radix(&clean, radix)
                .map_err(|_| self.error(start, "numeric literal out of range"))?;
            if self.peek() == Some('n') {
                return Err(self.error(self.pos, "BigInt literals are unsupported"));
            }
            return Ok(value as f64);
        }
        let mut saw_dot = false;
        let mut saw_exp = false;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.bump();
        }
        if self.peek() == Some('.') {
            saw_dot = true;
            self.bump();
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                self.bump();
            }
        }
        if self.peek().is_some_and(|c| c == 'e' || c == 'E') {
            saw_exp = true;
            self.bump();
            if self.peek().is_some_and(|c| c == '+' || c == '-') {
                self.bump();
            }
            let exp_start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                self.bump();
            }
            if exp_start == self.pos {
                return Err(self.error(start, "malformed exponent"));
            }
        }
        let raw: String = self.source[start..self.pos]
            .chars()
            .filter(|c| *c != '_')
            .collect();
        if raw.ends_with('_') || raw.contains("__") || raw.starts_with('_') {
            return Err(self.error(start, "malformed numeric separator"));
        }
        let value = raw
            .parse::<f64>()
            .map_err(|_| self.error(start, "malformed numeric literal"))?;
        if self.peek() == Some('n') {
            return Err(self.error(self.pos, "BigInt literals are unsupported"));
        }
        let _ = (saw_dot, saw_exp);
        Ok(value)
    }

    fn read_string(&mut self, quote: char) -> Result<Vec<u16>, String> {
        let start = self.pos;
        self.bump();
        let mut out = Vec::new();
        loop {
            let Some(ch) = self.bump() else {
                return Err(self.error(start, "unterminated string literal"));
            };
            if ch == quote {
                break;
            }
            if ch == '\n' || ch == '\r' || ch == '\u{2028}' || ch == '\u{2029}' {
                return Err(self.error(self.pos, "line terminator in string literal"));
            }
            if ch != '\\' {
                out.extend(ch.encode_utf16(&mut [0; 2]).iter().copied());
                continue;
            }
            let esc_at = self.pos - 1;
            let Some(esc) = self.bump() else {
                return Err(self.error(esc_at, "unterminated escape"));
            };
            match esc {
                'n' => out.push(10),
                'r' => out.push(13),
                't' => out.push(9),
                'b' => out.push(8),
                'f' => out.push(12),
                'v' => out.push(11),
                '0' => out.push(0),
                '\'' => out.push(39),
                '"' => out.push(34),
                '\\' => out.push(92),
                '\n' => {}
                '\r' => {
                    if self.peek() == Some('\n') {
                        self.bump();
                    }
                }
                'x' => out.push(self.read_hex(2, esc_at)? as u16),
                'u' => {
                    if self.peek() == Some('{') {
                        self.bump();
                        let cp_start = self.pos;
                        while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                            self.bump();
                        }
                        if cp_start == self.pos || self.bump() != Some('}') {
                            return Err(self.error(esc_at, "malformed Unicode escape"));
                        }
                        let cp = u32::from_str_radix(&self.source[cp_start..self.pos - 1], 16)
                            .map_err(|_| self.error(esc_at, "malformed Unicode escape"))?;
                        if cp > 0x10ffff {
                            return Err(self.error(esc_at, "Unicode escape out of range"));
                        }
                        if let Some(scalar) = char::from_u32(cp) {
                            let mut buf = [0; 2];
                            out.extend_from_slice(scalar.encode_utf16(&mut buf));
                        }
                    } else {
                        out.push(self.read_hex(4, esc_at)? as u16);
                    }
                }
                _ => out.extend(esc.encode_utf16(&mut [0; 2]).iter().copied()),
            }
        }
        Ok(out)
    }

    fn read_hex(&mut self, count: usize, at: usize) -> Result<u32, String> {
        let start = self.pos;
        for _ in 0..count {
            if !self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                return Err(self.error(at, "malformed hexadecimal escape"));
            }
            self.bump();
        }
        u32::from_str_radix(&self.source[start..self.pos], 16)
            .map_err(|_| self.error(at, "malformed hexadecimal escape"))
    }

    fn read_template(&mut self) -> Result<Vec<TemplateLex>, String> {
        let start = self.pos;
        self.bump();
        let mut parts = Vec::new();
        let mut text = Vec::new();
        loop {
            let Some(ch) = self.bump() else {
                return Err(self.error(start, "unterminated template literal"));
            };
            match ch {
                '`' => {
                    if !text.is_empty() || parts.is_empty() {
                        parts.push(TemplateLex::Text(text));
                    }
                    return Ok(parts);
                }
                '\\' => {
                    let Some(escaped) = self.bump() else {
                        return Err(self.error(start, "unterminated template escape"));
                    };
                    match escaped {
                        'n' => text.push(10),
                        'r' => text.push(13),
                        't' => text.push(9),
                        '`' => text.push(96),
                        '$' => text.push(36),
                        '\\' => text.push(92),
                        '\n' => {}
                        '\r' => {
                            if self.peek() == Some('\n') {
                                self.bump();
                            }
                        }
                        'u' => {
                            if self.peek() == Some('{') {
                                self.bump();
                                let cp_start = self.pos;
                                while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                                    self.bump();
                                }
                                if cp_start == self.pos || self.bump() != Some('}') {
                                    return Err(
                                        self.error(self.pos, "malformed template Unicode escape")
                                    );
                                }
                                let cp =
                                    u32::from_str_radix(&self.source[cp_start..self.pos - 1], 16)
                                        .map_err(|_| {
                                        self.error(cp_start, "malformed template Unicode escape")
                                    })?;
                                let scalar = char::from_u32(cp).ok_or_else(|| {
                                    self.error(cp_start, "template Unicode escape out of range")
                                })?;
                                let mut buf = [0; 2];
                                text.extend_from_slice(scalar.encode_utf16(&mut buf));
                            } else {
                                let unit = self.read_hex(4, self.pos - 2)? as u16;
                                text.push(unit);
                            }
                        }
                        other => text.extend(other.encode_utf16(&mut [0; 2]).iter().copied()),
                    }
                }
                '$' if self.peek() == Some('{') => {
                    self.bump();
                    if !text.is_empty() || parts.is_empty() {
                        parts.push(TemplateLex::Text(std::mem::take(&mut text)));
                    }
                    let expr_start = self.pos;
                    let (expr_end, after) = find_template_expr(self.source, expr_start)
                        .ok_or_else(|| {
                            self.error(expr_start, "unterminated template interpolation")
                        })?;
                    let expr = self.source[expr_start..expr_end].to_owned();
                    parts.push(TemplateLex::Expr(expr, expr_start));
                    self.pos = after;
                }
                other => text.extend(other.encode_utf16(&mut [0; 2]).iter().copied()),
            }
        }
    }

    fn read_punct(&mut self) -> Result<String, String> {
        const PUNCT: [&str; 33] = [
            ">>>=", "**=", "&&=", "||=", "??=", "<<=", ">>=", "===", "!==", ">>>", "...", "=>",
            "==", "!=", "<=", ">=", "++", "--", "&&", "||", "??", "**", "+=", "-=", "*=", "/=",
            "%=", "&=", "|=", "^=", "<<", ">>", "?.",
        ];
        for punct in PUNCT {
            if self.starts(punct) {
                self.pos += punct.len();
                return Ok(punct.to_owned());
            }
        }
        let ch = self
            .bump()
            .ok_or_else(|| self.error(self.pos, "unexpected end of source"))?;
        if "{}[]();,:.?~!+-*/%<>=&|^".contains(ch) {
            Ok(ch.to_string())
        } else {
            Err(self.error(self.pos - ch.len_utf8(), "unsupported character"))
        }
    }

    fn error(&self, at: usize, message: &str) -> String {
        located_error(self.source, at, message)
    }
}

fn find_template_expr(source: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut i = start;
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = i.saturating_add(2);
                    } else if bytes[i] == quote {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            b'`' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = i.saturating_add(2);
                    } else if bytes[i] == b'`' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = i.saturating_add(2);
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((i, i + 1));
                }
                i += 1;
            }
            _ => {
                let ch = source.get(i..)?.chars().next()?;
                i += ch.len_utf8();
            }
        }
    }
    None
}

fn is_ident_start(ch: char) -> bool {
    ch == '$' || ch == '_' || ch.is_alphabetic() || (ch as u32) >= 0x80
}
fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit() || ch == '\u{200c}' || ch == '\u{200d}'
}

fn located_error(source: &str, at: usize, message: &str) -> String {
    let safe = at.min(source.len());
    let prefix = &source[..safe];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("SyntaxError at {line}:{column}: {message}")
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    nesting: usize,
    function_depth: usize,
    allow_in: bool,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Result<Self, String> {
        Ok(Self {
            source,
            tokens: Lexer::new(source).tokenize()?,
            pos: 0,
            nesting: 0,
            function_depth: 0,
            allow_in: true,
        })
    }

    fn parse_program(mut self) -> Result<Vec<Stmt>, String> {
        let mut body = Vec::new();
        while !self.eof() {
            let before = self.pos;
            body.push(self.statement()?);
            if self.pos == before {
                return Err(self.error_here("parser made no progress"));
            }
        }
        Ok(body)
    }

    fn statement(&mut self) -> Result<Stmt, String> {
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        if self.is_punct("{") {
            return self.block();
        }
        if let Some(kind) = self.eat_var_kind() {
            return self.variable_statement(kind, true);
        }
        if self.eat_ident("async") {
            if self.eat_ident("function") {
                return self.function_declaration(true);
            }
            if self.is_ident("function") {
                self.bump();
                return self.function_declaration(true);
            }
            self.pos -= 1;
        }
        if self.eat_ident("function") {
            return self.function_declaration(false);
        }
        if self.eat_ident("class") {
            return self.class_declaration();
        }
        if self.eat_ident("return") {
            if self.function_depth == 0 {
                return Err(self.error_previous("return outside function"));
            }
            let value = if self.line_break_before()
                || self.is_punct(";")
                || self.is_punct("}")
                || self.eof()
            {
                None
            } else {
                Some(self.expression()?)
            };
            self.semicolon()?;
            return Ok(Stmt::Return(value));
        }
        if self.eat_ident("throw") {
            if self.line_break_before() {
                return Err(self.error_previous("line terminator after throw"));
            }
            let value = self.expression()?;
            self.semicolon()?;
            return Ok(Stmt::Throw(value));
        }
        if self.eat_ident("if") {
            self.expect_punct("(")?;
            let test = self.expression()?;
            self.expect_punct(")")?;
            let then = Box::new(self.statement()?);
            let otherwise = if self.eat_ident("else") {
                Some(Box::new(self.statement()?))
            } else {
                None
            };
            return Ok(Stmt::If(test, then, otherwise));
        }
        if self.eat_ident("while") {
            self.expect_punct("(")?;
            let test = self.expression()?;
            self.expect_punct(")")?;
            return Ok(Stmt::While(test, Box::new(self.statement()?)));
        }
        if self.eat_ident("do") {
            let body = Box::new(self.statement()?);
            self.expect_ident("while")?;
            self.expect_punct("(")?;
            let test = self.expression()?;
            self.expect_punct(")")?;
            self.semicolon()?;
            return Ok(Stmt::DoWhile(body, test));
        }
        if self.eat_ident("for") {
            return self.for_statement();
        }
        if self.eat_ident("try") {
            let body = Box::new(self.statement()?);
            let catch = if self.eat_ident("catch") {
                let binding = if self.eat_punct("(") {
                    let pat = if self.is_punct(")") {
                        None
                    } else {
                        Some(self.pattern()?)
                    };
                    self.expect_punct(")")?;
                    pat
                } else {
                    None
                };
                Some((binding, Box::new(self.statement()?)))
            } else {
                None
            };
            let finally = if self.eat_ident("finally") {
                Some(Box::new(self.statement()?))
            } else {
                None
            };
            if catch.is_none() && finally.is_none() {
                return Err(self.error_previous("try requires catch or finally"));
            }
            return Ok(Stmt::Try {
                body,
                catch,
                finally,
            });
        }
        if self.eat_ident("switch") {
            return self.switch_statement();
        }
        if self.eat_ident("break") {
            self.semicolon()?;
            return Ok(Stmt::Break);
        }
        if self.eat_ident("continue") {
            self.semicolon()?;
            return Ok(Stmt::Continue);
        }
        if self.eat_ident("debugger") {
            self.semicolon()?;
            return Ok(Stmt::Empty);
        }
        if self.eat_ident("import") {
            return Err(self.error_previous("import declarations are unsupported"));
        }
        if self.eat_ident("export") {
            return self.export_statement();
        }
        let expr = self.expression()?;
        self.semicolon()?;
        Ok(Stmt::Expr(expr))
    }

    fn block(&mut self) -> Result<Stmt, String> {
        self.enter()?;
        self.expect_punct("{")?;
        let mut statements = Vec::new();
        while !self.eof() && !self.is_punct("}") {
            statements.push(self.statement()?);
        }
        self.expect_punct("}")?;
        self.leave();
        Ok(Stmt::Block(statements))
    }

    fn eat_var_kind(&mut self) -> Option<VarKind> {
        if self.eat_ident("var") {
            Some(VarKind::Var)
        } else if self.eat_ident("let") {
            Some(VarKind::Let)
        } else if self.eat_ident("const") {
            Some(VarKind::Const)
        } else {
            None
        }
    }

    fn variable_statement(&mut self, kind: VarKind, semi: bool) -> Result<Stmt, String> {
        let mut declarations = Vec::new();
        loop {
            let pattern = self.pattern()?;
            let (pattern, init) = if let Pattern::Default(pattern, initial) = pattern {
                (*pattern, Some(*initial))
            } else {
                let initial = if self.eat_punct("=") {
                    Some(self.assignment_expression()?)
                } else {
                    None
                };
                (pattern, initial)
            };
            if semi && init.is_none() && declarations.is_empty() && self.last_binding_is_const() {
                return Err(self.error_previous("const declaration requires an initializer"));
            }
            declarations.push((pattern, init));
            if !self.eat_punct(",") {
                break;
            }
        }
        if semi {
            self.semicolon()?;
        }
        Ok(Stmt::Var(kind, declarations))
    }

    fn last_binding_is_const(&self) -> bool {
        self.tokens[..self.pos]
            .iter()
            .rev()
            .find_map(|t| match &t.kind {
                Kind::Ident(s) if ["var", "let", "const"].contains(&s.as_str()) => {
                    Some(s == "const")
                }
                _ => None,
            })
            .unwrap_or(false)
    }

    fn function_declaration(&mut self, asynchronous: bool) -> Result<Stmt, String> {
        let name = self.expect_identifier_name()?;
        let mut function = self.function_tail(Some(name.clone()), false)?;
        function.asynchronous = asynchronous;
        Ok(Stmt::FunctionDecl(name, function))
    }

    fn class_declaration(&mut self) -> Result<Stmt, String> {
        let name = self.expect_identifier_name()?;
        let class = self.class_tail(Some(name.clone()))?;
        Ok(Stmt::ClassDecl(name, class))
    }

    fn class_tail(&mut self, name: Option<String>) -> Result<ClassExpr, String> {
        self.enter()?;
        let extends = if self.eat_ident("extends") {
            Some(Box::new(self.left_hand_side()?))
        } else {
            None
        };
        self.expect_punct("{")?;
        let mut methods = Vec::new();
        let mut fields = Vec::new();
        while !self.eof() && !self.is_punct("}") {
            if self.eat_punct(";") {
                continue;
            }
            let is_static = if self.is_ident("static") && !self.peek_n_punct(1, "(") {
                self.bump();
                true
            } else {
                false
            };
            let asynchronous = self.is_ident("async")
                && !self.peek_n_linebreak(1)
                && !self.peek_n_punct(1, "(")
                && !self.peek_n_punct(1, "=")
                && !self.peek_n_punct(1, ";")
                && !self.peek_n_punct(1, "}");
            if asynchronous {
                self.bump();
            }
            let mut getter = false;
            let mut setter = false;
            if (self.is_ident("get") || self.is_ident("set")) && !self.peek_n_punct(1, "(") {
                getter = self.is_ident("get");
                setter = !getter;
                self.bump();
            }
            let method_name = self.property_name()?;
            if !self.is_punct("(") && !getter && !setter {
                let initial = if self.eat_punct("=") {
                    Some(self.assignment_expression()?)
                } else {
                    None
                };
                self.semicolon()?;
                fields.push((method_name, initial, is_static));
                continue;
            }
            if asynchronous && (getter || setter || method_name == "constructor") {
                return Err(self.error_previous("this class method cannot be async"));
            }
            let mut function = self.method_function(Some(method_name.clone()))?;
            function.asynchronous = asynchronous;
            methods.push(ClassMethod {
                name: method_name,
                function,
                getter,
                setter,
                is_static,
            });
        }
        self.expect_punct("}")?;
        self.leave();
        Ok(ClassExpr {
            name,
            extends,
            methods,
            fields,
        })
    }

    fn function_tail(&mut self, name: Option<String>, arrow: bool) -> Result<FunctionExpr, String> {
        let source_offset = Some(self.current().at);
        let params = self.parameters()?;
        self.function_depth += 1;
        let body = match self.block()? {
            Stmt::Block(stmts) => stmts,
            _ => Vec::new(),
        };
        self.function_depth -= 1;
        Ok(FunctionExpr {
            params,
            body,
            arrow,
            asynchronous: false,
            name,
            source_offset,
        })
    }

    fn method_function(&mut self, name: Option<String>) -> Result<FunctionExpr, String> {
        self.function_tail(name, false)
    }

    fn parameters(&mut self) -> Result<Vec<Pattern>, String> {
        self.enter()?;
        self.expect_punct("(")?;
        let mut params = Vec::new();
        if !self.is_punct(")") {
            loop {
                let pattern = if self.eat_punct("...") {
                    Pattern::Rest(Box::new(self.pattern()?))
                } else {
                    self.pattern()?
                };
                params.push(pattern);
                if !self.eat_punct(",") {
                    break;
                }
                if self.is_punct(")") {
                    break;
                }
            }
        }
        self.expect_punct(")")?;
        self.leave();
        Ok(params)
    }

    fn for_statement(&mut self) -> Result<Stmt, String> {
        self.expect_punct("(")?;
        let init = if self.eat_punct(";") {
            None
        } else if let Some(kind) = self.eat_var_kind() {
            Some(Box::new(self.variable_statement(kind, false)?))
        } else {
            let allow_in = self.allow_in;
            self.allow_in = false;
            let parsed = self.expression();
            self.allow_in = allow_in;
            Some(Box::new(Stmt::Expr(parsed?)))
        };
        if self.eat_ident("in") || self.eat_ident("of") {
            let keys = self.previous_is_ident("in");
            let (pattern, kind) = match init {
                Some(boxed) => match *boxed {
                    Stmt::Expr(e) => (expr_to_pattern(e)?, None),
                    Stmt::Var(kind, mut v) if v.len() == 1 => (v.remove(0).0, Some(kind)),
                    _ => return Err(self.error_previous("invalid for-each binding")),
                },
                None => return Err(self.error_previous("missing for-each binding")),
            };
            let iterable = self.expression()?;
            self.expect_punct(")")?;
            let body = Box::new(self.statement()?);
            return Ok(Stmt::ForEach {
                pattern,
                kind,
                iterable,
                body,
                keys,
            });
        }
        if init.is_some() {
            self.expect_punct(";")?;
        }
        let test = if self.is_punct(";") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect_punct(";")?;
        let update = if self.is_punct(")") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect_punct(")")?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::For {
            init,
            test,
            update,
            body,
        })
    }

    fn switch_statement(&mut self) -> Result<Stmt, String> {
        self.expect_punct("(")?;
        let value = self.expression()?;
        self.expect_punct(")")?;
        self.expect_punct("{")?;
        let mut cases = Vec::new();
        while !self.eof() && !self.is_punct("}") {
            let test = if self.eat_ident("case") {
                Some(self.expression_no_comma()?)
            } else if self.eat_ident("default") {
                None
            } else {
                return Err(self.error_here("expected case or default"));
            };
            self.expect_punct(":")?;
            let mut body = Vec::new();
            while !self.eof()
                && !self.is_punct("}")
                && !self.is_ident("case")
                && !self.is_ident("default")
            {
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        self.expect_punct("}")?;
        Ok(Stmt::Switch(value, cases))
    }

    fn export_statement(&mut self) -> Result<Stmt, String> {
        if self.eat_punct("*") {
            return Err(self.error_previous("export star declarations are unsupported"));
        }
        if self.eat_ident("default") {
            if self.eat_ident("function") {
                return self.function_declaration(false);
            }
            if self.eat_ident("class") {
                return self.class_declaration();
            }
            let expr = self.assignment_expression()?;
            self.semicolon()?;
            return Ok(Stmt::Expr(expr));
        }
        if let Some(kind) = self.eat_var_kind() {
            return self.variable_statement(kind, true);
        }
        if self.eat_ident("function") {
            return self.function_declaration(false);
        }
        if self.eat_ident("class") {
            return self.class_declaration();
        }
        Err(self.error_previous("unsupported export form"))
    }

    fn expression(&mut self) -> Result<Expr, String> {
        let mut expr = self.assignment_expression()?;
        if self.eat_punct(",") {
            let mut values = vec![expr];
            loop {
                values.push(self.assignment_expression()?);
                if !self.eat_punct(",") {
                    break;
                }
            }
            expr = Expr::Sequence(values);
        }
        Ok(expr)
    }

    fn expression_no_comma(&mut self) -> Result<Expr, String> {
        self.assignment_expression()
    }

    fn assignment_expression(&mut self) -> Result<Expr, String> {
        if let Some((params, is_async)) = self.try_arrow_params()? {
            return self.arrow_tail(params, is_async);
        }
        let left = self.conditional_expression()?;
        if let Some(op) = self.assignment_operator() {
            self.bump();
            if !is_assignable(&left) {
                return Err(self.error_previous("invalid assignment target"));
            }
            let right = self.assignment_expression()?;
            Ok(Expr::Assign(op, Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn try_arrow_params(&mut self) -> Result<Option<(Vec<Pattern>, bool)>, String> {
        let save = self.pos;
        let saved_nesting = self.nesting;
        let is_async = if self.is_ident("async") && !self.peek_n_linebreak(1) {
            self.bump();
            true
        } else {
            false
        };
        if let Kind::Ident(name) = self.current().kind.clone() {
            self.bump();
            if self.eat_punct("=>") {
                return Ok(Some((vec![Pattern::Ident(name)], is_async)));
            }
        } else if self.is_punct("(") {
            if let Ok(params) = self.parameters() {
                if self.eat_punct("=>") {
                    return Ok(Some((params, is_async)));
                }
            }
        }
        self.pos = save;
        self.nesting = saved_nesting;
        Ok(None)
    }

    fn arrow_tail(&mut self, params: Vec<Pattern>, asynchronous: bool) -> Result<Expr, String> {
        let source_offset = Some(self.current().at);
        let body = if self.eat_punct("{") {
            self.pos -= 1;
            self.function_depth += 1;
            let parsed = self.block()?;
            self.function_depth -= 1;
            match parsed {
                Stmt::Block(body) => body,
                _ => Vec::new(),
            }
        } else {
            vec![Stmt::Return(Some(self.assignment_expression()?))]
        };
        Ok(Expr::Function(FunctionExpr {
            params,
            body,
            arrow: true,
            asynchronous,
            name: None,
            source_offset,
        }))
    }

    fn conditional_expression(&mut self) -> Result<Expr, String> {
        let test = self.binary_expression(1)?;
        if self.eat_punct("?") {
            let yes = self.assignment_expression()?;
            self.expect_punct(":")?;
            let no = self.assignment_expression()?;
            Ok(Expr::Conditional(
                Box::new(test),
                Box::new(yes),
                Box::new(no),
            ))
        } else {
            Ok(test)
        }
    }

    fn binary_expression(&mut self, min_prec: u8) -> Result<Expr, String> {
        let mut left = self.unary_expression()?;
        while let Some((op, prec, right_assoc)) = self.binary_op() {
            if prec < min_prec {
                break;
            }
            self.bump();
            let right = self.binary_expression(if right_assoc { prec } else { prec + 1 })?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary_expression(&mut self) -> Result<Expr, String> {
        if self.eat_ident("await") {
            return Ok(Expr::Await(Box::new(self.unary_expression()?)));
        }
        if self.eat_ident("yield") {
            return Err(self.error_previous("yield expressions are unsupported"));
        }
        if self.eat_ident("typeof")
            || self.eat_ident("void")
            || self.eat_ident("delete")
            || self.eat_punct("!")
            || self.eat_punct("~")
            || self.eat_punct("+")
            || self.eat_punct("-")
        {
            let op = self.previous_text();
            return Ok(Expr::Unary(op, Box::new(self.unary_expression()?)));
        }
        if self.eat_punct("++") || self.eat_punct("--") {
            let op = self.previous_text();
            let target = self.unary_expression()?;
            if !is_assignable(&target) {
                return Err(self.error_previous("invalid update target"));
            }
            return Ok(Expr::Update(op, Box::new(target), true));
        }
        let mut expr = self.left_hand_side()?;
        if (self.eat_punct("++") || self.eat_punct("--")) && !self.line_break_before() {
            let op = self.previous_text();
            if !is_assignable(&expr) {
                return Err(self.error_previous("invalid update target"));
            }
            expr = Expr::Update(op, Box::new(expr), false);
        }
        Ok(expr)
    }

    fn left_hand_side(&mut self) -> Result<Expr, String> {
        if self.eat_ident("new") {
            let mut callee = self.primary()?;
            loop {
                if self.eat_punct(".") {
                    let key = self.property_name()?;
                    callee = Expr::Member(
                        Box::new(callee),
                        Box::new(Expr::Literal(Literal::String(key.encode_utf16().collect()))),
                    );
                } else if self.eat_punct("[") {
                    let key = self.expression()?;
                    self.expect_punct("]")?;
                    callee = Expr::Member(Box::new(callee), Box::new(key));
                } else {
                    break;
                }
            }
            let args = if self.is_punct("(") {
                self.arguments()?
            } else {
                Vec::new()
            };
            return self.postfix(Expr::New(Box::new(callee), args));
        }
        self.member_only()
    }

    fn member_only(&mut self) -> Result<Expr, String> {
        let expr = self.primary()?;
        self.postfix(expr)
    }

    fn postfix(&mut self, mut expr: Expr) -> Result<Expr, String> {
        let mut parts = Vec::new();
        let mut optional = false;
        loop {
            if self.eat_punct(".") {
                let key = self.property_name()?;
                parts.push(ChainPart::Member(
                    Expr::Literal(Literal::String(key.encode_utf16().collect())),
                    false,
                ));
            } else if self.eat_punct("[") {
                let key = self.expression()?;
                self.expect_punct("]")?;
                parts.push(ChainPart::Member(key, false));
            } else if self.is_punct("(") {
                let args = self.arguments()?;
                parts.push(ChainPart::Call(args, false));
            } else if self.eat_punct("?.") {
                optional = true;
                if self.eat_punct("[") {
                    let key = self.expression()?;
                    self.expect_punct("]")?;
                    parts.push(ChainPart::Member(key, true));
                } else if self.is_punct("(") {
                    let args = self.arguments()?;
                    parts.push(ChainPart::Call(args, true));
                } else {
                    let key = self.property_name()?;
                    parts.push(ChainPart::Member(
                        Expr::Literal(Literal::String(key.encode_utf16().collect())),
                        true,
                    ));
                }
            } else {
                break;
            }
        }
        if optional {
            return Ok(Expr::Chain(Box::new(expr), parts));
        }
        for part in parts {
            expr = match part {
                ChainPart::Member(key, _) => Expr::Member(Box::new(expr), Box::new(key)),
                ChainPart::Call(args, _) => Expr::Call(Box::new(expr), args),
            };
        }
        Ok(expr)
    }

    fn arguments(&mut self) -> Result<Vec<Expr>, String> {
        self.enter()?;
        self.expect_punct("(")?;
        let mut args = Vec::new();
        if !self.is_punct(")") {
            loop {
                if self.eat_punct("...") {
                    args.push(Expr::Spread(Box::new(self.assignment_expression()?)));
                } else {
                    args.push(self.assignment_expression()?);
                }
                if !self.eat_punct(",") {
                    break;
                }
                if self.is_punct(")") {
                    break;
                }
            }
        }
        self.expect_punct(")")?;
        self.leave();
        Ok(args)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let token = self.bump().clone();
        match token.kind {
            Kind::Number(n) => Ok(Expr::Literal(Literal::Number(n))),
            Kind::String(s) => Ok(Expr::Literal(Literal::String(s))),
            Kind::Template(parts) => self.template_expr(parts, token.at),
            Kind::Regex(source, flags) => Ok(Expr::New(
                Box::new(Expr::Ident("RegExp".into())),
                vec![
                    Expr::Literal(Literal::String(source.encode_utf16().collect())),
                    Expr::Literal(Literal::String(flags.encode_utf16().collect())),
                ],
            )),
            Kind::Ident(name) => match name.as_str() {
                "true" => Ok(Expr::Literal(Literal::Bool(true))),
                "false" => Ok(Expr::Literal(Literal::Bool(false))),
                "null" => Ok(Expr::Literal(Literal::Null)),
                "undefined" => Ok(Expr::Literal(Literal::Undefined)),
                "this" => Ok(Expr::This),
                "function" => {
                    let name = if self.is_identifier_name() && !self.is_punct("(") {
                        Some(self.expect_identifier_name()?)
                    } else {
                        None
                    };
                    Ok(Expr::Function(self.function_tail(name, false)?))
                }
                "async" if self.eat_ident("function") => {
                    let name = if self.is_identifier_name() && !self.is_punct("(") {
                        Some(self.expect_identifier_name()?)
                    } else {
                        None
                    };
                    let mut function = self.function_tail(name, false)?;
                    function.asynchronous = true;
                    Ok(Expr::Function(function))
                }
                "class" => {
                    let name = if self.is_identifier_name()
                        && !self.is_punct("{")
                        && !self.is_ident("extends")
                    {
                        Some(self.expect_identifier_name()?)
                    } else {
                        None
                    };
                    Ok(Expr::Class(self.class_tail(name)?))
                }
                _ => Ok(Expr::Ident(name)),
            },
            Kind::Punct(p) if p == "(" => {
                self.enter()?;
                let expr = self.expression()?;
                self.expect_punct(")")?;
                self.leave();
                Ok(expr)
            }
            Kind::Punct(p) if p == "[" => self.array_literal(),
            Kind::Punct(p) if p == "{" => self.object_literal(),
            _ => Err(self.error_at(token.at, "expected expression")),
        }
    }

    fn template_expr(&mut self, parts: Vec<TemplateLex>, at: usize) -> Result<Expr, String> {
        let mut parsed = Vec::new();
        for part in parts {
            match part {
                TemplateLex::Text(text) => parsed.push(TemplatePart::Text(text)),
                TemplateLex::Expr(source, offset) => {
                    let mut sub = Parser::new(&source)
                        .map_err(|e| format!("{e} in template at {}", self.location(offset)))?;
                    let expr = sub
                        .expression()
                        .map_err(|e| format!("{e} in template at {}", self.location(offset)))?;
                    if !sub.eof() {
                        return Err(self.error_at(
                            offset + sub.current().at,
                            "unexpected token in template interpolation",
                        ));
                    }
                    parsed.push(TemplatePart::Expression(expr));
                }
            }
        }
        let _ = at;
        Ok(Expr::Template(parsed))
    }

    fn array_literal(&mut self) -> Result<Expr, String> {
        self.enter()?;
        let mut items = Vec::new();
        while !self.eof() && !self.is_punct("]") {
            if self.eat_punct(",") {
                items.push(None);
                continue;
            }
            if self.eat_punct("...") {
                items.push(Some(Expr::Spread(Box::new(self.assignment_expression()?))));
            } else {
                items.push(Some(self.assignment_expression()?));
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("]")?;
        self.leave();
        Ok(Expr::Array(items))
    }

    fn object_literal(&mut self) -> Result<Expr, String> {
        self.enter()?;
        let mut entries = Vec::new();
        while !self.eof() && !self.is_punct("}") {
            if self.eat_punct("...") {
                entries.push(ObjectEntry::Spread(self.assignment_expression()?));
            } else {
                let accessor = (self.is_ident("get") || self.is_ident("set"))
                    && !self.peek_n_punct(1, "(")
                    && !self.peek_n_punct(1, ":")
                    && !self.peek_n_punct(1, ",")
                    && !self.peek_n_punct(1, "}");
                let mut getter = false;
                let mut setter = false;
                if accessor {
                    getter = self.is_ident("get");
                    setter = !getter;
                    self.bump();
                }
                if self.eat_punct("[") {
                    let key = self.expression()?;
                    self.expect_punct("]")?;
                    if self.eat_punct(":") {
                        entries.push(ObjectEntry::Computed(key, self.assignment_expression()?));
                    } else if self.is_punct("(") {
                        let fun = self.method_function(None)?;
                        entries.push(ObjectEntry::Computed(key, Expr::Function(fun)));
                    } else {
                        return Err(self.error_here("invalid computed object property"));
                    }
                } else {
                    let name = self.property_name()?;
                    if self.eat_punct(":") {
                        entries.push(ObjectEntry::Property(name, self.assignment_expression()?));
                    } else if self.is_punct("(") {
                        let fun = self.method_function(Some(name.clone()))?;
                        if getter {
                            entries.push(ObjectEntry::Getter(name, fun));
                        } else if setter {
                            entries.push(ObjectEntry::Setter(name, fun));
                        } else {
                            entries.push(ObjectEntry::Method(name, fun));
                        }
                    } else {
                        if getter || setter {
                            return Err(self.error_here("accessor must have a method body"));
                        }
                        entries.push(ObjectEntry::Property(name.clone(), Expr::Ident(name)));
                    }
                }
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        self.leave();
        Ok(Expr::Object(entries))
    }

    fn pattern(&mut self) -> Result<Pattern, String> {
        self.enter()?;
        let mut pattern = if self.eat_punct("...") {
            Pattern::Rest(Box::new(self.pattern()?))
        } else if self.eat_punct("[") {
            let mut items = Vec::new();
            while !self.eof() && !self.is_punct("]") {
                if self.eat_punct(",") {
                    items.push(None);
                    continue;
                }
                items.push(Some(self.pattern()?));
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct("]")?;
            Pattern::Array(items)
        } else if self.eat_punct("{") {
            let mut fields = Vec::new();
            while !self.eof() && !self.is_punct("}") {
                if self.eat_punct("...") {
                    fields.push((
                        String::from("..."),
                        Pattern::Rest(Box::new(self.pattern()?)),
                    ));
                    if !self.eat_punct(",") {
                        break;
                    }
                    continue;
                }
                let key = self.property_name()?;
                let value = if self.eat_punct(":") {
                    self.pattern()?
                } else {
                    Pattern::Ident(key.clone())
                };
                fields.push((key, value));
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct("}")?;
            Pattern::Object(fields)
        } else {
            Pattern::Ident(self.expect_identifier_name()?)
        };
        if self.eat_punct("=") {
            pattern = Pattern::Default(Box::new(pattern), Box::new(self.assignment_expression()?));
        }
        self.leave();
        Ok(pattern)
    }

    fn property_name(&mut self) -> Result<String, String> {
        match self.bump().kind.clone() {
            Kind::Ident(name) => Ok(name),
            Kind::String(units) => Ok(String::from_utf16_lossy(&units)),
            Kind::Number(number) => Ok(number.to_string()),
            Kind::Punct(p) if p == "[" => {
                Err(self.error_previous("computed name requires expression context"))
            }
            _ => Err(self.error_previous("expected property name")),
        }
    }

    fn binary_op(&self) -> Option<(String, u8, bool)> {
        let op = self.punct_text()?;
        let item = match op {
            "||" => (1, false),
            "??" => (1, false),
            "&&" => (2, false),
            "|" => (3, false),
            "^" => (4, false),
            "&" => (5, false),
            "==" | "!=" | "===" | "!==" => (6, false),
            "<" | ">" | "<=" | ">=" => (7, false),
            "in" if self.allow_in => (7, false),
            "instanceof" => (7, false),
            "<<" | ">>" | ">>>" => (8, false),
            "+" | "-" => (9, false),
            "*" | "/" | "%" => (10, false),
            "**" => (11, true),
            _ => return None,
        };
        Some((op.to_owned(), item.0, item.1))
    }

    fn assignment_operator(&self) -> Option<String> {
        let op = self.punct_text()?;
        if [
            "=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", ">>>=", "**=",
            "&&=", "||=", "??=",
        ]
        .contains(&op)
        {
            Some(op.to_owned())
        } else {
            None
        }
    }

    fn semicolon(&mut self) -> Result<(), String> {
        if self.eat_punct(";") || self.eof() || self.is_punct("}") || self.line_break_before() {
            Ok(())
        } else {
            Err(self.error_here("expected semicolon"))
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.nesting += 1;
        if self.nesting > MAX_NESTING {
            return Err(self.error_here("nesting limit exceeded"));
        }
        Ok(())
    }
    fn leave(&mut self) {
        self.nesting = self.nesting.saturating_sub(1);
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }
    fn bump(&mut self) -> &Token {
        let p = self.pos;
        if !self.eof() {
            self.pos += 1;
        }
        &self.tokens[p]
    }
    fn eof(&self) -> bool {
        matches!(self.current().kind, Kind::Eof)
    }
    fn is_punct(&self, p: &str) -> bool {
        matches!(&self.current().kind, Kind::Punct(s) if s == p)
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect_punct(&mut self, p: &str) -> Result<(), String> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err(self.error_here(&format!("expected '{p}'")))
        }
    }
    fn is_ident(&self, name: &str) -> bool {
        matches!(&self.current().kind, Kind::Ident(s) if s == name)
    }
    fn eat_ident(&mut self, name: &str) -> bool {
        if self.is_ident(name) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect_ident(&mut self, name: &str) -> Result<(), String> {
        if self.eat_ident(name) {
            Ok(())
        } else {
            Err(self.error_here(&format!("expected {name}")))
        }
    }
    fn is_identifier_name(&self) -> bool {
        matches!(self.current().kind, Kind::Ident(_))
            || matches!(&self.current().kind, Kind::Punct(p) if p == "default")
    }
    fn expect_identifier_name(&mut self) -> Result<String, String> {
        match self.bump().kind.clone() {
            Kind::Ident(name) => Ok(name),
            _ => Err(self.error_previous("expected identifier")),
        }
    }
    fn line_break_before(&self) -> bool {
        self.current().line_break_before
    }
    fn peek_n_linebreak(&self, n: usize) -> bool {
        self.tokens
            .get(self.pos + n)
            .is_some_and(|t| t.line_break_before)
    }
    fn peek_n_punct(&self, n: usize, p: &str) -> bool {
        matches!(self.tokens.get(self.pos + n).map(|t| &t.kind), Some(Kind::Punct(s)) if s == p)
    }
    fn punct_text(&self) -> Option<&str> {
        match &self.current().kind {
            Kind::Punct(s) => Some(s),
            Kind::Ident(s) if s == "in" || s == "instanceof" => Some(s),
            _ => None,
        }
    }
    fn previous_text(&self) -> String {
        match &self.tokens[self.pos.saturating_sub(1)].kind {
            Kind::Punct(s) | Kind::Ident(s) => s.clone(),
            _ => String::new(),
        }
    }
    fn previous_is_ident(&self, name: &str) -> bool {
        matches!(&self.tokens[self.pos.saturating_sub(1)].kind, Kind::Ident(s) if s == name)
    }
    fn error_here(&self, message: &str) -> String {
        self.error_at(self.current().at, message)
    }
    fn error_previous(&self, message: &str) -> String {
        self.error_at(self.tokens[self.pos.saturating_sub(1)].at, message)
    }
    fn error_at(&self, at: usize, message: &str) -> String {
        located_error(self.source, at, message)
    }
    fn location(&self, at: usize) -> String {
        let e = located_error(self.source, at, "");
        e.trim_end_matches(": ").to_owned()
    }
}

fn is_assignable(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(_) | Expr::Member(_, _) => true,
        Expr::Array(items) => items.iter().flatten().all(is_assignable),
        Expr::Object(entries) => entries.iter().all(|entry| match entry {
            ObjectEntry::Property(_, value) => is_assignable(value),
            ObjectEntry::Computed(_, value) => is_assignable(value),
            ObjectEntry::Spread(value) => is_assignable(value),
            _ => false,
        }),
        _ => false,
    }
}

fn expr_to_pattern(expr: Expr) -> Result<Pattern, String> {
    match expr {
        Expr::Ident(name) => Ok(Pattern::Ident(name)),
        Expr::Array(items) => Ok(Pattern::Array(
            items
                .into_iter()
                .map(|item| item.map(expr_to_pattern).transpose())
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Expr::Object(entries) => {
            let mut fields = Vec::new();
            for entry in entries {
                match entry {
                    ObjectEntry::Property(name, value) => {
                        fields.push((name, expr_to_pattern(value)?))
                    }
                    _ => {
                        return Err(String::from(
                            "SyntaxError at 1:1: unsupported object assignment pattern",
                        ));
                    }
                }
            }
            Ok(Pattern::Object(fields))
        }
        Expr::Assign(_, left, right) => {
            Ok(Pattern::Default(Box::new(expr_to_pattern(*left)?), right))
        }
        Expr::Spread(value) => Ok(Pattern::Rest(Box::new(expr_to_pattern(*value)?))),
        _ => Err(String::from("SyntaxError at 1:1: invalid binding pattern")),
    }
}

pub fn parse(source: &str) -> Result<Vec<Stmt>, String> {
    Parser::new(source)?.parse_program()
}
