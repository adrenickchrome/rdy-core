//! The side-view seam of the publication review (D-W4YAE): what each row builder reads of one
//! side of a comparison. [`PublicationReviewSide`] is read-only. [`ReaderReviewSide`] builds it
//! from a resolved same-store side, after profile filtering and after the removals of both sides
//! are applied ([`ReviewRemovals`]). A future baseline view builds the same facts from validated
//! baseline metadata, with part hashes and no reader text.
//!
//! The compared parts follow D-MCH70's closed table ([`BlockParts`]): each part is the authored
//! value ([`ReaderPartValue`]), never rendered text. A part hash is BLAKE3 over the RFC 8785
//! bytes of the part's tagged form ([`ReaderPartValue::tagged`]). Reader text stays apart from
//! the parts: it feeds excerpts and short diffs only.
//!
//! Reference labels read a separate lookup into the captured candidate source
//! ([`ReferenceLookup`]). It may name a target outside the selection for the authorizer. It never
//! widens the reader view, its counts, its excerpts or its search text.
//!
//! No fact here enters a selection hash, a selection identity or a doc digest (D-B2AC3).

use super::diff::removed_versions;
use super::references::ReferenceReport;
use super::select::{AttachmentEntry, DocEntry, Resolved, SelectionEntry};
use super::source::PublicationSource;
use crate::as_built::{Derived, Status};
use crate::error::RdyError;
use crate::materialize::Materialized;
use crate::reader::inline::Ch;
use crate::reader::labels;
use crate::reader::review::mask_removed;
use crate::reader::shape::{self, NUMBERED, Shape};
use crate::reader::text::{self, Reading};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{Block, BlockId, FieldValue, Hash, decision, tree};
use rdy_schema::publication::Audience;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

// ---- RFC 8785 -----------------------------------------------------------------------------

/// What the RFC 8785 encoder refuses: a JSON number. The tagged part form holds none, so the
/// number rules of RFC 8785 do not apply to it. A caller that needs numbers adds its own rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalNumber;

/// The RFC 8785 (JSON Canonicalization Scheme) text of `v`: no whitespace, object keys sorted by
/// their UTF-16 code units (RFC 8785 section 3.2.3), and strings escaped as ECMAScript
/// `JSON.stringify` escapes them. A number is refused ([`CanonicalNumber`]).
pub(crate) fn rfc8785(v: &Value) -> Result<String, CanonicalNumber> {
    let mut out = String::new();
    write_canonical(v, &mut out)?;
    Ok(out)
}

fn write_canonical(v: &Value, out: &mut String) -> Result<(), CanonicalNumber> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(_) => return Err(CanonicalNumber),
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in utf16_sorted(map).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_canonical(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// The members of `map` sorted by the UTF-16 code units of their keys. Rust's byte order differs
/// from it for keys that mix U+E000..U+FFFF with characters above U+FFFF.
fn utf16_sorted(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut members: Vec<(&String, &Value)> = map.iter().collect();
    members.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
    members
}

/// `s` as a JSON string, escaped as `JSON.stringify` escapes it: `\"`, `\\`, `\b`, `\f`, `\n`,
/// `\r`, `\t`, every other control below U+0020 as `\u00xx` in lower-case hex, and nothing else.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---- part values --------------------------------------------------------------------------

/// One authored part value (D-MCH70). It keeps scalar types, list order, map values and field
/// presence: a missing `field:<name>` part is [`ReaderPartValue::Absent`], and an authored null
/// stays [`ReaderPartValue::Null`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderPartValue {
    Absent,
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<ReaderPartValue>),
    Map(BTreeMap<String, ReaderPartValue>),
}

/// The value of a part a side does not hold.
static ABSENT: ReaderPartValue = ReaderPartValue::Absent;

