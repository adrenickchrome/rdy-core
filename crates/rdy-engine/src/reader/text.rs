//! A reader's text (p-8Q2FB): the plain words of a field, of a block's own text as the search
//! index reads it, and of a decision's statement. Markdown is flattened, a reference reads as its
//! display text or its target's label, and `\{{` is a literal `{{`. HTML is never stripped and a
//! raw body is never an excerpt: every text is read through [`super::inline`].
use super::inline::{self, Form, code_escape};
use super::labels::{self, LABEL_DEPTH, typed_key, upper_first};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{Block, BlockId, FieldValue, RefMode, RefSpan, decision, entity};
use regex::Regex;
use std::sync::LazyLock;

/// What a reference that leads nowhere reads as on a source output when the author gave no
/// display text.
pub const UNRESOLVED: &str = "unresolved reference";

/// What a reference to a block outside a named profile's selection reads as when the author gave
/// no display text (D-0VCQ5): never the target's label, key or id.
pub const UNAVAILABLE: &str = "unavailable reference";

/// Supporting text: off the site (FW-D10). It has no anchor; a link to it leads to its doc's
/// page.
pub const RATIONALE: &str = "rationale";

/// A publication profile: administrative, off every site and print output, removed or not. It
/// has no item, anchor, permalink, search entry or text of its own; a link to it leads to its
/// doc's page and reads as that doc.
pub const PUBLICATION_PROFILE: &str = rdy_schema::publication::PROFILE_TYPE;

/// Whether `b` is supporting content off the site and every print: a rationale (FW-D10), or a
/// finding or a source, which live only in the store (D-Z8P09). None has an anchor or a place in
/// the text; a link to one leads to its doc's page.
pub fn is_off_site(b: &Block) -> bool {
    matches!(
        b.block_type.as_str(),
        RATIONALE | rdy_schema::supporting::FINDING | rdy_schema::supporting::SOURCE
    )
}

/// Whether `b` is a publication profile ([`PUBLICATION_PROFILE`]).
pub fn is_profile(b: &Block) -> bool {
    b.block_type == PUBLICATION_PROFILE
}

/// One side as a reader reads it: the facts the text and label rules take as inputs. The site
/// (rdy-render's `Site`) supplies them, and may cache [`Reading::label_at`] and
/// [`Reading::summary_words`].
pub trait Reading {
    /// The block `id` names on this side.
    fn block(&self, id: &BlockId) -> Option<&Block>;
    /// The doc that holds `id` (itself for a doc).
    fn doc_of(&self, id: &BlockId) -> Option<&Block>;
    /// The tree name of a doc or folder.
    fn tree_name(&self, id: &BlockId) -> Option<String>;
    /// `b`'s number among the blocks of its kind in its doc, from 1 in reading order; 1 when it
    /// has none.
    fn number(&self, b: &Block) -> usize;
    /// A named profile's words for a reference that leads nowhere ([`UNAVAILABLE`]); none for a
    /// source output.
    fn unavailable(&self) -> Option<&'static str>;
    /// Whether the output drops a reference to `id` (D-7M0YF): a named profile lacks the block,
    /// and it is a decision outside the selection.
    fn drops(&self, id: &BlockId) -> bool;
    /// What a reader calls `b` at reference depth `depth` ([`labels::label`]).
    fn label_at(&self, b: &Block, depth: usize) -> String {
        labels::label(self, b, depth)
    }
    /// `b`'s summary words, uncut ([`labels::summary_words`]).
    fn summary_words(&self, b: &Block, depth: usize, keep_token: bool) -> String {
        labels::summary_words(self, b, depth, keep_token)
    }
}

/// Whether the output drops the reference `span` with the parentheses it empties: it has no
/// display words and [`Reading::drops`] its target.
pub fn drops_ref<R: Reading + ?Sized>(r: &R, span: &RefSpan) -> bool {
    span.display.is_none() && BlockId::parse(&span.token).is_ok_and(|id| r.drops(&id))
}

