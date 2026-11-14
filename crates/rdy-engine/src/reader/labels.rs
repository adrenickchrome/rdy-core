//! What a reader calls a block (p-SHBDB): its label, its one-line summary, the cut that holds a
//! label to [`LABEL_MAX`] characters, and the one table of code language names.
//!
//! The rules take what depends on the whole site as inputs through [`Reading`]: a block's
//! number, its doc, its tree name, and the label of each block a reference names.
use super::inline::{Ch, Decoder, Form, source_plain};
use super::text::{self, Reading, as_written, is_profile, plain, plain_coded, reads_ref_words};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{Block, BlockId, RefMode, entity, tree};
use regex::Regex;
use std::sync::LazyLock;

/// The most characters of a change's label (O1). Code-span backticks are marks, not characters,
/// so they are never counted.
pub const LABEL_MAX: usize = 40;

/// The most characters of a block's one-line summary ([`summary`]).
pub const SUMMARY_MAX: usize = 80;

/// How many references deep a label reads its references' labels; past it a reference reads
/// as `…` (a title that names a block whose title names it back stops here).
pub const LABEL_DEPTH: usize = 3;

/// The id inside a longer text, as the leak scan reads it.
pub static ID_INSIDE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}\b").expect("a pattern"));

/// The key a reference typed as a key (`{{id|@key}}`) reads as, if the target has one. A doc, a
/// folder or a section has a title a reader knows it by, so a key-typed reference to one reads as
/// that title, as `{{id}}` does (FW-F2b); a value reads as its value (FW-EN2); any other keyed
/// block reads as its key (FW-F2a). A publication profile's key is its own text, off the site:
/// a key-typed reference to one reads as its doc, as `{{id}}` does.
pub fn typed_key(b: &Block) -> Option<&str> {
    match b.block_type.as_str() {
        tree::DOC | tree::FOLDER | "section" | entity::VALUE | text::PUBLICATION_PROFILE => None,
        _ => b.field_str("key"),
    }
}

// ---- the cut ------------------------------------------------------------------------------

/// `text` with its whitespace collapsed, cut at a word boundary to at most `max` characters, with
/// `…` when something was cut. A collapsed space keeps the code span of the first whitespace it
/// stands for; a cut inside a code span ends the span there.
pub fn cut(text: &[Ch], max: usize) -> Vec<Ch> {
    let mut words: Vec<Ch> = Vec::with_capacity(text.len());
    let mut space: Option<u32> = None;
    for &(c, span) in text {
        if c.is_whitespace() {
            space.get_or_insert(span);
            continue;
        }
        if let Some(s) = space.take().filter(|_| !words.is_empty()) {
            words.push((' ', s));
        }
        words.push((c, span));
    }
    if words.len() <= max {
        return words;
    }
    // the cut falls on a word boundary when the next character is a space
    let mut kept = &words[..max];
    if words[max].0 != ' '
        && let Some(i) = kept.iter().rposition(|c| c.0 == ' ')
    {
        kept = &kept[..i];
    }
    let end = kept
        .iter()
        .rposition(|c| ![' ', ',', ';', ':', '.', '-'].contains(&c.0))
        .map_or(0, |i| i + 1);
    let mut out = kept[..end].to_vec();
    out.push(('…', 0));
    out
}

/// Plain `text` cut as [`cut`] says, as a string: whitespace collapsed, at most `max` characters
/// (a word boundary when one falls in them), and one `…` when something was cut. A character has
/// no code span here.
pub fn cut_plain(text: &str, max: usize) -> String {
    let chars: Vec<Ch> = text.chars().map(|c| (c, 0)).collect();
    cut(&chars, max).into_iter().map(|(c, _)| c).collect()
}

/// A change's label from plain `text` (a heading, a name, the notice): cut to [`LABEL_MAX`].
pub fn plain_label(text: &str) -> String {
    cut_plain(text, LABEL_MAX)
}

/// A change's label from characters with their code spans ([`label_coded`]): cut to
/// [`LABEL_MAX`], each code span kept.
pub fn coded_label(chars: &[Ch]) -> Vec<Ch> {
    cut(chars, LABEL_MAX)
}

// ---- language names -----------------------------------------------------------------------