impl ReaderPartValue {
    /// The authored field value `v`, unchanged.
    pub fn of_field(v: &FieldValue) -> ReaderPartValue {
        match v {
            FieldValue::Null => ReaderPartValue::Null,
            FieldValue::Bool(b) => ReaderPartValue::Bool(*b),
            FieldValue::Int(i) => ReaderPartValue::Int(*i),
            FieldValue::Str(s) => ReaderPartValue::Str(s.clone()),
            FieldValue::List(l) => ReaderPartValue::List(l.iter().map(Self::of_field).collect()),
            FieldValue::Map(m) => ReaderPartValue::Map(
                m.iter()
                    .map(|(k, v)| (k.clone(), Self::of_field(v)))
                    .collect(),
            ),
        }
    }

    /// The tagged form that the hash reads: `["absent"]`, `["null"]`, `["bool", b]`,
    /// `["int", "<signed decimal>"]`, `["str", s]`, `["list", [..]]` or `["map", {..}]`. An
    /// integer is a decimal string, so the full `i64` range survives; the form holds no number.
    pub fn tagged(&self) -> Value {
        let tag = |t: &str| Value::String(t.to_string());
        match self {
            ReaderPartValue::Absent => Value::Array(vec![tag("absent")]),
            ReaderPartValue::Null => Value::Array(vec![tag("null")]),
            ReaderPartValue::Bool(b) => Value::Array(vec![tag("bool"), Value::Bool(*b)]),
            ReaderPartValue::Int(i) => Value::Array(vec![tag("int"), Value::String(i.to_string())]),
            ReaderPartValue::Str(s) => Value::Array(vec![tag("str"), Value::String(s.clone())]),
            ReaderPartValue::List(l) => Value::Array(vec![
                tag("list"),
                Value::Array(l.iter().map(Self::tagged).collect()),
            ]),
            ReaderPartValue::Map(m) => Value::Array(vec![
                tag("map"),
                Value::Object(m.iter().map(|(k, v)| (k.clone(), v.tagged())).collect()),
            ]),
        }
    }

    /// The RFC 8785 text of [`ReaderPartValue::tagged`]: the exact UTF-8 bytes the hash reads,
    /// with no prefix and no trailing newline.
    pub fn canonical(&self) -> String {
        rfc8785(&self.tagged()).expect("the tagged part form holds no number")
    }

    /// The part hash (D-W4YAE): BLAKE3 over [`ReaderPartValue::canonical`], as 64 lower-case hex
    /// digits.
    pub fn hash(&self) -> String {
        blake3::hash(self.canonical().as_bytes())
            .to_hex()
            .to_string()
    }
}

/// The name of one compared part. The order is the order of a change row's `what`: `title`,
/// `text`, `statement`, `type`, `links`, `status`, then each `field:<name>` by name. No block
/// has both `text` and `statement`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PartName {
    Title,
    Text,
    Statement,
    Type,
    Links,
    Status,
    Field(String),
}

impl fmt::Display for PartName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartName::Title => f.write_str("title"),
            PartName::Text => f.write_str("text"),
            PartName::Statement => f.write_str("statement"),
            PartName::Type => f.write_str("type"),
            PartName::Links => f.write_str("links"),
            PartName::Status => f.write_str("status"),
            PartName::Field(name) => write!(f, "field:{name}"),
        }
    }
}

/// The fields that a block type's `text` part covers, besides `body` (D-MCH70). Other ordinary
/// block types cover none.
pub fn text_fields(block_type: &str) -> &'static [&'static str] {
    match block_type {
        "term" | "component" | "actor" | "interface" | "entity" => &["name"],
        "value" => &["name", "value", "unit"],
        "question" => &["prompt"],
        "diagram" | "figure" => &["alt"],
        "table" => &["columns", "rows"],
        "flow" => &["name", "steps"],
        _ => &[],
    }
}

/// The fields a doc or folder compares, besides `title`.
const TREE_FIELDS: [&str; 2] = ["name", "description"];

/// The compared parts of one block, by D-MCH70's closed table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockParts(BTreeMap<PartName, ReaderPartValue>);