/// What a reference reads as in plain words: the notice when its target is removed, whatever
/// display text the author gave (D-3M81W); else its display text; else its target's current key
/// when typed as one and the target has one, else the target's label (`…` past
/// [`LABEL_DEPTH`]); else nothing when the output drops it, a named profile's unavailable words,
/// or `unresolved(token)` when it leads nowhere.
pub fn ref_text<R: Reading + ?Sized>(
    r: &R,
    span: &RefSpan,
    depth: usize,
    unresolved: &dyn Fn(&str) -> String,
) -> String {
    let target = BlockId::parse(&span.token).ok().and_then(|id| r.block(&id));
    if target.is_some_and(|t| t.is_redacted()) {
        return CONTENT_REMOVED.to_string();
    }
    if let Some(d) = &span.display {
        return d.clone();
    }
    match target {
        Some(_) if depth >= LABEL_DEPTH => "…".to_string(),
        Some(t) => match typed_key(t).filter(|k| span.key_typed && !k.is_empty()) {
            Some(k) => k.to_string(),
            None => r.label_at(t, depth + 1),
        },
        // a decision outside a named profile's selection: no words of its own (D-7M0YF)
        None if drops_ref(r, span) => String::new(),
        None => match r.unavailable() {
            Some(words) => words.to_string(),
            None => unresolved(&span.token),
        },
    }
}

/// `text` as plain words in `form` ([`inline::plain_refs_every`]): a reference reads as
/// [`ref_text`] says at reference depth `depth`.
pub fn plain<R: Reading + ?Sized>(
    r: &R,
    text: &str,
    mode: RefMode,
    form: Form,
    depth: usize,
    unresolved: &dyn Fn(&str) -> String,
) -> String {
    inline::plain_refs_every(
        text,
        mode,
        form,
        &|span| ref_text(r, span, depth, unresolved),
        &|span| drops_ref(r, span),
    )
}

/// As [`plain`], but each code span of `text` stands between the code marks
/// ([`inline::plain_refs_coded`]): a word diff keeps a code span code (the task-6 ruling).
pub fn plain_coded<R: Reading + ?Sized>(
    r: &R,
    text: &str,
    mode: RefMode,
    form: Form,
    unresolved: &dyn Fn(&str) -> String,
) -> String {
    inline::plain_refs_coded(
        text,
        mode,
        form,
        &|span| ref_text(r, span, 0, unresolved),
        &|span| drops_ref(r, span),
    )
}

/// A field's text as a label ([`Form::Label`]): a code span's text, a reference as
/// [`ref_text`] says, an unresolved one as [`UNRESOLVED`], `\{{` as `{{`.
pub fn words<R: Reading + ?Sized>(r: &R, text: &str, depth: usize) -> String {
    plain(r, text, RefMode::Strict, Form::Label, depth, &|_| {
        UNRESOLVED.to_string()
    })
}

/// A field read as written (a value and its unit, a code language, a profile's title):
/// unchanged for a source output; for a named profile, each reference in it reads as
/// [`ref_text`] says, never its token, and nothing else changes but `\{{` as `{{`.
pub fn as_written<R: Reading + ?Sized>(r: &R, text: &str, depth: usize) -> String {
    match r.unavailable() {
        None => text.to_string(),
        Some(_) => inline::source_plain(text, RefMode::Strict, &|span| {
            ref_text(r, span, depth, &|_| String::new())
        }),
    }
}

/// Whether a field read as written ([`as_written`]) holds a reference that a named profile reads
/// as words. Always false for a source output.
pub fn reads_ref_words<R: Reading + ?Sized>(r: &R, text: &str) -> bool {
    r.unavailable().is_some() && !rdy_model::ref_spans(text, RefMode::Strict).is_empty()
}

/// A folder's description as plain words on one line: it is running text (spans), a reference
/// with no display text reads as its target's label, one that leads nowhere as nothing.
pub fn description_words<R: Reading + ?Sized>(r: &R, text: &str) -> String {
    let words = plain(r, text, RefMode::Strict, Form::Spans, 0, &|_| String::new());
    words.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How a block's own text is read: as the search index reads it, or `coded`, with each code
/// span between the code marks for a word diff ([`plain_coded`]).
struct Read<'r, R: ?Sized> {
    r: &'r R,
    coded: bool,
}

impl<R: Reading + ?Sized> Read<'_, R> {
    fn plain(&self, text: &str, mode: RefMode, form: Form) -> String {
        if self.coded {
            plain_coded(self.r, text, mode, form, &|_| String::new())
        } else {
            plain(self.r, text, mode, form, 0, &|_| String::new())
        }
    }

    /// Text read as it is, with no Markdown: in the coded form, escaped ([`code_escape`]).
    fn raw(&self, text: &str) -> String {
        if self.coded {
            code_escape(text)
        } else {
            text.to_string()
        }
    }

    fn field(&self, b: &Block, name: &str) -> String {
        let form = match name {
            "statement" | "prompt" => Form::Spans,
            _ => Form::Label,
        };
        self.plain(b.field_str(name).unwrap_or(""), RefMode::Strict, form)
    }

    fn cell(&self, v: &FieldValue) -> String {
        match v {
            FieldValue::Str(s) => self.plain(s, RefMode::Strict, Form::Spans),
            FieldValue::Bool(b) => b.to_string(),
            FieldValue::Int(i) => i.to_string(),
            _ => String::new(),
        }
    }
}