/// Every code language a reader names: the names it goes by in a code block's `lang` (lower
/// case, the first is the language's own), and what a reader calls it. The highlighter keys its
/// rules by the first name ([`language`]).
const LANGUAGES: &[(&[&str], &str)] = &[
    (&["python", "py", "python3"], "Python"),
    (&["yaml", "yml"], "YAML"),
    (&["json"], "JSON"),
    (&["rust", "rs"], "Rust"),
    (&["sh", "bash", "zsh", "shell", "console"], "Shell"),
    (&["toml"], "TOML"),
    (&["peg"], "Grammar"),
    (&["mermaid"], "Mermaid"),
    (&["d2"], "D2"),
    (&["", "text", "txt", "plain"], "Text"),
    (&["html"], "HTML"),
    (&["css"], "CSS"),
    (&["js", "javascript"], "JavaScript"),
    (&["ts", "typescript"], "TypeScript"),
    (&["sql"], "SQL"),
    (&["md", "markdown"], "Markdown"),
];

/// The language `lang` names (trimmed, any case), by its own first name; none for a language
/// the table does not hold.
pub fn language(lang: &str) -> Option<&'static str> {
    let name = lang.trim().to_lowercase();
    LANGUAGES
        .iter()
        .find(|(names, _)| names.contains(&name.as_str()))
        .map(|(names, _)| names[0])
}

/// The language as a reader names it, for the code label: the table's name, else `lang` trimmed
/// with its first letter upper-cased.
pub fn human_name(lang: &str) -> String {
    let name = lang.trim().to_lowercase();
    if let Some((_, human)) = LANGUAGES
        .iter()
        .find(|(names, _)| names.contains(&name.as_str()))
    {
        return (*human).to_string();
    }
    upper_first(lang.trim())
}

// ---- labels -------------------------------------------------------------------------------

/// What a reader calls `b` at reference depth `depth`: its title, else its `name` field, else
/// (for a doc or folder) its tree name worded as a title, else (for code) its language and number
/// (`Python code 3`), else (for a diagram) its alt text or number (`Diagram 2`), else the start of
/// its text. A reference in the title, name or alt reads as its target's label. Never an id, and
/// never code or diagram source. Uncut and uncached: [`Reading::label_at`] may cache it.
pub fn label<R: Reading + ?Sized>(r: &R, b: &Block, depth: usize) -> String {
    let label = match label_from(b) {
        // a publication profile, removed or not, has no words on the site: it is called by
        // its doc, where a link to it leads
        LabelFrom::Profile => {
            return r
                .doc_of(&b.id)
                .map(|d| r.label_at(d, depth))
                .unwrap_or_default();
        }
        // a removed block is called by the notice, never by its id path or old words
        // (D-3M81W)
        LabelFrom::Removed => return CONTENT_REMOVED.to_string(),
        LabelFrom::Value(v) => return as_written(r, &v, depth),
        LabelFrom::Said(t) => text::words(r, t, depth),
        LabelFrom::TreeName => r
            .tree_name(&b.id)
            .map(|n| name_as_title(&n, &b.id))
            .unwrap_or_default(),
        LabelFrom::Code => {
            let k = r.number(b);
            // for a named profile, a language that holds a reference reads as its words
            // exactly, never worded as a language's name
            let lang = b.field_str("lang").map(|l| (l, as_written(r, l, depth)));
            match lang.filter(|(_, w)| !w.trim().is_empty()) {
                Some((l, w)) if reads_ref_words(r, l) => format!("{} code {k}", w.trim()),
                Some((_, w)) => format!("{} code {k}", human_name(&w)),
                None => format!("Code {k}"),
            }
        }
        LabelFrom::Diagram => match b.field_str("alt").filter(|a| !a.trim().is_empty()) {
            Some(a) => upper_first(&text::words(r, a, depth)),
            None => format!("Diagram {}", r.number(b)),
        },
        LabelFrom::Summary => summary_at(r, b, depth, true),
    };
    if label.trim().is_empty() {
        "Untitled".to_string()
    } else {
        label
    }
}