impl BlockParts {
    /// The parts of `b`. `status` is the side's status label for `b`: the shown status that the
    /// doc digest writes, for an ordinary block; the derived status tag, for a decision; `None`
    /// for unversioned content. A doc or folder ignores it.
    ///
    /// - A doc or folder: `title`, `field:name` and `field:description`.
    /// - A decision: `title`, the authored `statement` and `status`.
    /// - Any other block: `title`, `text`, `type`, `links`, `status`, and `field:<name>` for each
    ///   other field. `text` is a map of `body` and each covered field the block has
    ///   ([`text_fields`]). `links` maps each kind to its links in order; a link keeps its
    ///   `target` and, when set, its `part`. Pins are never a part.
    ///
    /// A missing `title` is null. A missing field gives no `field:<name>` part.
    pub fn of(b: &Block, status: Option<&str>) -> BlockParts {
        let mut parts = BTreeMap::new();
        let title = b
            .fields
            .get("title")
            .map_or(ReaderPartValue::Null, ReaderPartValue::of_field);
        parts.insert(PartName::Title, title);
        let status = status.map_or(ReaderPartValue::Null, |s| ReaderPartValue::Str(s.into()));
        if tree::is_tree_type(&b.block_type) {
            for name in TREE_FIELDS {
                if let Some(v) = b.fields.get(name) {
                    parts.insert(PartName::Field(name.into()), ReaderPartValue::of_field(v));
                }
            }
            return BlockParts(parts);
        }
        if b.block_type == decision::DECISION {
            let statement = b
                .fields
                .get("statement")
                .map_or(ReaderPartValue::Absent, ReaderPartValue::of_field);
            parts.insert(PartName::Statement, statement);
            parts.insert(PartName::Status, status);
            return BlockParts(parts);
        }
        let covered = text_fields(&b.block_type);
        let mut text = BTreeMap::from([("body".to_string(), ReaderPartValue::Str(b.body.clone()))]);
        for name in covered {
            if let Some(v) = b.fields.get(*name) {
                text.insert((*name).to_string(), ReaderPartValue::of_field(v));
            }
        }
        parts.insert(PartName::Text, ReaderPartValue::Map(text));
        parts.insert(PartName::Type, ReaderPartValue::Str(b.block_type.clone()));
        let links = b
            .links
            .iter()
            .map(|(kind, links)| {
                let links = links
                    .iter()
                    .map(|l| {
                        let mut link = BTreeMap::from([(
                            "target".to_string(),
                            ReaderPartValue::Str(l.target.to_string()),
                        )]);
                        if let Some(part) = &l.part {
                            link.insert("part".to_string(), ReaderPartValue::Str(part.clone()));
                        }
                        ReaderPartValue::Map(link)
                    })
                    .collect();
                (kind.clone(), ReaderPartValue::List(links))
            })
            .collect();
        parts.insert(PartName::Links, ReaderPartValue::Map(links));
        parts.insert(PartName::Status, status);
        for (name, v) in &b.fields {
            if name == "title" || covered.contains(&name.as_str()) {
                continue;
            }
            parts.insert(PartName::Field(name.clone()), ReaderPartValue::of_field(v));
        }
        BlockParts(parts)
    }

    /// The value of `part`: [`ReaderPartValue::Absent`] when the block has no such part.
    pub fn get(&self, part: &PartName) -> &ReaderPartValue {
        self.0.get(part).unwrap_or(&ABSENT)
    }

    /// Each part the block has, in [`PartName`] order.
    pub fn iter(&self) -> impl Iterator<Item = (&PartName, &ReaderPartValue)> {
        self.0.iter()
    }
}

/// The parts that differ between two views of one block, in [`PartName`] order: the seam's one
/// part comparison. A part that one side lacks compares as [`ReaderPartValue::Absent`]. Two equal
/// values have equal hashes, so a baseline view compares by hash with the same result.
pub fn changed_parts(before: &BlockParts, after: &BlockParts) -> Vec<PartName> {
    let names: BTreeSet<&PartName> = before.0.keys().chain(after.0.keys()).collect();
    names
        .into_iter()
        .filter(|name| before.get(name) != after.get(name))
        .cloned()
        .collect()
}

// ---- removals -----------------------------------------------------------------------------

