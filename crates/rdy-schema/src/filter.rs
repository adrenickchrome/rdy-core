//! The filter grammar's parser: one grammar filters blocks for `rdy_query`, `rdy q` and pack
//! rules. It reads text against one pack and has no other input, so a pack can check its rules
//! when it loads. Evaluating a filter belongs to the engine.
//!
//! Every input is bounded before it can cost anything: the length first, then the depth and the
//! link nesting while the parser reads, before it recurses.

use crate::pack::Pack;
use rdy_model::did_you_mean::nearest;
use std::fmt;

pub const MAX_FILTER_CHARS: usize = 2_000;
pub const MAX_FILTER_DEPTH: usize = 16;
pub const MAX_LINK_NESTING: usize = 3;
pub const MAX_FIELD_NAME: usize = 64;

/// The most characters of the input that a refusal repeats.
const MAX_ERROR_TOKEN: usize = 40;

pub const FILTER_GUIDE: &str = "A filter selects blocks. A test is <field> <op> <value>, has <field>, or a link test. Join tests with and, or, not and parentheses; not binds first, then and, then or. Fields: id, key, type, trait, path, doc, area, lifecycle, status, suspect, tension, coverage and fields.<name>. Ops: = != < <= > >= ^=, where ^= means starts with. A value is a word, a number, true, false, a block id or a quoted string. In a quoted string, backslash-quote is a quote and two backslashes are one backslash. A number is a whole number in the signed 64-bit range; quote a longer run of digits to use it as text. Quote a value that is and, or, not, has, no, in or out. A link test is out <link> or in <link>, with an optional (filter) that a linked block must match. no out <link> and no in <link> hold when no linked block matches. A link is a link kind of the pack, contains or references. Keywords, field names and link kinds are case-insensitive; values keep their case. status is the derived status. coverage is a score from 0 to 100; suspect and tension take true or false. A test of a field that the block lacks is false, for != too. Limits: 2000 characters, nesting depth 16, 3 nested link tests. Scope is a parameter, never part of the filter. Example: type = decision and status = live and no in implements.";

/// The words a bare value cannot be.
const RESERVED: [&str; 7] = ["and", "or", "not", "has", "no", "in", "out"];
const FIELD_NAMES: [&str; 12] = [
    "id",
    "key",
    "type",
    "trait",
    "path",
    "doc",
    "area",
    "lifecycle",
    "status",
    "suspect",
    "tension",
    "coverage",
];
const TRAITS: [&str; 7] = [
    "normative",
    "informative",
    "defines",
    "describes",
    "asset",
    "structural",
    "presentational",
];
const LIFECYCLES: [&str; 3] = ["draft", "stable", "deprecated"];
const STATUSES: [&str; 6] = [
    "proposed",
    "rejected",
    "live",
    "partially_superseded",
    "superseded",
    "revoked",
];
const CUSTOM_PREFIX: &str = "fields.";

/// A refusal: `what` went wrong, and at which character of the filter, counted from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterError {
    pub column: usize,
    pub what: String,
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "filter: {} at column {}", self.what, self.column)
    }
}

impl std::error::Error for FilterError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Id,
    Key,
    Type,
    Trait,
    Path,
    Doc,
    Area,
    Lifecycle,
    Status,
    Suspect,
    Tension,
    Coverage,
    /// `fields.<name>`, holding the name as written.
    Custom(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    StartsWith,
}