/// [`label`] as characters with their code spans ([`plain_coded`], read back) when the label is
/// the start of `b`'s text (its summary words), so that a code span there stays code (ruling
/// task-2). `None` when the label is a title, a name, a tree name, a value, a code or diagram
/// label, a figure's source text, the notice, or empty (`Untitled`): [`label`] then gives it.
/// Uncut. Each id-shaped run of the plain characters reads as one plain `…`, as in [`label`],
/// even when a code span holds or splits it.
pub fn label_coded<R: Reading + ?Sized>(r: &R, b: &Block) -> Option<Vec<Ch>> {
    if !matches!(label_from(b), LabelFrom::Summary) {
        return None;
    }
    // an unresolved reference keeps its token, then every id-shaped word is `…`, as the
    // label's summary reads it
    let read = |t: &str, mode: RefMode, form: Form| {
        let coded = plain_coded(r, t, mode, form, &|token| token.to_string());
        Decoder::default().read(&coded)
    };
    let blank = |chars: &[Ch]| chars.iter().all(|c| c.0.is_whitespace());
    let chars = match summary_from(b) {
        SummaryFrom::Said(t, form) => read(t, RefMode::Strict, form),
        // source is not Markdown: it holds no code span
        SummaryFrom::Text {
            body: Some(_),
            source: true,
            ..
        } => return None,
        SummaryFrom::Text { body, tail, .. } => body
            .map(|t| read(t, RefMode::for_body(&b.block_type), Form::Body))
            .filter(|w| !blank(w))
            .or_else(|| tail.map(|t| read(t, RefMode::Strict, Form::Label)))
            .unwrap_or_default(),
    };
    let chars = hide_ids(&chars);
    (!blank(&chars)).then_some(chars)
}

/// `b`'s one-line summary as a reader reads it: its statement, prompt, name or title, else its
/// body, else its alt text or caption, as plain words (the form that fits the field), whitespace
/// collapsed and cut to [`SUMMARY_MAX`] characters. A reference reads as its display text or its
/// target's label, one that leads nowhere keeps its id as written, and any id left in the words
/// is read as `…`.
pub fn summary<R: Reading + ?Sized>(r: &R, b: &Block) -> String {
    summary_at(r, b, 0, true)
}

/// [`summary`] at reference depth `depth`; `keep_token` as [`summary_words`] says.
pub fn summary_at<R: Reading + ?Sized>(r: &R, b: &Block, depth: usize, keep_token: bool) -> String {
    let words = r.summary_words(b, depth, keep_token);
    let words = ID_INSIDE.replace_all(&words, "…");
    words.chars().take(SUMMARY_MAX).collect()
}

/// The summary's words, uncut and uncached ([`Reading::summary_words`] may cache them).
/// `keep_token` leaves an unresolved reference as its token (then every id-shaped word is `…`);
/// otherwise it reads as the unresolved marker's words.
pub fn summary_words<R: Reading + ?Sized>(
    r: &R,
    b: &Block,
    depth: usize,
    keep_token: bool,
) -> String {
    // a publication profile, removed or not, has no words on the site: a trail's reference
    // to one reads as its doc, where the reference leads (as its label does)
    if is_profile(b) {
        return r
            .doc_of(&b.id)
            .map(|d| r.summary_words(d, depth, keep_token))
            .unwrap_or_default();
    }
    if b.is_redacted() {
        return CONTENT_REMOVED.to_string();
    }
    let unresolved_of = |token: &str| match keep_token {
        true => token.to_string(),
        false => text::UNRESOLVED.to_string(),
    };
    let read = |t: &str, mode: RefMode, form: Form| {
        let words = plain(r, t, mode, form, depth, &unresolved_of);
        words.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let (body, source, tail) = match summary_from(b) {
        SummaryFrom::Said(t, form) => return read(t, RefMode::Strict, form),
        SummaryFrom::Text { body, source, tail } => (body, source, tail),
    };
    let mode = RefMode::for_body(&b.block_type);
    let words = match body {
        // source is not Markdown: its words are its text, each reference in it (none in
        // code) read as on the drawn diagram, so no id is quoted
        Some(t) if source => {
            let words = source_plain(t, mode, &|span| {
                text::ref_text(r, span, depth, &unresolved_of)
            });
            words.split_whitespace().collect::<Vec<_>>().join(" ")
        }
        Some(t) => read(t, mode, Form::Body),
        None => String::new(),
    };
    if !words.is_empty() {
        return words;
    }
    tail.map(|t| read(t, RefMode::Strict, Form::Label))
        .unwrap_or_default()
}

/// What [`label`] calls a block, by the first rule that fits it (one rule for the label and its
/// coded form, [`label_coded`]).
enum LabelFrom<'b> {
    /// A publication profile: its doc's label.
    Profile,
    /// A removed block: the notice.
    Removed,
    /// A value: its value and unit as written.
    Value(String),
    /// Its title, else (not for a doc or folder) its `name` field.
    Said(&'b str),
    /// A doc's or folder's tree name, worded as a title.
    TreeName,
    /// Code: its language and number.
    Code,
    /// A diagram: its alt text or number.
    Diagram,
    /// The start of its text: its summary words ([`summary_from`]).
    Summary,
}

fn label_from(b: &Block) -> LabelFrom<'_> {
    if is_profile(b) {
        return LabelFrom::Profile;
    }
    if b.is_redacted() {
        return LabelFrom::Removed;
    }
    if let Some(v) = entity::value_text(b) {
        return LabelFrom::Value(v);
    }
    let tree = tree::is_tree_type(&b.block_type);
    let said = b
        .field_str("title")
        .filter(|t| !t.is_empty())
        .or_else(|| b.field_str("name").filter(|t| !t.is_empty() && !tree));
    match said {
        Some(t) => LabelFrom::Said(t),
        None if tree => LabelFrom::TreeName,
        None if b.block_type == "code" => LabelFrom::Code,
        None if b.block_type == "diagram" => LabelFrom::Diagram,
        None => LabelFrom::Summary,
    }
}