/// The removals both sides of a comparison apply, combined before any reader text is read: each
/// removed block version and each removed figure blob, with the request that removed it. A
/// removal matches by version, never by id alone (D-Z8Q4N). Either removed image removes its
/// figure's reader text (D-TBDA1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewRemovals {
    pub versions: BTreeMap<Hash, Hash>,
    pub blobs: BTreeMap<Hash, Hash>,
}

impl ReviewRemovals {
    /// The removals of `sides` in the captured `source`: every removed version a side retains or
    /// holds outside a doc, and every unavailable selected figure blob that a removal masks. No
    /// blob byte is read.
    pub fn of_sides(
        source: &dyn PublicationSource,
        sides: &[&Resolved],
    ) -> Result<ReviewRemovals, RdyError> {
        let mut versions = removed_versions(sides);
        for e in sides.iter().flat_map(|r| &r.removed_outside_docs) {
            if let Some(request) = e.redaction {
                versions.entry(e.version).or_insert(request);
            }
        }
        let mut blobs = BTreeMap::new();
        for a in sides
            .iter()
            .flat_map(|r| &r.attachments)
            .filter(|a| !a.available)
        {
            if blobs.contains_key(&a.blob) {
                continue;
            }
            if let Some(request) = source.removed_blob(&a.blob)? {
                blobs.insert(a.blob, request);
            }
        }
        Ok(ReviewRemovals { versions, blobs })
    }

    /// `m` with these removals applied: each removed version, and each figure that shows a
    /// removed blob, reads as its stub.
    pub fn mask(&self, m: &Materialized) -> Materialized {
        mask_removed(m, &self.versions, &self.blobs).unwrap_or_else(|| m.clone())
    }
}

// ---- labels -------------------------------------------------------------------------------

/// What a change row calls `b` (the site's change label): the notice for removed content; else
/// the start of its text with each code span kept in backticks, when its label is that; else its
/// label. Cut to [`labels::LABEL_MAX`] visible characters, then `…`; backticks do not count.
pub fn change_label<R: Reading + ?Sized>(r: &R, b: &Block) -> String {
    if b.is_redacted() {
        return labels::plain_label(CONTENT_REMOVED);
    }
    match labels::label_coded(r, b) {
        Some(chars) => backticked(&labels::coded_label(&chars)),
        None => labels::plain_label(&labels::label(r, b, 0)),
    }
}

/// What a decision row calls decision `d` (D-9BWMV, D-0C4TB): the notice for removed content;
/// else its title; else the change label over its statement ([`change_label`]), never its id.
pub fn decision_title<R: Reading + ?Sized>(r: &R, d: &Block) -> String {
    if d.is_redacted() {
        return CONTENT_REMOVED.to_string();
    }
    match d.field_str("title").filter(|t| !t.is_empty()) {
        Some(_) => labels::label(r, d, 0),
        None => change_label(r, d),
    }
}

/// Coded characters as plain text: each code span's run between backticks.
pub(super) fn backticked(chars: &[Ch]) -> String {
    let mut out = String::new();
    for run in chars.chunk_by(|a, b| a.1 == b.1) {
        let t: String = run.iter().map(|c| c.0).collect();
        if run[0].1 == 0 {
            out.push_str(&t);
        } else {
            out.push('`');
            out.push_str(&t);
            out.push('`');
        }
    }
    out
}

/// One side's reader inputs: its masked blocks, numbers and profile words.
struct SideReading<'a> {
    m: &'a Materialized,
    numbers: &'a BTreeMap<BlockId, usize>,
    named: bool,
    dropped: &'a BTreeSet<BlockId>,
}

impl Reading for SideReading<'_> {
    fn block(&self, id: &BlockId) -> Option<&Block> {
        self.m.get(id)
    }

    fn doc_of(&self, id: &BlockId) -> Option<&Block> {
        self.m.doc_of(id)
    }

    fn tree_name(&self, id: &BlockId) -> Option<String> {
        self.m.name_of(id)
    }

    fn number(&self, b: &Block) -> usize {
        self.numbers.get(&b.id).copied().unwrap_or(1)
    }

    fn unavailable(&self) -> Option<&'static str> {
        self.named.then_some(text::UNAVAILABLE)
    }

    fn drops(&self, id: &BlockId) -> bool {
        self.named && self.m.get(id).is_none() && self.dropped.contains(id)
    }
}