/// `Int` and `Bool` keep the text as written, for the text comparison of id, key, path and doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Int { value: i64, text: String },
    Bool { value: bool, text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Out,
    In,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Contains,
    References,
    /// A link kind of the pack, in the pack's spelling.
    Link(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Or(Vec<Expr>),
    And(Vec<Expr>),
    Not(Box<Expr>),
    Compare {
        field: Field,
        op: Op,
        value: Value,
    },
    Has(Field),
    Linked {
        dir: Dir,
        kind: LinkKind,
        inner: Option<Box<Expr>>,
        none: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub text: String,
    pub expr: Expr,
}

/// Which supplied sets an evaluation reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    pub suspect: bool,
    pub tension: bool,
    pub coverage: bool,
}

impl Filter {
    /// True for each of `suspect`, `tension` and `coverage` that a test names, inner link
    /// expressions included (`has coverage` counts).
    pub fn needs(&self) -> Needs {
        let mut needs = Needs::default();
        add_needs(&self.expr, &mut needs);
        needs
    }
}

fn add_needs(expr: &Expr, needs: &mut Needs) {
    let mut add_field = |field: &Field| match field {
        Field::Suspect => needs.suspect = true,
        Field::Tension => needs.tension = true,
        Field::Coverage => needs.coverage = true,
        _ => {}
    };
    match expr {
        Expr::Compare { field, .. } | Expr::Has(field) => add_field(field),
        Expr::Or(all) | Expr::And(all) => all.iter().for_each(|e| add_needs(e, needs)),
        Expr::Not(inner) => add_needs(inner, needs),
        Expr::Linked { inner, .. } => {
            if let Some(inner) = inner {
                add_needs(inner, needs);
            }
        }
    }
}

/// Parses `text` against `pack` (its block types, areas and link kinds). Checks the length
/// first, and the depth and link nesting while it reads, before it recurses.
pub fn parse(text: &str, pack: &Pack) -> Result<Filter, FilterError> {
    if text.chars().nth(MAX_FILTER_CHARS).is_some() {
        return Err(refuse(
            MAX_FILTER_CHARS + 1,
            format!("the filter is longer than {MAX_FILTER_CHARS} characters"),
        ));
    }
    let mut parser = Parser {
        tokens: tokenize(text),
        at: 0,
        pack,
    };
    let expr = parser.expr(0, 0)?;
    let end = parser.peek()?;
    if end.kind != Kind::End {
        return Err(unexpected(&end));
    }
    Ok(Filter {
        text: text.to_string(),
        expr,
    })
}

fn refuse(column: usize, what: String) -> FilterError {
    FilterError { column, what }
}

/// `text` as a refusal repeats it, cut to 40 characters.
fn cut(text: &str) -> String {
    text.chars().take(MAX_ERROR_TOKEN).collect()
}

fn unexpected(token: &Token<'_>) -> FilterError {
    refuse(token.column, format!("unexpected {}", cut(token.text)))
}

fn did_you_mean(what: String, word: &str, known: &[&str]) -> String {
    match nearest(word, known) {
        Some(near) => format!("{what}; did you mean {near}?"),
        None => what,
    }
}

fn is_word(word: &str, known: &str) -> bool {
    word.eq_ignore_ascii_case(known)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Open,
    Close,
    Op(Op),
    Quoted(String),
    Word,
    /// What the tokenizer could not read. Nothing follows it, so the parser reports it only when
    /// it gets there, and the leftmost refusal wins.
    Bad(String),
    End,
}

#[derive(Clone, Debug)]
struct Token<'a> {
    kind: Kind,
    column: usize,
    /// The token as written, quotes included.
    text: &'a str,
}

/// The tokens, always closed by `End` or `Bad`.
fn tokenize(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().enumerate().peekable();
    while let Some((index, (start, c))) = chars.next() {
        let mut column = index + 1;
        if c.is_whitespace() {
            continue;
        }
        let mut end = start + c.len_utf8();
        let kind = match c {
            '(' => Kind::Open,
            ')' => Kind::Close,
            '=' => Kind::Op(Op::Eq),
            '<' | '>' | '!' | '^' => {
                if chars.next_if(|(_, (_, next))| *next == '=').is_some() {
                    end += 1;
                    match c {
                        '<' => Kind::Op(Op::Le),
                        '>' => Kind::Op(Op::Ge),
                        '!' => Kind::Op(Op::Ne),
                        _ => Kind::Op(Op::StartsWith),
                    }
                } else {
                    match c {
                        '<' => Kind::Op(Op::Lt),
                        '>' => Kind::Op(Op::Gt),
                        _ => Kind::Bad(format!("unexpected {c}")),
                    }
                }
            }
            '"' => {
                let mut value = String::new();
                let mut kind = Kind::Bad("unterminated string".to_string());
                while let Some((inner_index, (inner_start, inner))) = chars.next() {
                    end = inner_start + inner.len_utf8();
                    match inner {
                        '"' => {
                            kind = Kind::Quoted(std::mem::take(&mut value));
                            break;
                        }
                        '\\' => match chars.next() {
                            Some((_, (next_start, next))) => {
                                end = next_start + next.len_utf8();
                                if next == '"' || next == '\\' {
                                    value.push(next);
                                } else {
                                    kind = Kind::Bad(format!("bad escape \\{next}"));
                                    column = inner_index + 1;
                                    break;
                                }
                            }
                            None => break,
                        },
                        _ => value.push(inner),
                    }
                }
                kind
            }
            _ => {
                while let Some((_, (next_start, next))) =
                    chars.next_if(|(_, (_, next))| !ends_word(*next))
                {
                    end = next_start + next.len_utf8();
                }
                Kind::Word
            }
        };
        let stop = matches!(kind, Kind::Bad(_));
        tokens.push(Token {
            kind,
            column,
            text: &text[start..end],
        });
        if stop {
            return tokens;
        }
    }
    tokens.push(Token {
        kind: Kind::End,
        column: text.trim_end().chars().count() + 1,
        text: "",
    });
    tokens
}

