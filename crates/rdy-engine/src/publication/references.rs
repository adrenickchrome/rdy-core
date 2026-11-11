//! The reference check of a selection (D-0VCQ5): every reference in a selected block whose
//! target lies outside the selection, counted per occurrence, under the audience's fixed policy.
//! The rows hold only the source's own id, location and token, never a target's text.
//!
//! The check counts what an output can show (D-44NMT). No output shows a link edge, so a link
//! to a target outside the selection is a link outside it: the report lists it apart, and it
//! never sets the status. A profiled output drops a reference to a decision outside the
//! selection (D-7M0YF), so that reference never counts either.
//!
//! The selection is the filtered view (D-WJ0SJ, D-69YCN): a block a profile hides is outside it,
//! so a reference to it is excluded, and a hidden block, which no output shows, is scanned as no
//! source. A shell is scanned as what it shows: its title, name and key.

use super::select::{Limits, PublicationWarning, limit};
use crate::error::*;
use crate::keys::Keys;
use crate::materialize::Materialized;
use rdy_model::decision::DECISION;
use rdy_model::reference::{ForeignRef, foreign_ref, foreign_token};
use rdy_model::{Block, BlockId, FieldValue, RefMode, key, ref_spans};
use rdy_schema::publication::Audience;
use std::collections::{BTreeMap, BTreeSet};

/// Where a reference outside the selection points: a block of the source outside the selection,
/// or no block of the source at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Classification {
    Excluded,
    Missing,
}

impl Classification {
    /// `excluded` or `missing`.
    pub fn as_str(self) -> &'static str {
        match self {
            Classification::Excluded => "excluded",
            Classification::Missing => "missing",
        }
    }
}

/// The check result: `blocked` for a public profile with a reference outside its selection,
/// `warning` for an org profile with one, else `clear`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckStatus {
    Clear,
    Warning,
    Blocked,
}

impl CheckStatus {
    /// `clear`, `warning` or `blocked`.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Clear => "clear",
            CheckStatus::Warning => "warning",
            CheckStatus::Blocked => "blocked",
        }
    }
}

/// The references from one location of one selected block to one target token, with one
/// classification. `location` is `body`, a JSON Pointer below `/fields`, or
/// `/links/{escaped kind}/{index}`; `target` is the token as the source block holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceRow {
    pub source: BlockId,
    pub location: String,
    pub target: String,
    pub classification: Classification,
    pub occurrences: u64,
}

/// Every reference that leaves the selection, sorted by (source, location, target,
/// classification), with the occurrence counts. `blocked_count` is the sum of both counts, and
/// only the two counts set the status.
///
/// `links_outside` lists each link edge whose target lies outside the selection, sorted the same
/// way, at its `/links/{escaped kind}/{index}` location (D-44NMT). `links_outside_count` is the
/// sum of their occurrences. They never block and never warn.
///
/// `dropped_decisions` holds, in id order, each decision outside the selection that a reference
/// names by id (D-7M0YF). That reference is no row and no count: a profiled output shows its
/// display words, or nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceReport {
    pub status: CheckStatus,
    pub excluded_count: u64,
    pub missing_count: u64,
    pub blocked_count: u64,
    pub rows: Vec<ReferenceRow>,
    pub links_outside_count: u64,
    pub links_outside: Vec<ReferenceRow>,
    pub dropped_decisions: BTreeSet<BlockId>,
}

/// The fix of `E_PUBLICATION_REFERENCE`.
const BLOCKED_FIX: &str =
    "Include the target docs or remove these references. Run the publication check again.";
/// The fix of `W_PUBLICATION_REFERENCE`.
const WARNING_FIX: &str = "Review these references before you share the output.";

impl ReferenceReport {
    /// `E_PUBLICATION_REFERENCE` when the check is blocked.
    pub fn require_output(&self) -> Result<(), RdyError> {
        match self.status {
            CheckStatus::Blocked => Err(RdyError::new(
                E_PUBLICATION_REFERENCE,
                format!(
                    "Public profile has {} references outside its selection.",
                    self.blocked_count
                ),
                BLOCKED_FIX,
            )),
            CheckStatus::Clear | CheckStatus::Warning => Ok(()),
        }
    }