// ---- the seam -----------------------------------------------------------------------------

/// The selection facts of one side: what its resolution recorded, unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionFacts {
    pub profile_id: Option<BlockId>,
    pub profile_version: Option<Hash>,
    pub audience: Audience,
    pub selection_hash: Hash,
    pub selection_identity: Hash,
    pub entries: Vec<SelectionEntry>,
    pub docs: Vec<DocEntry>,
    pub attachments: Vec<AttachmentEntry>,
    pub references: ReferenceReport,
    pub redactions: Vec<Hash>,
}

/// A doc or folder as one side shows it. `title` is its plain title, `None` only when a removal
/// removed it. `page` is its page relative to the site root, `None` when the site gives it none.
/// `position` is its place in depth-first reading order, with or without a page. `lede` is a
/// doc's lede block, as its page puts it first ([`shape::lede_of`]); `None` for a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeFacts {
    pub id: BlockId,
    pub block_type: String,
    pub parent: Option<BlockId>,
    pub title: Option<String>,
    pub page: Option<String>,
    pub position: Option<usize>,
    pub lede: Option<BlockId>,
    pub redacted: bool,
}

/// A block one side shows: its home doc, type, change label, nearest section, parent, order key,
/// and page position (its place in depth-first reading order). `redacted` is true when it reads
/// as a removed version's stub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownBlock {
    pub id: BlockId,
    pub block_type: String,
    pub home_doc: Option<BlockId>,
    pub parent: Option<BlockId>,
    pub order: String,
    pub label: String,
    pub section: Option<BlockId>,
    pub position: Option<usize>,
    pub redacted: bool,
}

/// A decision as one side shows it. `title` is its title, else the change label over its
/// statement, never its id, and the notice for a removed decision ([`decision_title`]).
/// `status` is its derived status tag. `text` is its statement as a reader reads it; `None` for
/// a removed decision or a side with no reader text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionFacts {
    pub title: String,
    pub status: Option<String>,
    pub text: Option<String>,
    pub redacted: bool,
}

/// One side of a publication review, as every row builder reads it (D-W4YAE). A reader view
/// ([`ReaderReviewSide`]) holds values and reader text. A future baseline view holds the same
/// facts from baseline metadata, part hashes in place of values, and no reader text.
pub trait PublicationReviewSide {
    /// The side's recorded selection facts.
    fn selection(&self) -> &SelectionFacts;
    /// Every selected folder and doc in depth-first reading order, siblings by their order keys,
    /// with or without a page.
    fn reading(&self) -> &[BlockId];
    /// A selected doc or folder.
    fn tree_entry(&self, id: &BlockId) -> Option<&TreeFacts>;
    /// A block the side shows.
    fn shown(&self, id: &BlockId) -> Option<&ShownBlock>;
    /// The blocks the side shows under `parent` (the top level for `None`), in sibling order:
    /// by order key, then by id.
    fn children(&self, parent: Option<&BlockId>) -> &[BlockId];
    /// The compared parts of a block the side shows.
    fn parts(&self, id: &BlockId) -> Option<&BlockParts>;
    /// A decision the side shows.
    fn decision(&self, id: &BlockId) -> Option<&DecisionFacts>;
    /// The reader text of a block the side shows, coded as [`text::own_text_coded`] codes it.
    /// `None` when the side has no text for it: a removed block, a decision, or a baseline view.
    /// It is never synthesized from a label.
    fn reader_text(&self, id: &BlockId) -> Option<&str>;
    /// The label of a selected figure, for its attachment rows.
    fn attachment_label(&self, block: &BlockId) -> Option<&str>;
}