/// Where a block's summary words come from ([`summary_words`], [`label_coded`]).
enum SummaryFrom<'b> {
    /// The first of its statement, prompt, name and title (a doc's or folder's title, then
    /// name) that is not empty, read in its form.
    Said(&'b str, Form),
    /// Its body when not empty (`source` for code, a diagram or a figure: not Markdown); when
    /// that reads as nothing, the first of its alt text and caption that is not blank.
    Text {
        body: Option<&'b str>,
        source: bool,
        tail: Option<&'b str>,
    },
}

fn summary_from(b: &Block) -> SummaryFrom<'_> {
    let said: &[(&str, Form)] = if tree::is_tree_type(&b.block_type) {
        &[("title", Form::Label), ("name", Form::Label)]
    } else {
        &[
            ("statement", Form::Spans),
            ("prompt", Form::Spans),
            ("name", Form::Label),
            ("title", Form::Label),
        ]
    };
    if let Some((t, form)) = said
        .iter()
        .find_map(|(f, form)| b.field_str(f).filter(|s| !s.is_empty()).map(|t| (t, form)))
    {
        return SummaryFrom::Said(t, *form);
    }
    SummaryFrom::Text {
        body: (!b.body.is_empty()).then_some(b.body.as_str()),
        source: matches!(b.block_type.as_str(), "code" | "diagram" | "figure"),
        tail: ["alt", "caption"]
            .iter()
            .find_map(|f| b.field_str(f).filter(|s| !s.trim().is_empty())),
    }
}

/// `chars` with each id-shaped run of its plain characters ([`ID_INSIDE`]) read as one plain
/// `…`, whatever code spans hold or split it.
pub fn hide_ids(chars: &[Ch]) -> Vec<Ch> {
    let plain: String = chars.iter().map(|c| c.0).collect();
    let starts: Vec<usize> = plain.char_indices().map(|(i, _)| i).collect();
    let at = |byte: usize| starts.partition_point(|&s| s < byte);
    let mut out = Vec::with_capacity(chars.len());
    let mut from = 0;
    for m in ID_INSIDE.find_iter(&plain) {
        let (a, z) = (at(m.start()), at(m.end()));
        out.extend_from_slice(&chars[from..a]);
        out.push(('…', 0));
        from = z;
    }
    out.extend_from_slice(&chars[from..]);
    out
}

/// `s` with its first letter upper-cased.
pub fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A tree name worded as a title: `-` becomes a space, the first letter is upper-cased, and the
/// name `untitled-<id>` that a doc without a title gets (the one `id` gives) reads as `Untitled`.
pub fn name_as_title(name: &str, id: &BlockId) -> String {
    if name == tree::default_name(None, id) {
        return "Untitled".to_string();
    }
    upper_first(&name.replace('-', " "))
}