fn ends_word(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '"' | '=' | '!' | '<' | '>' | '^')
}

struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    at: usize,
    pack: &'a Pack,
}

impl<'a> Parser<'a> {
    /// The next token. A token the tokenizer could not read is the refusal.
    fn peek(&self) -> Result<Token<'a>, FilterError> {
        // `tokenize` closes the list with `End` or `Bad`, and `advance` never passes it.
        let token = self.tokens[self.at].clone();
        match token.kind {
            Kind::Bad(what) => Err(refuse(token.column, what)),
            _ => Ok(token),
        }
    }

    fn advance(&mut self) {
        if self.at + 1 < self.tokens.len() {
            self.at += 1;
        }
    }

    /// True, and past it, when the next token is the word `known`.
    fn eat_word(&mut self, known: &str) -> Result<bool, FilterError> {
        let token = self.peek()?;
        let found = token.kind == Kind::Word && is_word(token.text, known);
        if found {
            self.advance();
        }
        Ok(found)
    }

    /// One more level inside `depth`, or the refusal at the token that opens it.
    fn deeper(depth: usize, column: usize) -> Result<usize, FilterError> {
        if depth >= MAX_FILTER_DEPTH {
            return Err(refuse(
                column,
                format!("nesting deeper than {MAX_FILTER_DEPTH}"),
            ));
        }
        Ok(depth + 1)
    }

    fn expr(&mut self, depth: usize, links: usize) -> Result<Expr, FilterError> {
        let mut terms = vec![self.term(depth, links)?];
        while self.eat_word("or")? {
            terms.push(self.term(depth, links)?);
        }
        Ok(one_or(terms, Expr::Or))
    }

    fn term(&mut self, depth: usize, links: usize) -> Result<Expr, FilterError> {
        let mut factors = vec![self.factor(depth, links)?];
        while self.eat_word("and")? {
            factors.push(self.factor(depth, links)?);
        }
        Ok(one_or(factors, Expr::And))
    }

    fn factor(&mut self, depth: usize, links: usize) -> Result<Expr, FilterError> {
        let token = self.peek()?;
        if token.kind == Kind::Open {
            let inner = Self::deeper(depth, token.column)?;
            self.advance();
            let expr = self.expr(inner, links)?;
            self.close()?;
            return Ok(expr);
        }
        if token.kind == Kind::Word && is_word(token.text, "not") {
            let inner = Self::deeper(depth, token.column)?;
            self.advance();
            return Ok(Expr::Not(Box::new(self.factor(inner, links)?)));
        }
        self.test(depth, links)
    }

    fn close(&mut self) -> Result<(), FilterError> {
        let token = self.peek()?;
        if token.kind != Kind::Close {
            return Err(refuse(token.column, "expected \")\"".to_string()));
        }
        self.advance();
        Ok(())
    }

    fn test(&mut self, depth: usize, links: usize) -> Result<Expr, FilterError> {
        let token = self.peek()?;
        let expected = refuse(token.column, "expected a test".to_string());
        if token.kind != Kind::Word {
            return Err(expected);
        }
        let word = token.text;
        if is_word(word, "and") || is_word(word, "or") {
            return Err(expected);
        }
        if is_word(word, "has") {
            self.advance();
            return self.has();
        }
        if is_word(word, "no") {
            self.advance();
            let dir = self.peek()?;
            if dir.kind == Kind::End {
                return Err(refuse(dir.column, "expected a test".to_string()));
            }
            let is_dir =
                dir.kind == Kind::Word && (is_word(dir.text, "out") || is_word(dir.text, "in"));
            if !is_dir {
                return Err(unexpected(&dir));
            }
            self.advance();
            return self.linked(&dir, token.column, true, depth, links);
        }
        if is_word(word, "out") || is_word(word, "in") {
            self.advance();
            return self.linked(&token, token.column, false, depth, links);
        }
        self.compare(&token)
    }

    fn has(&mut self) -> Result<Expr, FilterError> {
        let token = self.peek()?;
        match token.kind {
            Kind::Word => {}
            Kind::End => return Err(refuse(token.column, "expected a test".to_string())),
            _ => return Err(unexpected(&token)),
        }
        let field = field_of(&token)?;
        let applies = matches!(
            field,
            Field::Key
                | Field::Path
                | Field::Doc
                | Field::Area
                | Field::Lifecycle
                | Field::Status
                | Field::Coverage
                | Field::Custom(_)
        );
        if !applies {
            return Err(refuse(
                token.column,
                format!("has does not apply to {}", token.text.to_ascii_lowercase()),
            ));
        }
        self.advance();
        Ok(Expr::Has(field))
    }

    /// The link test after its direction word `dir`. `column` is where the test starts, which is
    /// the `no` of a `no` test.
    fn linked(
        &mut self,
        dir: &Token<'a>,
        column: usize,
        none: bool,
        depth: usize,
        links: usize,
    ) -> Result<Expr, FilterError> {
        if links >= MAX_LINK_NESTING {
            return Err(refuse(
                column,
                format!("more than {MAX_LINK_NESTING} nested link tests"),
            ));
        }
        let dir = if is_word(dir.text, "out") {
            Dir::Out
        } else {
            Dir::In
        };
        let token = self.peek()?;
        if token.kind != Kind::Word {
            return Err(refuse(token.column, "expected a link kind".to_string()));
        }
        let kind = self.link_kind(&token)?;
        self.advance();
        let open = self.peek()?;
        let inner = if open.kind == Kind::Open {
            let inner_depth = Self::deeper(depth, open.column)?;
            self.advance();
            let expr = self.expr(inner_depth, links + 1)?;
            self.close()?;
            Some(Box::new(expr))
        } else {
            None
        };
        Ok(Expr::Linked {
            dir,
            kind,
            inner,
            none,
        })
    }

    fn link_kind(&self, token: &Token<'a>) -> Result<LinkKind, FilterError> {
        let word = token.text;
        if is_word(word, "contains") {
            return Ok(LinkKind::Contains);
        }
        if is_word(word, "references") {
            return Ok(LinkKind::References);
        }
        if let Some(link) = self.pack.link_types.iter().find(|l| is_word(word, l)) {
            return Ok(LinkKind::Link(link.clone()));
        }
        let mut known = vec!["contains", "references"];
        known.extend(self.pack.link_types.iter().map(String::as_str));
        let what = format!("unknown link {}", cut(word));
        Err(refuse(
            token.column,
            did_you_mean(what, &word.to_ascii_lowercase(), &known),
        ))
    }

    fn compare(&mut self, name: &Token<'a>) -> Result<Expr, FilterError> {
        let field = field_of(name)?;
        self.advance();
        let token = self.peek()?;
        let Kind::Op(op) = token.kind else {
            return Err(refuse(token.column, "expected an operator".to_string()));
        };
        if !takes_op(&field, op) {
            return Err(refuse(
                token.column,
                format!(
                    "{} does not apply to {}",
                    token.text,
                    name.text.to_ascii_lowercase()
                ),
            ));
        }
        self.advance();
        let token = self.peek()?;
        let value = value_of(&token)?;
        self.check_value(&field, &value, token.column)?;
        self.advance();
        Ok(Expr::Compare { field, op, value })
    }

    /// The checks of a closed field's value. `fields.<name>`, id, key, path and doc take any.
    fn check_value(&self, field: &Field, value: &Value, column: usize) -> Result<(), FilterError> {
        let text = match value {
            Value::Str(text) | Value::Int { text, .. } | Value::Bool { text, .. } => text.as_str(),
        };
        let shown = cut(text);
        let what = match field {
            Field::Type if !self.pack.block_types.contains_key(text) => {
                let known: Vec<&str> = self.pack.block_types.keys().map(String::as_str).collect();
                did_you_mean(format!("no block type {shown}"), text, &known)
            }
            Field::Area if !self.pack.areas.contains_key(text) => {
                let known: Vec<&str> = self.pack.areas.keys().map(String::as_str).collect();
                did_you_mean(format!("no area {shown}"), text, &known)
            }
            Field::Trait if !TRAITS.contains(&text) => format!("{shown} is not a trait"),
            Field::Lifecycle if !LIFECYCLES.contains(&text) => {
                format!("{shown} is not draft, stable or deprecated")
            }
            Field::Status if !STATUSES.contains(&text) => {
                format!("{shown} is not a decision status")
            }
            Field::Suspect if !matches!(value, Value::Bool { .. }) => {
                "suspect takes true or false".to_string()
            }
            Field::Tension if !matches!(value, Value::Bool { .. }) => {
                "tension takes true or false".to_string()
            }
            Field::Coverage if !matches!(value, Value::Int { value: 0..=100, .. }) => {
                "coverage takes a number from 0 to 100".to_string()
            }
            _ => return Ok(()),
        };
        Err(refuse(column, what))
    }
}