    /// The `W_PUBLICATION_REFERENCE` warning when the check warns.
    pub fn warning(&self) -> Option<PublicationWarning> {
        (self.status == CheckStatus::Warning).then(|| PublicationWarning {
            code: W_PUBLICATION_REFERENCE,
            message: format!(
                "Org profile has {} references outside its selection.",
                self.blocked_count
            ),
            fix: WARNING_FIX.to_string(),
        })
    }

    /// The `W_PUBLICATION_REFERENCE` warning a live inspection shows (D-CWC91): none when the
    /// check is clear, the org warning when it warns, and for a blocked public check the refusal's
    /// own message and fix as a warning. The status stays as it is, and output still refuses a
    /// blocked check ([`ReferenceReport::require_output`]).
    pub fn live_warning(&self) -> Option<PublicationWarning> {
        match self.status {
            CheckStatus::Clear => None,
            CheckStatus::Warning => self.warning(),
            CheckStatus::Blocked => Some(PublicationWarning {
                code: W_PUBLICATION_REFERENCE,
                message: format!(
                    "Public profile has {} references outside its selection.",
                    self.blocked_count
                ),
                fix: BLOCKED_FIX.to_string(),
            }),
        }
    }
}

/// One segment of a JSON Pointer (RFC 6901): `~` as `~0`, `/` as `~1`.
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// The children of one field container being walked, in map-key order or index order.
enum Kids<'a> {
    List(std::iter::Enumerate<std::slice::Iter<'a, FieldValue>>),
    Map(std::collections::btree_map::Iter<'a, String, FieldValue>),
}

impl<'a> Iterator for Kids<'a> {
    type Item = (String, &'a FieldValue);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Kids::List(it) => it.next().map(|(i, v)| (i.to_string(), v)),
            Kids::Map(it) => it.next().map(|(k, v)| (escape(k), v)),
        }
    }
}

/// The resource bounds a scan of block text keeps, whatever the reference policy: one text value,
/// all text together, the reference occurrences, the field depth, and one reference row's
/// location and token. Each is checked before the value that would pass it is kept.
pub(super) struct Budget {
    l: Limits,
    text: usize,
    occurrences: usize,
}

impl Budget {
    pub(super) fn new(l: &Limits) -> Budget {
        Budget {
            l: *l,
            text: 0,
            occurrences: 0,
        }
    }

    /// Counts one reference or link occurrence.
    fn occurrence(&mut self) -> Result<(), RdyError> {
        self.occurrences += 1;
        if self.occurrences > self.l.references {
            return Err(limit("references", self.l.references, "occurrences"));
        }
        Ok(())
    }

    /// The bounds of one reference row: its location, then its target token.
    fn row(&self, location: &str, token: &str) -> Result<(), RdyError> {
        if location.len() > self.l.location {
            return Err(limit("reference location", self.l.location, "bytes"));
        }
        if token.len() > self.l.token {
            return Err(limit("reference token", self.l.token, "bytes"));
        }
        Ok(())
    }

    /// Counts one body or string value against the text bounds.
    fn text(&mut self, text: &str) -> Result<(), RdyError> {
        if text.len() > self.l.text_value {
            return Err(limit("text value", self.l.text_value, "bytes"));
        }
        self.text += text.len();
        if self.text > self.l.text {
            return Err(limit("text", self.l.text, "bytes"));
        }
        Ok(())
    }

    /// The bound of a field value `level` levels down (the top-level value is level 1).
    fn depth(&self, level: usize) -> Result<(), RdyError> {
        if level > self.l.field_depth {
            return Err(limit("field depth", self.l.field_depth, "levels"));
        }
        Ok(())
    }
}

/// A walk over block text: the [`Budget`] it keeps, and what it does at each reference or link
/// occurrence. [`walk_block`] and [`walk_field`] visit the text in one fixed order.
trait Visit {
    fn budget(&mut self) -> &mut Budget;