/// A reader view of one same-store side (D-CWC91): its own profile revision's filtered
/// selection, with the comparison's removals applied before any label, part or text is read.
#[derive(Clone, Debug)]
pub struct ReaderReviewSide {
    selection: SelectionFacts,
    state: Materialized,
    reading: Vec<BlockId>,
    tree: BTreeMap<BlockId, TreeFacts>,
    shown: BTreeMap<BlockId, ShownBlock>,
    children: BTreeMap<Option<BlockId>, Vec<BlockId>>,
    parts: BTreeMap<BlockId, BlockParts>,
    decisions: BTreeMap<BlockId, DecisionFacts>,
    text: BTreeMap<BlockId, String>,
    attachment_labels: BTreeMap<BlockId, String>,
}

impl ReaderReviewSide {
    /// The reader view of `resolved` with `removals` applied. Shown status comes from the
    /// resolution and is never worked out again from the filtered state; a decision's status
    /// tag comes from its graph, as the block diff reads it.
    pub fn new(resolved: &Resolved, removals: &ReviewRemovals) -> ReaderReviewSide {
        let state = removals.mask(&resolved.m);
        let shape = Shape::build(&state);
        let numbers: BTreeMap<BlockId, usize> = state
            .iter()
            .filter(|b| NUMBERED.contains(&b.block_type.as_str()))
            .filter_map(|b| Some((b.id.clone(), shape.number(&b.id)?)))
            .collect();
        let r = SideReading {
            m: &state,
            numbers: &numbers,
            named: resolved.profile.id.is_some(),
            dropped: &resolved.references.dropped_decisions,
        };
        let positions = shape::reading_positions(&state);

        let mut tree_facts = BTreeMap::new();
        let mut shown = BTreeMap::new();
        let mut parts = BTreeMap::new();
        let mut decisions = BTreeMap::new();
        let mut texts = BTreeMap::new();
        for b in state.iter() {
            let redacted = b.is_redacted();
            if tree::is_tree_type(&b.block_type) {
                tree_facts.insert(
                    b.id.clone(),
                    TreeFacts {
                        id: b.id.clone(),
                        block_type: b.block_type.clone(),
                        parent: b.parent.clone(),
                        title: (!redacted).then(|| labels::label(&r, b, 0)),
                        page: shape.page(&b.id),
                        position: shape.position(&b.id),
                        lede: (b.block_type == tree::DOC)
                            .then(|| shape::lede_of(&state, b).map(|l| l.id.clone()))
                            .flatten(),
                        redacted,
                    },
                );
            }
            shown.insert(
                b.id.clone(),
                ShownBlock {
                    id: b.id.clone(),
                    block_type: b.block_type.clone(),
                    home_doc: state.doc_of(&b.id).map(|d| d.id.clone()),
                    parent: b.parent.clone(),
                    order: b.order.clone(),
                    label: change_label(&r, b),
                    section: shape::section_of(&state, &b.id).map(|s| s.id.clone()),
                    position: positions.get(&b.id).copied(),
                    redacted,
                },
            );
            let status = if b.block_type == decision::DECISION {
                let status = match redacted {
                    true => None,
                    false => resolved.graph.status(&b.id).map(|s| s.tag().to_string()),
                };
                let revokes = b
                    .links
                    .get(decision::REVOKES)
                    .is_some_and(|l| !l.is_empty());
                decisions.insert(
                    b.id.clone(),
                    DecisionFacts {
                        title: decision_title(&r, b),
                        status: status.clone(),
                        text: (!redacted).then(|| text::decision_statement(&r, b, revokes)),
                        redacted,
                    },
                );
                status
            } else {
                if !redacted {
                    texts.insert(b.id.clone(), text::own_text_coded(&r, b));
                }
                resolved
                    .as_built
                    .get(&b.id)
                    .filter(|s| s.status != Status::Unversioned)
                    .map(|s| {
                        Derived {
                            status: s.status,
                            since: s.since.clone(),
                        }
                        .label()
                    })
            };
            parts.insert(b.id.clone(), BlockParts::of(b, status.as_deref()));
        }
        let attachment_labels = resolved
            .attachments
            .iter()
            .filter_map(|a| {
                let b = state.get(&a.block)?;
                Some((a.block.clone(), change_label(&r, b)))
            })
            .collect();
        let children = std::iter::once(None)
            .chain(state.iter().map(|b| Some(b.id.clone())))
            .filter_map(|parent| {
                let kids: Vec<BlockId> = state
                    .children(parent.as_ref())
                    .into_iter()
                    .map(|c| c.id.clone())
                    .collect();
                (!kids.is_empty()).then_some((parent, kids))
            })
            .collect();
        let reading = shape
            .reading()
            .iter()
            .filter(|id| tree_facts.contains_key(*id))
            .cloned()
            .collect();
        ReaderReviewSide {
            selection: SelectionFacts {
                profile_id: resolved.profile.id.clone(),
                profile_version: resolved.profile.version,
                audience: resolved.profile.audience,
                selection_hash: resolved.selection_hash,
                selection_identity: resolved.selection_identity,
                entries: resolved.entries.clone(),
                docs: resolved.docs.clone(),
                attachments: resolved.attachments.clone(),
                references: resolved.references.clone(),
                redactions: resolved.redactions.clone(),
            },
            state,
            reading,
            tree: tree_facts,
            shown,
            children,
            parts,
            decisions,
            text: texts,
            attachment_labels,
        }
    }