/// A field's words as the search index reads them: a statement or prompt reads as running text,
/// any other field as a label. A reference that leads nowhere reads as nothing.
pub fn field<R: Reading + ?Sized>(r: &R, b: &Block, name: &str) -> String {
    Read { r, coded: false }.field(b, name)
}

/// The text a reader can find in `b` itself (not in its children), with its own whitespace:
/// what the search index reads, field by field for each block type.
pub fn own_text<R: Reading + ?Sized>(r: &R, b: &Block) -> String {
    own_text_in(b, Read { r, coded: false })
}

/// As [`own_text`], with each code span of the block's spans between the code marks
/// ([`inline::plain_refs_coded`]) and every authored character kept, escaped where it is a mark
/// character ([`code_escape`]): the word diff of a change summary reads it (sr6).
pub fn own_text_coded<R: Reading + ?Sized>(r: &R, b: &Block) -> String {
    own_text_in(b, Read { r, coded: true })
}

fn own_text_in<R: Reading + ?Sized>(b: &Block, r: Read<'_, R>) -> String {
    // a publication profile, removed or not, is off every output: it has no words here
    if is_profile(b) {
        return String::new();
    }
    // a removed block of any other type says the notice, before its type's fields are read
    // (D-3M81W); a decision, removed or not, has no place in the text
    if b.is_redacted() && b.block_type != decision::DECISION {
        return CONTENT_REMOVED.to_string();
    }
    let body = || r.plain(&b.body, RefMode::for_body(&b.block_type), Form::Body);
    let list = |name: &str| match b.fields.get(name) {
        Some(FieldValue::List(l)) => l.clone(),
        _ => vec![],
    };
    match b.block_type.as_str() {
        // a decision has no place in the text
        decision::DECISION => String::new(),
        "term" | "component" | "actor" | "interface" | "entity" => {
            format!("{}: {}", r.field(b, "name"), body())
        }
        "value" => {
            let v = r.raw(&as_written(
                r.r,
                &entity::value_text(b).unwrap_or_default(),
                0,
            ));
            format!("{}: {v} {}", r.field(b, "name"), body())
                .trim_end()
                .to_string()
        }
        "question" => r.field(b, "prompt"),
        "diagram" | "figure" => r.field(b, "alt"),
        "code" => r.raw(&b.body),
        "table" => {
            let rows = list("rows").into_iter().map(|row| match row {
                FieldValue::List(cells) => cells
                    .iter()
                    .map(|c| r.cell(c))
                    .collect::<Vec<_>>()
                    .join(" "),
                other => r.cell(&other),
            });
            list("columns")
                .iter()
                .map(|c| r.cell(c))
                .chain(rows)
                .collect::<Vec<_>>()
                .join(" ")
        }
        "flow" => {
            let steps = list("steps").into_iter().map(|s| match s {
                FieldValue::Str(t) => r.plain(&t, RefMode::Strict, Form::Spans),
                other => r.cell(&other),
            });
            std::iter::once(r.field(b, "name"))
                .chain(steps)
                .chain(std::iter::once(body()))
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => body(),
    }
}

/// The prefix a revocation's statement opens with: the id it revokes is never a reader's word.
static REVOKE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Revoke [A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}: ").expect("regex"));

/// A decision's statement as its trail shows it, still Markdown: the notice for a removed
/// decision (D-3M81W); for a decision that revokes (`revokes`), its reason, without the id it
/// names, its first letter upper-cased; else the statement as written.
pub fn decision_statement_source(d: &Block, revokes: bool) -> String {
    if d.is_redacted() {
        return CONTENT_REMOVED.to_string();
    }
    let raw = d.field_str("statement").unwrap_or("");
    if revokes {
        upper_first(&REVOKE_PREFIX.replace(raw, ""))
    } else {
        raw.to_string()
    }
}

/// A decision's statement as a reader reads it (D-9BWMV): [`decision_statement_source`] read as
/// running text ([`Form::Spans`]) with the search index's rules, a reference that leads nowhere
/// reading as nothing.
pub fn decision_statement<R: Reading + ?Sized>(r: &R, d: &Block, revokes: bool) -> String {
    if d.is_redacted() {
        return CONTENT_REMOVED.to_string();
    }
    let source = decision_statement_source(d, revokes);
    Read { r, coded: false }.plain(&source, RefMode::Strict, Form::Spans)
}