    /// One reference occurrence (`link` false) or link occurrence (`link` true) from `b` to
    /// `token`, at the location `at` gives.
    fn occur(
        &mut self,
        b: &Block,
        token: &str,
        link: bool,
        at: &dyn Fn() -> String,
    ) -> Result<(), RdyError>;
}

/// Checks one body or string value against the text bounds, then visits its references.
fn walk_text<V: Visit>(
    v: &mut V,
    b: &Block,
    text: &str,
    mode: RefMode,
    at: &dyn Fn() -> String,
) -> Result<(), RdyError> {
    v.budget().text(text)?;
    for span in ref_spans(text, mode) {
        v.occur(b, &span.token, false, at)?;
    }
    Ok(())
}

/// Every string leaf of field `name`, in map-key order and index order, without recursion.
fn walk_field<V: Visit>(
    v: &mut V,
    b: &Block,
    name: &str,
    value: &FieldValue,
) -> Result<(), RdyError> {
    // the open containers, and the pointer segments down to the current value
    let mut stack: Vec<Kids<'_>> = vec![];
    let mut path: Vec<String> = vec![];
    let mut next = Some((escape(name), value));
    loop {
        let (segment, value) = match next.take() {
            Some(item) => item,
            None => {
                let Some(top) = stack.last_mut() else {
                    break;
                };
                match top.next() {
                    Some(item) => item,
                    None => {
                        stack.pop();
                        path.pop();
                        continue;
                    }
                }
            }
        };
        // the top-level value is level 1
        v.budget().depth(stack.len() + 1)?;
        match value {
            FieldValue::Str(text) => {
                path.push(segment);
                let at = || format!("/fields/{}", path.join("/"));
                let r = walk_text(v, b, text, RefMode::Strict, &at);
                path.pop();
                r?;
            }
            FieldValue::List(items) => {
                path.push(segment);
                stack.push(Kids::List(items.iter().enumerate()));
            }
            FieldValue::Map(m) => {
                path.push(segment);
                stack.push(Kids::Map(m.iter()));
            }
            FieldValue::Null | FieldValue::Bool(_) | FieldValue::Int(_) => {}
        }
    }
    Ok(())
}

/// The body, then the fields, then the links of `b`. A removed version's stub holds no
/// reference.
fn walk_block<V: Visit>(v: &mut V, b: &Block) -> Result<(), RdyError> {
    if b.is_redacted() {
        return Ok(());
    }
    walk_text(v, b, &b.body, RefMode::for_body(&b.block_type), &|| {
        "body".to_string()
    })?;
    for (name, value) in &b.fields {
        walk_field(v, b, name, value)?;
    }
    for (kind, links) in &b.links {
        for (i, l) in links.iter().enumerate() {
            v.occur(b, l.target.as_str(), true, &|| {
                format!("/links/{}/{i}", escape(kind))
            })?;
        }
    }
    // repo links: the cited blocks a kind links to, after its local targets
    for (kind, links) in &b.foreign.links {
        let local = b.links.get(kind).map_or(0, Vec::len);
        for (i, l) in links.iter().enumerate() {
            let token = foreign_token(&l.target.repo, &l.target.id);
            v.occur(b, &token, true, &|| {
                format!("/links/{}/{}", escape(kind), local + i)
            })?;
        }
    }
    Ok(())
}

/// The state a scan classifies reference targets against: its blocks by id, and all of them.
/// A publication's source state, or a snapshot's own blocks.
pub(crate) trait Source {
    fn block(&self, id: &BlockId) -> Option<&Block>;
    fn blocks(&self) -> Box<dyn Iterator<Item = &Block> + '_>;
}

impl Source for Materialized {
    fn block(&self, id: &BlockId) -> Option<&Block> {
        self.get(id)
    }

    fn blocks(&self) -> Box<dyn Iterator<Item = &Block> + '_> {
        Box::new(self.iter())
    }
}

impl Source for BTreeMap<&BlockId, &Block> {
    fn block(&self, id: &BlockId) -> Option<&Block> {
        self.get(id).copied()
    }