fn one_or(mut list: Vec<Expr>, wrap: fn(Vec<Expr>) -> Expr) -> Expr {
    if list.len() == 1 {
        list.remove(0)
    } else {
        wrap(list)
    }
}

/// The field a word names, in any ASCII case. A custom field keeps its name as written.
fn field_of(token: &Token<'_>) -> Result<Field, FilterError> {
    let word = token.text;
    if let Some(prefix) = word.get(..CUSTOM_PREFIX.len())
        && is_word(prefix, CUSTOM_PREFIX)
    {
        let name = &word[CUSTOM_PREFIX.len()..];
        let good = (1..=MAX_FIELD_NAME).contains(&name.len())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !good {
            return Err(refuse(
                token.column,
                format!(
                    "fields.<name> takes a name of 1 to {MAX_FIELD_NAME} characters from -, _, letters and digits"
                ),
            ));
        }
        return Ok(Field::Custom(name.to_string()));
    }
    let lower = word.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "id" => Field::Id,
        "key" => Field::Key,
        "type" => Field::Type,
        "trait" => Field::Trait,
        "path" => Field::Path,
        "doc" => Field::Doc,
        "area" => Field::Area,
        "lifecycle" => Field::Lifecycle,
        "status" => Field::Status,
        "suspect" => Field::Suspect,
        "tension" => Field::Tension,
        "coverage" => Field::Coverage,
        _ => {
            let what = format!("unknown field {}", cut(word));
            return Err(refuse(
                token.column,
                did_you_mean(what, &lower, &FIELD_NAMES),
            ));
        }
    })
}