    /// The masked selected state: two reader views classify their changes from it. A baseline
    /// view has none.
    pub fn state(&self) -> &Materialized {
        &self.state
    }
}

impl PublicationReviewSide for ReaderReviewSide {
    fn selection(&self) -> &SelectionFacts {
        &self.selection
    }

    fn reading(&self) -> &[BlockId] {
        &self.reading
    }

    fn tree_entry(&self, id: &BlockId) -> Option<&TreeFacts> {
        self.tree.get(id)
    }

    fn shown(&self, id: &BlockId) -> Option<&ShownBlock> {
        self.shown.get(id)
    }

    fn children(&self, parent: Option<&BlockId>) -> &[BlockId] {
        self.children
            .get(&parent.cloned())
            .map_or(&[], Vec::as_slice)
    }

    fn parts(&self, id: &BlockId) -> Option<&BlockParts> {
        self.parts.get(id)
    }

    fn decision(&self, id: &BlockId) -> Option<&DecisionFacts> {
        self.decisions.get(id)
    }

    fn reader_text(&self, id: &BlockId) -> Option<&str> {
        self.text.get(id).map(String::as_str)
    }

    fn attachment_label(&self, block: &BlockId) -> Option<&str> {
        self.attachment_labels.get(block).map(String::as_str)
    }
}

// ---- reference targets --------------------------------------------------------------------

/// The label lookup of reference rows into the captured candidate source, with the review's
/// removals applied first. It may name a block outside the selection, for the authorizer only:
/// no reader view, count, excerpt or search text reads it.
#[derive(Clone, Debug)]
pub struct ReferenceLookup {
    m: Materialized,
}

impl ReferenceLookup {
    /// The lookup into `source`, the full source state the candidate was resolved from.
    pub fn new(source: &Materialized, removals: &ReviewRemovals) -> ReferenceLookup {
        ReferenceLookup {
            m: removals.mask(source),
        }
    }

    /// What a reference row calls `id` ([`change_label`]); `None` for a block the source lacks.
    pub fn label(&self, id: &BlockId) -> Option<String> {
        let b = self.m.get(id)?;
        Some(change_label(self, b))
    }

    /// The title of the doc that holds `id`; `None` when it sits in no doc or a removal removed
    /// the doc's title.
    pub fn doc_title(&self, id: &BlockId) -> Option<String> {
        let d = self.m.doc_of(id)?;
        (!d.is_redacted()).then(|| labels::label(self, d, 0))
    }

    /// Whether a removal removed the title of the doc that holds `id` (D-62KA3): false when it
    /// sits in no doc.
    pub fn doc_redacted(&self, id: &BlockId) -> bool {
        self.m.doc_of(id).is_some_and(Block::is_redacted)
    }

    /// What a dropped decision row calls decision `id` ([`decision_title`]); `None` for a block
    /// the source lacks.
    pub fn decision_title(&self, id: &BlockId) -> Option<String> {
        let d = self.m.get(id)?;
        Some(decision_title(self, d))
    }