    fn blocks(&self) -> Box<dyn Iterator<Item = &Block> + '_> {
        Box::new(self.values().copied())
    }
}

/// The scan of the selected blocks: its running counts and rows.
struct Scan<'a> {
    source: &'a dyn Source,
    keep: &'a BTreeSet<BlockId>,
    budget: Budget,
    /// The source's keys, built on the first key-shaped token.
    keys: Option<Keys>,
    rows: BTreeMap<(BlockId, String, String, Classification), u64>,
    links: BTreeMap<(BlockId, String, String, Classification), u64>,
    dropped: BTreeSet<BlockId>,
}

impl Scan<'_> {
    /// How the full source classifies `token`; None when the selection holds its target.
    fn classify(&mut self, token: &str) -> Option<Classification> {
        if let Ok(id) = BlockId::parse(token) {
            return self.by_id(&id);
        }
        // repo links: a cited block lives in the foreign log, outside every selection. Its
        // delivered body is never part of a publication, so its reference is excluded.
        if matches!(foreign_ref(token), Some(ForeignRef::Stored { .. })) {
            return Some(Classification::Excluded);
        }
        if key::problem(token).is_some() {
            return Some(Classification::Missing);
        }
        let source = self.source;
        let keys = self.keys.get_or_insert_with(|| Keys::of(source.blocks()));
        let holders = keys.holders(token);
        if holders.is_empty() {
            Some(Classification::Missing)
        } else if holders.iter().all(|(_, id)| self.keep.contains(id)) {
            None
        } else {
            Some(Classification::Excluded)
        }
    }

    /// The decision outside the selection that `token` names by id, if it does (D-7M0YF).
    fn outside_decision(&self, token: &str) -> Option<BlockId> {
        let id = BlockId::parse(token).ok()?;
        let b = self.source.block(&id)?;
        (b.block_type == DECISION && !self.keep.contains(&id)).then_some(id)
    }

    fn by_id(&self, id: &BlockId) -> Option<Classification> {
        if self.keep.contains(id) {
            None
        } else if self.source.block(id).is_some() {
            Some(Classification::Excluded)
        } else {
            Some(Classification::Missing)
        }
    }

    /// The fields of the chosen profile `b` that become the output's own metadata (its title
    /// and description), when `b` lies outside the selection; a selected profile's fields are
    /// already scanned as a selected block's.
    fn metadata(&mut self, b: &Block) -> Result<(), RdyError> {
        if b.is_redacted() || self.keep.contains(&b.id) {
            return Ok(());
        }
        for name in METADATA_FIELDS {
            if let Some(value) = b.fields.get(name) {
                walk_field(self, b, name, value)?;
            }
        }
        Ok(())
    }
}

impl Visit for Scan<'_> {
    fn budget(&mut self) -> &mut Budget {
        &mut self.budget
    }

    /// Counts one occurrence whose target lies outside the selection: a link is a link
    /// outside, a reference to a decision is dropped, and any other reference is a row.
    fn occur(
        &mut self,
        b: &Block,
        token: &str,
        link: bool,
        at: &dyn Fn() -> String,
    ) -> Result<(), RdyError> {
        self.budget.occurrence()?;
        let Some(class) = self.classify(token) else {
            return Ok(());
        };
        if !link
            && class == Classification::Excluded
            && let Some(id) = self.outside_decision(token)
        {
            self.dropped.insert(id);
            return Ok(());
        }
        let location = at();
        self.budget.row(&location, token)?;
        let rows = match link {
            true => &mut self.links,
            false => &mut self.rows,
        };
        *rows
            .entry((b.id.clone(), location, token.to_string(), class))
            .or_default() += 1;
        Ok(())
    }
}

/// The resource walk of [`check_resources`]: no classification and no rows.
struct Resources {
    budget: Budget,
}

impl Visit for Resources {
    fn budget(&mut self) -> &mut Budget {
        &mut self.budget
    }