/// Which ops a field takes. A `fields.<name>` takes each one, since the block's value decides
/// whether a test holds.
fn takes_op(field: &Field, op: Op) -> bool {
    match field {
        Field::Id | Field::Key | Field::Path => matches!(op, Op::Eq | Op::Ne | Op::StartsWith),
        Field::Type
        | Field::Trait
        | Field::Doc
        | Field::Area
        | Field::Lifecycle
        | Field::Status
        | Field::Suspect
        | Field::Tension => matches!(op, Op::Eq | Op::Ne),
        Field::Coverage => op != Op::StartsWith,
        Field::Custom(_) => true,
    }
}

fn value_of(token: &Token<'_>) -> Result<Value, FilterError> {
    match &token.kind {
        Kind::Quoted(text) => Ok(Value::Str(text.clone())),
        Kind::Word => {
            let word = token.text;
            if RESERVED.iter().any(|known| is_word(word, known)) {
                return Err(refuse(token.column, format!("{} needs quotes", cut(word))));
            }
            if is_number(word) {
                let value = word.parse::<i64>().map_err(|_| {
                    refuse(
                        token.column,
                        "number is outside the signed 64-bit range".to_string(),
                    )
                })?;
                return Ok(Value::Int {
                    value,
                    text: word.to_string(),
                });
            }
            if is_word(word, "true") || is_word(word, "false") {
                return Ok(Value::Bool {
                    value: is_word(word, "true"),
                    text: word.to_string(),
                });
            }
            Ok(Value::Str(word.to_string()))
        }
        _ => Err(refuse(token.column, "expected a value".to_string())),
    }
}

/// `-?[0-9]+`.
fn is_number(word: &str) -> bool {
    let digits = word.strip_prefix('-').unwrap_or(word);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}