    /// The masked source state the lookup reads.
    pub(super) fn state(&self) -> &Materialized {
        &self.m
    }
}

impl Reading for ReferenceLookup {
    fn block(&self, id: &BlockId) -> Option<&Block> {
        self.m.get(id)
    }

    fn doc_of(&self, id: &BlockId) -> Option<&Block> {
        self.m.doc_of(id)
    }

    fn tree_name(&self, id: &BlockId) -> Option<String> {
        self.m.name_of(id)
    }

    /// The block's number among the blocks of its kind in its doc, in reading order.
    fn number(&self, b: &Block) -> usize {
        let Some(doc) = self.m.doc_of(&b.id) else {
            return 1;
        };
        shape::doc_blocks(&self.m, &doc.id)
            .into_iter()
            .filter(|x| x.block_type == b.block_type)
            .position(|x| x.id == b.id)
            .map_or(1, |i| i + 1)
    }

    fn unavailable(&self) -> Option<&'static str> {
        None
    }

    fn drops(&self, _: &BlockId) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canon(v: Value) -> String {
        rfc8785(&v).expect("no number")
    }

    #[test]
    fn drv1_rfc8785_sorts_keys_by_utf16_code_units() {
        // U+FF61 is one UTF-16 unit (0xFF61); U+1F600 is a surrogate pair (0xD83D 0xDE00). UTF-16
        // puts the emoji first; Rust's byte order (EF BD A1 against F0 9F 98 80) puts it last.
        let v = json!({"\u{ff61}": "a", "\u{1f600}": "b", "z": "c", "": "d", "A": "e"});
        assert_eq!(
            canon(v),
            "{\"\":\"d\",\"A\":\"e\",\"z\":\"c\",\"\u{1f600}\":\"b\",\"\u{ff61}\":\"a\"}"
        );
        let mut bytes = ["\u{ff61}", "\u{1f600}"];
        bytes.sort();
        assert_eq!(bytes, ["\u{ff61}", "\u{1f600}"], "byte order differs");
        // nested objects sort too, and arrays keep their order
        assert_eq!(
            canon(json!({"b": [{"y": true, "x": false}, null], "a": []})),
            "{\"a\":[],\"b\":[{\"x\":false,\"y\":true},null]}"
        );
    }

    #[test]
    fn drv1_rfc8785_escapes_as_json_stringify() {
        let mut s = String::new();
        for c in 0u32..0x20 {
            s.push(char::from_u32(c).unwrap());
        }
        s.push_str("\"\\/\u{7f}é\u{2028}\u{1f600}");
        let want = concat!(
            "\"\\u0000\\u0001\\u0002\\u0003\\u0004\\u0005\\u0006\\u0007",
            "\\b\\t\\n\\u000b\\f\\r\\u000e\\u000f",
            "\\u0010\\u0011\\u0012\\u0013\\u0014\\u0015\\u0016\\u0017",
            "\\u0018\\u0019\\u001a\\u001b\\u001c\\u001d\\u001e\\u001f",
            "\\\"\\\\/\u{7f}é\u{2028}\u{1f600}\""
        );
        assert_eq!(canon(Value::String(s)), want);
        // a key is escaped the same way
        assert_eq!(canon(json!({"a\nb": "\t"})), "{\"a\\nb\":\"\\t\"}");
    }

    #[test]
    fn drv1_rfc8785_refuses_a_number() {
        assert_eq!(rfc8785(&json!([1])), Err(CanonicalNumber));
        assert_eq!(rfc8785(&json!({"a": 1.5})), Err(CanonicalNumber));
    }

    #[test]
    fn drv1_tagged_integers_keep_the_full_range() {
        for (i, want) in [
            (0, "[\"int\",\"0\"]"),
            (-1, "[\"int\",\"-1\"]"),
            (9_007_199_254_740_993, "[\"int\",\"9007199254740993\"]"),
            (i64::MIN, "[\"int\",\"-9223372036854775808\"]"),
            (i64::MAX, "[\"int\",\"9223372036854775807\"]"),
        ] {
            assert_eq!(ReaderPartValue::Int(i).canonical(), want);
        }
    }
}