    /// Counts one occurrence. Whether it is a row, and so has its location and token bounded,
    /// depends on the state that holds its target, which these blocks alone do not give.
    fn occur(
        &mut self,
        _: &Block,
        _: &str,
        _: bool,
        _: &dyn Fn() -> String,
    ) -> Result<(), RdyError> {
        self.budget.occurrence()
    }
}

/// The resource walk of [`check_resources`] kept open across blocks: a caller that reads a
/// state's blocks one by one counts each before it keeps it, so it holds no more than the
/// bounds allow.
pub(crate) struct ResourceMeter(Resources);

impl ResourceMeter {
    pub(crate) fn new(l: &Limits) -> ResourceMeter {
        ResourceMeter(Resources {
            budget: Budget::new(l),
        })
    }

    /// Counts block `b` with every block counted before it, as [`check_resources`] counts them.
    pub(crate) fn charge(&mut self, b: &Block) -> Result<(), RdyError> {
        walk_block(&mut self.0, b)
    }
}

/// The text and reference bounds of `blocks` together that need no other block, in the order a
/// scan checks them: each text value, all text, the reference occurrences and the field depth.
/// A scan counts every occurrence whatever its target, so blocks over these bounds are over
/// them in every state that holds them. A row's location and token bounds are not checked:
/// a scan applies them only to a reference whose target lies outside its selection.
pub(super) fn check_resources<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    l: &Limits,
) -> Result<(), RdyError> {
    let mut r = Resources {
        budget: Budget::new(l),
    };
    for b in blocks {
        walk_block(&mut r, b)?;
    }
    Ok(())
}

/// The fields of a chosen profile that its output shows as its own title and description.
const METADATA_FIELDS: [&str; 2] = ["description", "title"];

/// The reference check of `selected` (the selected blocks, in id order) against `source`, the
/// whole snapshot, which classifies each target and supplies nothing else. `profile` is the
/// chosen profile's block: its title and description reach the output whether or not its home
/// doc is selected, so they count once either way (None for the implicit profile). A snapshot
/// check passes the snapshot as `selected`, `source` and `keep` alike: the implicit profile.
pub(crate) fn scan<'a>(
    selected: impl IntoIterator<Item = &'a Block>,
    profile: Option<&Block>,
    source: &dyn Source,
    keep: &BTreeSet<BlockId>,
    audience: Audience,
    l: &Limits,
) -> Result<ReferenceReport, RdyError> {
    let mut s = Scan {
        source,
        keep,
        budget: Budget::new(l),
        keys: None,
        rows: BTreeMap::new(),
        links: BTreeMap::new(),
        dropped: BTreeSet::new(),
    };
    for b in selected {
        walk_block(&mut s, b)?;
    }
    if let Some(b) = profile {
        s.metadata(b)?;
    }
    let (mut excluded_count, mut missing_count) = (0u64, 0u64);
    let rows: Vec<ReferenceRow> = s
        .rows
        .into_iter()
        .map(|(k, occurrences)| {
            match k.3 {
                Classification::Excluded => excluded_count += occurrences,
                Classification::Missing => missing_count += occurrences,
            }
            row_of(k, occurrences)
        })
        .collect();
    let links_outside_count = s.links.values().sum();
    let links_outside = s.links.into_iter().map(|(k, n)| row_of(k, n)).collect();
    let blocked_count = excluded_count + missing_count;
    let status = match (blocked_count, audience) {
        (0, _) => CheckStatus::Clear,
        (_, Audience::Public) => CheckStatus::Blocked,
        (_, Audience::Org) => CheckStatus::Warning,
    };
    Ok(ReferenceReport {
        status,
        excluded_count,
        missing_count,
        blocked_count,
        rows,
        links_outside_count,
        links_outside,
        dropped_decisions: s.dropped,
    })
}

/// The row of one (source, location, target, classification) key and its occurrences.
fn row_of(
    (source, location, target, classification): (BlockId, String, String, Classification),
    occurrences: u64,
) -> ReferenceRow {
    ReferenceRow {
        source,
        location,
        target,
        classification,
        occurrences,
    }
}
