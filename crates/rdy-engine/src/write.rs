//! The write path: an agent's ops become one change on a view.
//!
//! Order keys: `after` and `before` are exact, also inside a run of siblings that share one key
//! (D-8EH6E; parallel appends under one parent leave such a tie, and M2 does not re-key at
//! landing, ruling task-2-round2). The fallback in `between` re-keys the smaller side of the run.
//! Known cost: it writes siblings the agent did not touch, so another view's edit of one of them
//! conflicts when both land. A pin on one stays current: the content hash leaves out `order`. In
//! a stable doc, an insert with no `implements` or `editorial` also gets `unjustified_change` on
//! a re-keyed sibling the agent never named.
use crate::error::*;
use crate::keys::{self, Keys};
use crate::{Delta, Materialized, Rdy, ReadState, Scope, Session, SessionKind, views};
use rdy_model::tree;
use rdy_model::{
    AltChoice, Block, BlockId, Change, ChangeLinks, FieldValue, Hash, Justification, Link, Op,
    RefMode, decision, order_between, ref_spans,
};
use rdy_schema::supporting;
use rdy_schema::{Issue, Pack, TypeClass, has_errors, skeleton_gaps, validate_block};
use rdy_store::Conflict;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    pub target: String,
    #[serde(default)]
    pub part: Option<String>,
}

/// llst1: one entry of a link kind's list: a target string or a `{target, part?}` object (its
/// keys are checked against `LINK_KEYS` before it becomes a `LinkInput`).
#[derive(Deserialize)]
#[serde(untagged)]
enum LinkEntry {
    Text(String),
    Full(BTreeMap<String, LinkScalar>),
}

/// One value in a link object. It is not a `serde_json::Value`: that turns a YAML `.nan` or
/// `.inf` into null, which would read as "no part" and drop the input (hd3 fix round 1).
#[derive(Deserialize)]
#[serde(untagged)]
enum LinkScalar {
    Text(String),
    Unit(()),
    Other(serde::de::IgnoredAny),
}

/// Ruling Q2: the keys a link object takes (a closed set).
const LINK_KEYS: [&str; 2] = ["target", "part"];

/// llst1: a link kind's value: one target string, or a list of entries.
#[derive(Deserialize)]
#[serde(untagged)]
enum LinkValue {
    One(String),
    Many(Vec<LinkEntry>),
    Bad(serde::de::IgnoredAny),
}

/// The text a malformed link kind value gets.
const LINK_FORMS: &str = "a link kind takes a target string, a list of target strings, or a list of {target, part?} objects";

/// llst1: the `deserialize_with` for link kinds: each kind is a string (one target), a list of
/// strings, or a list of `{target, part?}` objects (strings and objects may mix in a list).
pub fn link_map<'de, D>(d: D) -> Result<BTreeMap<String, Vec<LinkInput>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = BTreeMap::<String, LinkValue>::deserialize(d)?;
    raw.into_iter()
        .map(|(kind, v)| {
            let entries = match v {
                LinkValue::One(t) => vec![LinkEntry::Text(t)],
                LinkValue::Many(v) => v,
                LinkValue::Bad(_) => {
                    return Err(serde::de::Error::custom(format!("{kind}: {LINK_FORMS}")));
                }
            };
            let links = entries
                .into_iter()
                .map(|e| match e {
                    LinkEntry::Text(target) => Ok(LinkInput { target, part: None }),
                    LinkEntry::Full(o) => link_object(&kind, o),
                })
                .collect::<Result<_, _>>()?;
            Ok((kind, links))
        })
        .collect()
}

/// How the refusal of a key outside `LINK_KEYS` ends: `<kind>: unknown field "<key>" in a link`.
pub const LINK_KEY_TAIL: &str = " in a link";

/// The fix for a link object's unknown key `key` (ruling Q2): its did-you-mean, else the keys.
pub(crate) fn link_key_fix(key: &str) -> String {
    match crate::did_you_mean::nearest(key, &LINK_KEYS) {
        Some(y) => format!("did you mean \"{y}\"? rename it"),
        None => "remove it; a link takes target and part".to_string(),
    }
}

/// The unknown key a link-object refusal (see `link_object`) names, from its text.
fn link_key(msg: &str) -> Option<&str> {
    let (_, rest) = msg.split_once(": unknown field \"")?;
    rest.strip_suffix(LINK_KEY_TAIL)?.strip_suffix('"')
}

/// A link object of kind `kind` as a `LinkInput`: a key outside `LINK_KEYS` is refused (its fix
/// is `link_key_fix`, ruling Q2); a missing or non-string target is the generic `LINK_FORMS` text.
fn link_object<E: serde::de::Error>(
    kind: &str,
    mut o: BTreeMap<String, LinkScalar>,
) -> Result<LinkInput, E> {
    if let Some(k) = o.keys().find(|k| !LINK_KEYS.contains(&k.as_str())) {
        return Err(E::custom(format!(
            "{kind}: unknown field \"{k}\"{LINK_KEY_TAIL}"
        )));
    }
    let bad = || E::custom(format!("{kind}: {LINK_FORMS}"));
    let target = match o.remove("target") {
        Some(LinkScalar::Text(t)) => t,
        _ => return Err(bad()),
    };
    let part = match o.remove("part") {
        None | Some(LinkScalar::Unit(())) => None,
        Some(LinkScalar::Text(p)) => Some(p),
        Some(LinkScalar::Other(_)) => return Err(bad()),
    };
    Ok(LinkInput { target, part })
}

/// `link_map` for an optional map (an update that names no links keeps them).
pub fn link_map_opt<'de, D>(d: D) -> Result<Option<BTreeMap<String, Vec<LinkInput>>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Wrap(#[serde(deserialize_with = "link_map")] BTreeMap<String, Vec<LinkInput>>);
    Ok(Option::<Wrap>::deserialize(d)?.map(|w| w.0))
}

/// FW-HD1: an op refuses a field it does not take (the `op` tag is not one).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpInput {
    Create {
        #[serde(rename = "type")]
        block_type: String,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        after: Option<String>,
        /// first1: a sibling to put this block ahead of (the first child: make it first).
        #[serde(default)]
        before: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, serde_json::Value>,
        #[serde(default)]
        body: String,
        #[serde(default, deserialize_with = "link_map")]
        links: BTreeMap<String, Vec<LinkInput>>,
    },
    Update {
        id: String,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, serde_json::Value>,
        #[serde(default)]
        unset: Vec<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default, deserialize_with = "link_map_opt")]
        links: Option<BTreeMap<String, Vec<LinkInput>>>,
        /// rtyp1: a content block's new type, in place (same id).
        #[serde(rename = "type", default)]
        block_type: Option<String>,
        /// wcmv1: moves the block in the same op, as a `move` would.
        #[serde(default)]
        move_to: Option<MoveTo>,
    },
    Move {
        id: String,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        after: Option<String>,
        /// first1: a sibling to put this block ahead of (the first child: make it first).
        #[serde(default)]
        before: Option<String>,
    },
    Delete {
        id: String,
        #[serde(default)]
        base: Option<String>,
    },
    /// Re-keys a parent's children (omitted parent: the top level); frozen decisions keep theirs.
    Rebalance {
        #[serde(default)]
        parent: Option<String>,
    },
    /// sus3: re-pins a block's references and links to their targets' current versions.
    Repin {
        id: String,
        #[serde(default)]
        base: Option<String>,
        /// The targets to re-pin; empty: every block it references or links to.
        #[serde(default)]
        targets: Vec<String>,
        /// D-01QX5 (p-6XMAK): also records the code state the block's `reflects` globs match
        /// now, clearing its code suspect. The block must declare reflects.
        #[serde(default)]
        code: bool,
    },
    /// D-EJBPD (p-0H3JT): re-affirms a suspect live or partly superseded decision (`id`), or
    /// every such decision in the view that is suspect on one block (`because`), with a reason.
    Reaffirm {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        because: Option<String>,
        #[serde(default)]
        base: Option<String>,
        /// Limits the op to suspects on these targets; absent: every current suspect.
        #[serde(default)]
        targets: Option<Vec<String>>,
        /// 1 to 2000 characters once trimmed; an absent reason is empty, and refused.
        #[serde(default)]
        reason: String,
    },
}

/// The fix for an op that does not parse (and names no unknown field).
pub const OP_FIX: &str =
    "each op needs \"op\" (rdy_schema lists them under \"ops\"); create needs \"type\"";

/// Op `i` of a write, parsed: an unknown field is `E_BAD_INPUT` with a did-you-mean, named by its
/// path in the op (FW-HD1); an unknown key in a link object gets `link_key_fix`; any other
/// failure is `E_BAD_INPUT` with `OP_FIX`.
pub fn parse_op(i: usize, v: serde_json::Value) -> Result<OpInput, RdyError> {
    OpInput::deserialize(&v).map_err(|e| {
        let msg = e.to_string();
        if let Some(k) = link_key(&msg) {
            return RdyError::new(E_BAD_INPUT, format!("op {i}: {msg}"), link_key_fix(k));
        }
        crate::did_you_mean::unknown_field_error_in(&format!("op {i}"), &msg, &v)
            .unwrap_or_else(|| RdyError::new(E_BAD_INPUT, format!("op {i}: {msg}"), OP_FIX))
    })
}

/// A write's ops, parsed in order (see `parse_op`); the first failure is the error.
pub fn parse_ops(ops: Vec<serde_json::Value>) -> Result<Vec<OpInput>, RdyError> {
    ops.into_iter()
        .enumerate()
        .map(|(i, v)| parse_op(i, v))
        .collect()
}

/// wcmv1: where an update moves its block: the same keys as a `move` (no parent: the top level).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveTo {
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub before: Option<String>,
}

pub struct WriteRequest {
    pub view: String,
    pub ops: Vec<OpInput>,
    pub justification: Option<Justification>,
    pub message: String,
}

/// rf8 (D-4WC6R, D-XC9B9): a write's raw `reflects` justification. `sources` are selectors (an
/// id, a key, a path or a `$name` placeholder of a block the write creates), resolved in the
/// write's own overlay once every op has applied (`build_ops`); `code` records the code state of
/// each block the write creates or rewrites that declares reflects globs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReflectsInput {
    pub sources: Vec<String>,
    pub code: bool,
}

/// The most source arguments one `reflects` takes, counted before deduplication.
pub const MAX_REFLECTS_SOURCES: usize = 5000;
/// The most characters one `reflects` source selector has.
pub const MAX_REFLECTS_SELECTOR_CHARS: usize = 2000;

impl ReflectsInput {
    /// The raw checks, before the write reads anything: the source count, each selector's
    /// length, and that it names something.
    pub fn check(&self) -> Result<(), RdyError> {
        if self.sources.len() > MAX_REFLECTS_SOURCES {
            return Err(RdyError::new(
                E_LIMIT,
                format!("reflects takes at most {MAX_REFLECTS_SOURCES} sources"),
                "split the write into smaller changes",
            ));
        }
        if self
            .sources
            .iter()
            .any(|s| s.chars().count() > MAX_REFLECTS_SELECTOR_CHARS)
        {
            return Err(RdyError::new(
                E_LIMIT,
                format!(
                    "a reflects source selector takes at most {MAX_REFLECTS_SELECTOR_CHARS} characters"
                ),
                "use the source's id",
            ));
        }
        if self.sources.is_empty() && !self.code {
            return Err(bad(
                "reflects names no source and no code",
                "name a source (S-…), or pass code: true for a block with reflects globs",
            ));
        }
        Ok(())
    }
}

/// The raw `reflects` object as a boundary receives it: `{sources?, code?}`, with no other key.
/// Missing sources are none, missing code is false; an explicit false is refused ([`Self::input`]).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReflectsArg {
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    #[serde(default)]
    pub code: Option<bool>,
}

impl ReflectsArg {
    /// The input the writer takes; `code: false` is `E_BAD_INPUT`.
    pub fn input(self) -> Result<ReflectsInput, RdyError> {
        if self.code == Some(false) {
            return Err(bad(
                "reflects code must be true when given",
                "pass code: true, or omit code",
            ));
        }
        Ok(ReflectsInput {
            sources: self.sources.unwrap_or_default(),
            code: self.code.unwrap_or(false),
        })
    }
}

/// The refusal of a write given more than one justification.
pub fn one_justification_error() -> RdyError {
    bad(
        "give one of implements, editorial or reflects",
        "implements for decision-backed changes, editorial for typo, restructure or clarify, reflects for changes that follow a source",
    )
}

/// What [`parse_justification`] makes: the justification stored as given (`implements`,
/// `editorial`), or the raw `reflects` input the writer resolves in its overlay.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedJustification {
    pub stored: Option<Justification>,
    pub reflects: Option<ReflectsInput>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WriteResult {
    pub change: String,
    pub created: BTreeMap<String, String>,
    /// Names rdy assigned to created docs and folders, by block id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    /// Keys rdy assigned, by block id: to created docs and folders (FW-PR3), to docs and
    /// folders whose key followed their own move or rename (FW-PR3a), and to those whose key
    /// yielded to a new top-level name (FW-PR3b).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, String>,
    /// sup1: links rdy_supersede copied from the old decision, by kind.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub inherited: BTreeMap<String, Vec<String>>,
    /// Every change an import wrote, oldest first (one per author); empty for every other write.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
    /// p-0SH30: each decision a reaffirm op re-affirmed, with its causes; sorted by id.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reaffirmed: Vec<crate::reaffirm::Reaffirmed>,
    pub warnings: Vec<Issue>,
    /// rf8 (D-XC9B9): on a `reflects` write with `code: true`, the blocks whose code state it
    /// recorded, sorted; empty when no checkout is linked. Absent on every other write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_recorded: Option<Vec<String>>,
    /// rf8 (D-4WC6R): the normalized `reflects` justification the change stores; absent for
    /// every other justification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<crate::JustificationInfo>,
}

/// Why a JSON value is no field value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonFieldError {
    /// A number that is not an integer; the text says so.
    NotInteger(String),
    /// The value nests deeper than [`rdy_model::MAX_FIELD_NESTING`] levels (final review
    /// ruling 1).
    TooDeep,
}

impl std::fmt::Display for JsonFieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonFieldError::NotInteger(m) => f.write_str(m),
            JsonFieldError::TooDeep => f.write_str(&rdy_model::field_nesting_message()),
        }
    }
}

/// The field value of the JSON value `v`. A value deeper than [`rdy_model::MAX_FIELD_NESTING`]
/// levels is refused when its first value past the bound is reached, before anything below it
/// is built, so the recursion is bounded too.
pub fn json_to_field(v: &serde_json::Value) -> Result<FieldValue, JsonFieldError> {
    json_to_field_at(v, 1)
}

/// [`json_to_field`] of a value `level` levels down (the top-level value is level 1).
fn json_to_field_at(v: &serde_json::Value, level: usize) -> Result<FieldValue, JsonFieldError> {
    use serde_json::Value as J;
    if level > rdy_model::MAX_FIELD_NESTING {
        return Err(JsonFieldError::TooDeep);
    }
    let down = |v| json_to_field_at(v, level + 1);
    Ok(match v {
        J::Null => FieldValue::Null,
        J::Bool(b) => FieldValue::Bool(*b),
        J::Number(n) => FieldValue::Int(n.as_i64().ok_or_else(|| {
            JsonFieldError::NotInteger(format!(
                "number {n} is not an integer: floats are not supported, write it as a string"
            ))
        })?),
        J::String(s) => FieldValue::Str(s.clone()),
        J::Array(a) => FieldValue::List(a.iter().map(down).collect::<Result<_, _>>()?),
        J::Object(o) => FieldValue::Map(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), down(v)?)))
                .collect::<Result<_, JsonFieldError>>()?,
        ),
    })
}

fn bad(msg: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, msg, fix)
}

fn order_error(e: rdy_model::OrderError, parent: Option<&BlockId>) -> RdyError {
    let p = parent
        .map(|p| format!("\"{p}\""))
        .unwrap_or_else(|| "omitted for the top level".into());
    let fix = format!("rebalance the siblings first: {{\"op\":\"rebalance\",\"parent\":{p}}}");
    match e {
        rdy_model::OrderError::TooDeep => RdyError::new(E_ORDER_DEPTH, e.to_string(), fix),
        other => RdyError::new(E_BAD_INPUT, other.to_string(), fix),
    }
}

/// The children of one parent as this write sees them.
#[derive(Default)]
struct Sibs {
    order: BTreeSet<(String, BlockId)>,
    /// Lower-cased name → the docs and folders using it.
    names: BTreeMap<String, BTreeSet<BlockId>>,
}

impl Sibs {
    fn add(&mut self, b: &Block, name: Option<String>) {
        self.order.insert((b.order.clone(), b.id.clone()));
        if let Some(n) = name {
            self.names
                .entry(n.to_ascii_lowercase())
                .or_default()
                .insert(b.id.clone());
        }
    }

    fn drop(&mut self, b: &Block, name: Option<String>) {
        self.order.remove(&(b.order.clone(), b.id.clone()));
        if let Some(n) = name {
            let lower = n.to_ascii_lowercase();
            if let Some(set) = self.names.get_mut(&lower) {
                set.remove(&b.id);
                if set.is_empty() {
                    self.names.remove(&lower);
                }
            }
        }
    }

    /// A doc or folder other than `except` named `lower` (lower-cased).
    fn holder(&self, lower: &str, except: Option<&BlockId>) -> Option<&BlockId> {
        self.names.get(lower)?.iter().find(|id| Some(*id) != except)
    }
}

/// Pending edits layered over a materialized view. The children of a parent are indexed on first
/// use and kept current as edits are added, so each name check or append is a lookup.
struct Overlay<'a> {
    base: &'a Materialized,
    pending: BTreeMap<BlockId, Option<Block>>,
    /// Ids of pending (not deleted) blocks, by parent.
    pending_kids: BTreeMap<Option<BlockId>, BTreeSet<BlockId>>,
    sibs: RefCell<BTreeMap<Option<BlockId>, Sibs>>,
    /// The blocks with a recorded conflict in the view. A re-key of a tied run (`between`) does
    /// not write one this write has not changed: only an edit that names a version resolves it.
    conflicted: BTreeSet<BlockId>,
}

impl<'a> Overlay<'a> {
    fn new(base: &'a Materialized) -> Overlay<'a> {
        Overlay {
            base,
            pending: BTreeMap::new(),
            pending_kids: BTreeMap::new(),
            sibs: RefCell::default(),
            conflicted: BTreeSet::new(),
        }
    }

    fn get(&self, id: &BlockId) -> Option<&Block> {
        match self.pending.get(id) {
            Some(p) => p.as_ref(),
            None => self.base.get(id),
        }
    }

    /// The name `b` goes by: its stored name or, for a doc from M1 still under its parent in the
    /// view, the name readers give it.
    fn name(&self, b: &Block) -> Option<String> {
        if !tree::is_tree_type(&b.block_type) {
            return None;
        }
        if let Some(n) = b.field_str("name") {
            return Some(n.to_string());
        }
        self.base
            .get(&b.id)
            .filter(|old| old.parent == b.parent)
            .and_then(|_| self.base.name_of(&b.id))
    }

    fn with_sibs<R>(&self, parent: Option<&BlockId>, f: impl FnOnce(&Sibs) -> R) -> R {
        let key = parent.cloned();
        let mut all = self.sibs.borrow_mut();
        let sibs = all.entry(key.clone()).or_insert_with(|| {
            let mut s = Sibs::default();
            for b in self.base.children(parent) {
                if !self.pending.contains_key(&b.id) {
                    s.add(b, self.name(b));
                }
            }
            for id in self.pending_kids.get(&key).into_iter().flatten() {
                if let Some(Some(b)) = self.pending.get(id) {
                    s.add(b, self.name(b));
                }
            }
            s
        });
        f(sibs)
    }

    /// Records `id`'s new state (None: deleted).
    fn set(&mut self, id: BlockId, new: Option<Block>) {
        if let Some(old) = self.get(&id).cloned() {
            let name = self.name(&old);
            if let Some(s) = self.sibs.get_mut().get_mut(&old.parent) {
                s.drop(&old, name);
            }
            if let Some(set) = self.pending_kids.get_mut(&old.parent) {
                set.remove(&id);
            }
        }
        if let Some(b) = &new {
            let name = self.name(b);
            if let Some(s) = self.sibs.get_mut().get_mut(&b.parent) {
                s.add(b, name);
            }
            self.pending_kids
                .entry(b.parent.clone())
                .or_default()
                .insert(id.clone());
        }
        self.pending.insert(id, new);
    }
}

struct Resolver<'a> {
    placeholders: BTreeMap<String, BlockId>,
    base: &'a Materialized,
    /// The keys of `base`, indexed on first use: a write that neither sets a key nor names a
    /// non-id token builds no index.
    base_keys: OnceCell<Keys>,
    /// Keys held after earlier ops of this write: folded key → (key as written, holder).
    set_keys: BTreeMap<String, (String, BlockId)>,
    /// The folded key each block changed by this write holds in `set_keys`.
    set_by: BTreeMap<BlockId, String>,
    /// Blocks this write changed or deleted: their key in `base` no longer counts.
    rekeyed: BTreeSet<BlockId>,
}

impl Resolver<'_> {
    fn new(base: &Materialized) -> Resolver<'_> {
        Resolver {
            placeholders: BTreeMap::new(),
            base,
            base_keys: OnceCell::new(),
            set_keys: BTreeMap::new(),
            set_by: BTreeMap::new(),
            rekeyed: BTreeSet::new(),
        }
    }

    fn id(&self, s: &str) -> Result<BlockId, RdyError> {
        self.id_in(self.base, s)
    }

    /// `s` as [`Resolver::id`] reads it, with a path read in `paths` rather than the view before
    /// this write. rf8 (O10) passes the view as the write leaves it, so a raw `reflects` selector
    /// reads keys and paths in one state.
    fn id_in(&self, paths: &Materialized, s: &str) -> Result<BlockId, RdyError> {
        if let Some(name) = s.strip_prefix('$') {
            return self.placeholders.get(s).cloned().ok_or_else(|| {
                bad(
                    format!("placeholder ${name} is used before it is created"),
                    "create the block earlier in the same batch, or use a real id",
                )
            });
        }
        // not an id: a key (as this write has left them), else a folder or doc path, read in
        // `paths` (for an op, the view as it was before this write)
        keys::resolve_with(paths, s, &|t| self.key_target(t))
    }

    /// The one block holding key `text` (ignoring case) after the earlier ops of this write, for a
    /// reference or an id-taking field. A key two blocks hold (a floating view after another view
    /// landed the same key) is refused: the first holder must not win silently.
    fn key_target(&self, text: &str) -> Result<Option<BlockId>, RdyError> {
        let set = self
            .set_keys
            .get(&rdy_model::key::fold(text))
            .map(|(_, id)| id);
        let base = self
            .base_keys
            .get_or_init(|| Keys::of(self.base.iter()))
            .holders(text)
            .iter()
            .map(|(_, id)| id)
            .filter(|id| !self.rekeyed.contains(*id));
        keys::sole_holder(text, set.into_iter().chain(base))
    }

    /// The block holding key `text` (ignoring case) after the earlier ops of this write.
    fn key_holder(&self, text: &str) -> Option<(String, BlockId)> {
        let folded = rdy_model::key::fold(text);
        if let Some(h) = self.set_keys.get(&folded) {
            return Some(h.clone());
        }
        self.base_keys
            .get_or_init(|| Keys::of(self.base.iter()))
            .holders(text)
            .iter()
            .find(|(_, id)| !self.rekeyed.contains(id))
            .cloned()
    }

    /// Records the key `id` holds once this op applies (`None`: deleted, or no key).
    fn note_key(&mut self, id: &BlockId, now: Option<&Block>) {
        if let Some(old) = self.set_by.remove(id) {
            self.set_keys.remove(&old);
        }
        self.rekeyed.insert(id.clone());
        if let Some(k) = now.and_then(keys::key_of) {
            let folded = rdy_model::key::fold(k);
            self.set_by.insert(id.clone(), folded.clone());
            self.set_keys.insert(folded, (k.to_string(), id.clone()));
        }
    }

    /// An edit's key against the view: `was` is what the view held before the op. An edit that
    /// sets a key or ends with another key than `was` is checked, and a changed key is noted.
    /// (For a conflicted block `was` is the conflict's base, while an edit from a side starts
    /// from the side's block, whose key the view may not hold.)
    fn check_rekey(
        &mut self,
        block: &Block,
        was: Option<&Block>,
        sets_key: bool,
    ) -> Result<(), RdyError> {
        let changed = keys::key_of(block) != was.and_then(keys::key_of);
        if sets_key || changed {
            self.check_key(block)?;
        }
        if changed {
            self.note_key(&block.id, Some(block));
        }
        Ok(())
    }

    /// Refuses `block`'s key when another block already holds it (ignoring case).
    fn check_key(&self, block: &Block) -> Result<(), RdyError> {
        let Some(new) = keys::key_of(block) else {
            return Ok(());
        };
        match self.key_holder(new) {
            Some((theirs, other)) if other != block.id => Err(RdyError::new(
                E_KEY_CLASH,
                format!("key {new:?} is already on {other} (as {theirs:?})"),
                format!("give this block another key, or change {other}'s key first"),
            )),
            _ => Ok(()),
        }
    }

    /// Replaces `{{$name}}` references (as `mode` reads them) with the created ids, then
    /// `{{KEY}}` references with the ids holding those keys.
    fn text(&self, s: &str, mode: RefMode) -> Result<String, RdyError> {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for span in ref_spans(s, mode) {
            if let Some(id) = self.placeholders.get(&span.token) {
                out.push_str(&s[last..span.range.start]);
                out.push_str(&span.encode(&id.to_string()));
                last = span.range.end;
            }
        }
        out.push_str(&s[last..]);
        keys::rewrite(&out, mode, &|t| self.key_target(t))
    }

    fn value(&self, v: FieldValue) -> Result<FieldValue, RdyError> {
        Ok(match v {
            FieldValue::Str(s) => FieldValue::Str(self.text(&s, RefMode::Strict)?),
            FieldValue::List(items) => FieldValue::List(
                items
                    .into_iter()
                    .map(|i| self.value(i))
                    .collect::<Result<_, _>>()?,
            ),
            FieldValue::Map(m) => FieldValue::Map(
                m.into_iter()
                    .map(|(k, v)| Ok((k, self.value(v)?)))
                    .collect::<Result<_, RdyError>>()?,
            ),
            other => other,
        })
    }

    fn links(
        &self,
        links: BTreeMap<String, Vec<LinkInput>>,
    ) -> Result<BTreeMap<String, Vec<Link>>, RdyError> {
        links
            .into_iter()
            .map(|(k, v)| {
                Ok((
                    k,
                    v.into_iter()
                        .map(|l| {
                            Ok(Link {
                                target: self.id(&l.target)?,
                                part: l.part,
                            })
                        })
                        .collect::<Result<Vec<_>, RdyError>>()?,
                ))
            })
            .collect()
    }

    fn fields(
        &self,
        fields: BTreeMap<String, serde_json::Value>,
    ) -> Result<BTreeMap<String, FieldValue>, RdyError> {
        fields
            .into_iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    self.value(json_to_field(&v).map_err(|e| match e {
                        JsonFieldError::TooDeep => crate::error::field_nesting_limit(),
                        JsonFieldError::NotInteger(m) => {
                            bad(format!("field {k}: {m}"), "write the value as a string")
                        }
                    })?)?,
                ))
            })
            .collect()
    }
}

fn parse_base(s: &Option<String>) -> Result<Option<Hash>, RdyError> {
    s.as_ref()
        .map(|h| {
            h.parse::<Hash>().map_err(|e| {
                bad(
                    e.to_string(),
                    "pass base as the 64-character version hash from rdy_context",
                )
            })
        })
        .transpose()
}

/// Siblings that must get new keys so a block fits where it was asked: `(id, new key)`.
type Rekeys = Vec<(BlockId, String)>;

/// A sibling in order: its key and id.
type Slot = (String, BlockId);

/// The order key for a block that goes between two neighbours in (key, id) order, `left` before
/// `right` (either may be missing at the ends), and the siblings that must be re-keyed for it.
///
/// Siblings created in parallel views may share a key (D-8EH6E). A key cannot go strictly
/// between two siblings that share one, so the tied run is split: the siblings from `right` on
/// move up, or the siblings up to `left` move down. The side with fewer siblings goes first; a
/// side that holds a sibling that cannot move (a frozen decision, or a block in conflict that this
/// write has not changed) is not used, and a side with no room under the key-length cap gives way
/// to the other. The block lands directly where it was asked.
///
/// Known cost of this fallback: it writes siblings the agent did not touch (`apply_rekeys`). A
/// concurrent edit of one of them in another view then conflicts when both land (pinned by
/// `sg2_an_insert_into_a_tie_conflicts_with_a_concurrent_edit_of_the_re_keyed_sibling`). The
/// content hash leaves out `order`, so a pin on a re-keyed sibling stays current and a tension on
/// it keeps its vkey (pinned by `ok2_an_insert_into_a_tie_keeps_the_re_keyed_siblings_pin`). In a
/// stable doc, the write also edits that stable sibling, so
/// with no `implements` or `editorial` it opens `unjustified_change` on a block the agent never
/// named, and `unjustified_change: blocking` refuses the landing; the write's justification
/// covers the sibling too (pinned by
/// `sg2_an_insert_into_a_tie_in_a_stable_doc_flags_the_re_keyed_sibling_unless_justified`). A tie
/// is a state that parallel appends under one parent leave, or that an import leaves; M2 does not
/// re-key at landing (ruling task-2-round2).
fn between(
    ov: &Overlay,
    s: &Sibs,
    me: Option<&BlockId>,
    left: Option<&Slot>,
    right: Option<&Slot>,
    parent: Option<&BlockId>,
) -> Result<(String, Rekeys), RdyError> {
    let (l, r) = match (left, right) {
        (Some(l), Some(r)) if l.0 == r.0 => (l, r),
        _ => {
            let k = order_between(left.map(|l| l.0.as_str()), right.map(|r| r.0.as_str()))
                .map_err(|e| order_error(e, parent))?;
            return Ok((k, vec![]));
        }
    };
    // the siblings but the block being moved: the tied run, and the keys just outside it
    let others: Vec<&Slot> = s.order.iter().filter(|(_, id)| Some(id) != me).collect();
    let (down, up): (Vec<&BlockId>, Vec<&BlockId>) = others
        .iter()
        .filter(|(o, _)| *o == l.0)
        .map(|(_, id)| id)
        .partition(|id| **id <= l.1);
    // a sibling this write already changed takes its new key in that op (`apply_rekeys`)
    let movable = |ids: &[&BlockId]| {
        ids.iter().all(|id| {
            ov.get(id).is_some_and(|b| !decision::is_frozen(b))
                && (!ov.conflicted.contains(*id) || ov.pending.contains_key(*id))
        })
    };
    let hi = others
        .iter()
        .find(|(o, _)| *o > l.0)
        .map(|(o, _)| o.as_str());
    let lo = others
        .iter()
        .rev()
        .find(|(o, _)| *o < l.0)
        .map(|(o, _)| o.as_str());
    let split = |go_up: bool| -> Result<(String, Rekeys), rdy_model::OrderError> {
        if go_up {
            // the new block, then the siblings that were after it, above the run's key
            let mut keys = rdy_model::order_run(Some(&l.0), hi, up.len() + 1)?.into_iter();
            let new = keys.next().expect("one key more than the siblings");
            Ok((new, up.iter().map(|id| (*id).clone()).zip(keys).collect()))
        } else {
            // the siblings up to the anchor, then the new block, below the run's key
            let mut keys = rdy_model::order_run(lo, Some(&l.0), down.len() + 1)?;
            let new = keys.pop().expect("one key more than the siblings");
            Ok((new, down.iter().map(|id| (*id).clone()).zip(keys).collect()))
        }
    };
    let (can_up, can_down) = (movable(&up), movable(&down));
    let sides: Vec<bool> = if up.len() <= down.len() {
        vec![true, false]
    } else {
        vec![false, true]
    };
    let mut first_err = None;
    for go_up in sides {
        if !(if go_up { can_up } else { can_down }) {
            continue;
        }
        match split(go_up) {
            Ok(found) => return Ok(found),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(order_error(e, parent)),
        None => Err(RdyError::new(
            E_ORDER_DEPTH,
            format!(
                "cannot put a block between {} and {}: they share the order key {:?}, and frozen decisions or blocks in conflict stand on both sides of the gap",
                l.1, r.1, l.0
            ),
            "append the block, place it after a sibling with its own order key, or resolve the conflict first",
        )),
    }
}

fn order_after(
    ov: &Overlay,
    parent: Option<&BlockId>,
    after: Option<&BlockId>,
    me: Option<&BlockId>,
) -> Result<(String, Rekeys), RdyError> {
    let parent_name = parent
        .map(|p| p.to_string())
        .unwrap_or_else(|| "the document root".to_string());
    let key = after.map(|a| {
        ov.get(a)
            .filter(|b| b.parent.as_ref() == parent)
            .map(|b| (b.order.clone(), b.id.clone()))
            .ok_or_else(|| {
                bad(
                    format!("{a} is not a child of {parent_name}"),
                    "pass an `after` id that is a sibling under `parent`, or omit it to append",
                )
            })
    });
    let key = key.transpose()?;
    ov.with_sibs(parent, |s| match &key {
        None => order_between(s.order.last().map(|(o, _)| o.as_str()), None)
            .map(|k| (k, vec![]))
            .map_err(|e| order_error(e, parent)),
        Some(k) => {
            // the sibling right behind the anchor: it may share the anchor's key
            let next = s
                .order
                .range((Bound::Excluded(k), Bound::Unbounded))
                .find(|(_, id)| Some(id) != me);
            between(ov, s, me, Some(k), next, parent)
        }
    })
}

/// first1: a key just before sibling `before`. Siblings created in parallel views may share a
/// key; the new block still goes directly before `before`.
fn order_before(
    ov: &Overlay,
    parent: Option<&BlockId>,
    before: &BlockId,
    me: Option<&BlockId>,
) -> Result<(String, Rekeys), RdyError> {
    let parent_name = parent
        .map(|p| p.to_string())
        .unwrap_or_else(|| "the document root".to_string());
    let anchor = ov
        .get(before)
        .filter(|b| b.parent.as_ref() == parent)
        .map(|b| (b.order.clone(), b.id.clone()))
        .ok_or_else(|| {
            bad(
                format!("{before} is not a child of {parent_name}"),
                "pass a `before` id that is a sibling under `parent`, or omit it to append",
            )
        })?;
    ov.with_sibs(parent, |s| {
        let prev = s
            .order
            .range((Bound::Unbounded, Bound::Excluded(&anchor)))
            .rev()
            .find(|(_, id)| Some(id) != me);
        between(ov, s, me, prev, Some(&anchor), parent)
    })
}

/// The order key for a block placed after or before a sibling, or appended, and the siblings
/// that need new keys so it fits there. `me` is the block being moved, if any.
fn place(
    ov: &Overlay,
    parent: Option<&BlockId>,
    after: Option<&BlockId>,
    before: Option<&BlockId>,
    me: Option<&BlockId>,
) -> Result<(String, Rekeys), RdyError> {
    match (after, before) {
        (Some(_), Some(_)) => Err(bad(
            "give after or before, not both",
            "after puts the block behind a sibling, before ahead of one",
        )),
        (None, Some(b)) => order_before(ov, parent, b, me),
        (after, None) => order_after(ov, parent, after, me),
    }
}

/// The blocks whose only op in this write is an order re-key of `place`: block → the version
/// the view held before the write. A later op on such a block replaces the re-key op and starts
/// from the re-keyed block (`drop_rekey_op`).
type RekeyOnly = BTreeMap<BlockId, Hash>;

/// Gives the siblings `place` named their new keys.
///
/// A sibling that an earlier op of this write already changed takes its new key in that op: the
/// op's block and the overlay change, and no op is added. Any other sibling gets an update of its
/// own, which a later op on it replaces (`drop_rekey_op`); so a valid write is never refused
/// because `place` moved a sibling the write also edits, in either order.
#[allow(clippy::too_many_arguments)]
fn apply_rekeys(
    ov: &mut Overlay,
    rekeys: Rekeys,
    touched: &mut BTreeSet<BlockId>,
    content_touched: &mut BTreeSet<BlockId>,
    rekey_only: &mut RekeyOnly,
    conflicts: &ViewConflicts<'_>,
    store: &rdy_store::Store,
    ops: &mut Vec<Op>,
    prior: &mut Vec<Prior>,
) -> Result<(), RdyError> {
    for (id, k) in rekeys {
        // a re-key writes the sibling; this write counts it as changed, as a move (sus3)
        content_touched.insert(id.clone());
        if touched.contains(&id) {
            let Some(mut block) = ov.get(&id).cloned() else {
                continue;
            };
            block.order = k.clone();
            let pending = ops.iter_mut().rev().find_map(|op| match op {
                Op::Create { block } | Op::Update { block, .. } if block.id == id => Some(block),
                _ => None,
            });
            if let Some(pending) = pending {
                pending.order = k;
            }
            ov.set(id, Some(block));
            continue;
        }
        let (base_hash, cur) =
            current_for_edit(ov, &id, &None, touched, rekey_only, conflicts, store, false)?;
        let mut block: Block = cur.into_owned();
        // like a move, a doc from M1 stores the name readers already use for it
        if block.block_type == tree::DOC
            && !block.fields.contains_key("name")
            && let Some(n) = ov.base.name_of(&block.id)
        {
            block.fields.insert("name".into(), FieldValue::Str(n));
        }
        block.order = k;
        prior.push((true, block.pins.clone())); // like a move: pins unchanged
        touched.insert(block.id.clone());
        // `between` never re-keys a conflicted sibling this write has not changed
        rekey_only.insert(block.id.clone(), base_hash);
        ov.set(block.id.clone(), Some(block.clone()));
        ops.push(Op::Update {
            base: base_hash,
            block,
        });
    }
    Ok(())
}

/// A later op of this write is about to change `id`, which only an order re-key had changed so
/// far: the re-key op goes, and the later op (which starts from the re-keyed block and records the
/// version the view held) carries the new key. Call it before the op's own `prior` and `ops` push.
fn drop_rekey_op(
    id: &BlockId,
    rekey_only: &mut RekeyOnly,
    ops: &mut Vec<Op>,
    prior: &mut Vec<Prior>,
) {
    if rekey_only.remove(id).is_none() {
        return;
    }
    if let Some(at) = ops
        .iter()
        .position(|op| matches!(op, Op::Update { block, .. } if block.id == *id))
    {
        ops.remove(at);
        prior.remove(at);
    }
}

/// The conflicts of the view a write goes to, and the changes it includes: an `E_FROZEN` hint
/// names a side's change only when the view includes it (msw1).
pub(crate) struct ViewConflicts<'a> {
    pub view: &'a str,
    pub list: &'a [Conflict],
    pub included: &'a [Hash],
    /// Whether a frozen decision is one a reaffirm op could re-affirm (its `E_FROZEN` fix says
    /// so); read only when a refusal is built.
    pub reaffirmable: &'a dyn Fn(&BlockId) -> bool,
}

/// The block an update, move, delete or repin of `id` starts from, and the base its op records.
///
/// cres1: a block with a recorded conflict reads as the conflict's base, and an edit of it must
/// name the version it was made from: the conflict's base or one of its sides' versions. The
/// edit then starts from that version and, through the store, depends on every side, so it
/// resolves the conflict. A clean block keeps M1's strict check against its current version.
///
/// A frozen decision is refused, except that with `update` a clean one is returned: the update
/// path refuses it once it has built the new block, unless only tracking fields changed (trk1).
#[allow(clippy::too_many_arguments)]
fn current_for_edit<'b>(
    ov: &'b Overlay,
    id: &BlockId,
    base: &Option<String>,
    touched: &BTreeSet<BlockId>,
    rekey_only: &RekeyOnly,
    conflicts: &ViewConflicts<'_>,
    store: &rdy_store::Store,
    update: bool,
) -> Result<(Hash, Cow<'b, Block>), RdyError> {
    // a block that only an order re-key changed so far: the edit starts from the re-keyed block
    // and names the version the view held, which the agent read
    let rekeyed_from = rekey_only.get(id).copied();
    if touched.contains(id) && rekeyed_from.is_none() {
        return Err(bad(
            format!("{id} is changed more than once in this write"),
            "combine the edits into one op",
        ));
    }
    if let Some(c) = conflicts.list.iter().find(|c| c.block == *id) {
        let (b, cur) = conflicted_for_edit(ov, id, base, c, store, conflicts)?;
        check_not_frozen(id, &cur, conflicts.reaffirmable)?;
        return Ok((b, cur));
    }
    let cur = ov.get(id).ok_or_else(|| {
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no block {id} in this view"),
            "check the id with rdy_read",
        )
    })?;
    let cur_hash = rekeyed_from.unwrap_or_else(|| cur.version_hash());
    if let Some(b) = parse_base(base)?
        && b != cur_hash
    {
        return Err(RdyError::new(
            E_STALE_BASE,
            format!(
                "{id} changed since you read it: your base is {}, current is {}",
                b.short(),
                cur_hash.short()
            ),
            format!("re-read {id} (rdy_context) and retry with base={cur_hash}"),
        ));
    }
    if !update {
        check_not_frozen(id, cur, conflicts.reaffirmable)?;
    }
    Ok((cur_hash, Cow::Borrowed(cur)))
}

/// cres1: the base and starting block of an edit of `id`, which has the recorded conflict `c`.
fn conflicted_for_edit<'b>(
    ov: &'b Overlay,
    id: &BlockId,
    base: &Option<String>,
    c: &Conflict,
    store: &rdy_store::Store,
    conflicts: &ViewConflicts<'_>,
) -> Result<(Hash, Cow<'b, Block>), RdyError> {
    let cid = c.id.short();
    let sides: Vec<Hash> = c.sides.iter().filter_map(|s| s.version).collect();
    // the hint is computed only when something in the conflict is frozen
    if conflict_has_frozen(c, store)? {
        frozen_in_conflict_hinted(
            conflicts.view,
            id,
            c,
            store,
            conflicts.included,
            (conflicts.reaffirmable)(id),
        )?;
    }
    let Some(b) = parse_base(base)? else {
        let side_list = sides
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let fix = match c.base {
            Some(cb) => format!(
                "pass base={cb} (its base) or one of its sides' versions ({side_list}); rdy_context shows them"
            ),
            None => format!(
                "pass base=<one of its sides' versions> ({side_list}); rdy_context shows them"
            ),
        };
        return Err(bad(
            format!("{id} is conflicted ({cid}): name the version you edited"),
            fix,
        ));
    };
    if c.base != Some(b) && !sides.contains(&b) {
        let (its_base, retry) = match c.base {
            Some(cb) => (cb.short(), format!("base={cb} or a side's version")),
            None => ("none".to_string(), "base=<a side's version>".to_string()),
        };
        return Err(RdyError::new(
            E_STALE_BASE,
            format!(
                "{id} is conflicted ({cid}): your base {} is neither its base {its_base} nor a side",
                b.short()
            ),
            format!("re-read {id} (rdy_context lists the sides) and retry with {retry}"),
        ));
    }
    // the view reads the conflict's base, so the overlay holds it; a side's block is in the store
    match ov.get(id).filter(|cur| cur.version_hash() == b) {
        Some(cur) => Ok((b, Cow::Borrowed(cur))),
        None => Ok((b, Cow::Owned(store.get_block(&b)?))),
    }
}

/// cres1: a conflict whose base or any side is a frozen decision cannot be resolved by an edit,
/// from any version: the edit would change or drop an accepted or rejected decision. The ways out
/// are, first, to take the landed side when the conflict has one (cf1, D-X4GMH: `rdy view
/// resolve <view> <id> --landed`, `landed_side`), then to drop a side's change from the view or
/// to supersede the decision. msw1 (#1165): the hint names only the sides' changes the view can
/// exclude, the ones it includes (`included`), frozen or not. A landed change is in a view only
/// when the view is pinned (a pinned view is never rebased), and then it can be excluded too.
/// The check reads the blocks first and the landings only when something is frozen, so a plain
/// conflict never reads the landed index.
pub(crate) fn frozen_in_conflict(
    view: &str,
    id: &BlockId,
    c: &Conflict,
    store: &rdy_store::Store,
    included: &[Hash],
) -> Result<(), RdyError> {
    frozen_in_conflict_hinted(view, id, c, store, included, false)
}

/// `frozen_in_conflict` for an edit: when `reaffirmable` (a suspect accepted decision a reaffirm
/// op could re-affirm), the fix ends by naming the op, after the landed-side instruction.
fn frozen_in_conflict_hinted(
    view: &str,
    id: &BlockId,
    c: &Conflict,
    store: &rdy_store::Store,
    included: &[Hash],
    reaffirmable: bool,
) -> Result<(), RdyError> {
    // first pass: the base's and the sides' blocks decide whether anything is frozen
    let frozen_base = match c.base {
        Some(cb) if decision::is_frozen(&store.get_block(&cb)?) => Some(cb),
        _ => None,
    };
    let mut frozen_side = None;
    if frozen_base.is_none() {
        for (i, s) in c.sides.iter().enumerate() {
            if let Some(v) = s.version
                && decision::is_frozen(&store.get_block(&v)?)
            {
                frozen_side = Some(i);
                break;
            }
        }
        if frozen_side.is_none() {
            return Ok(());
        }
    }
    // second pass: the landings give the label and the changes the view can exclude
    let mut which = frozen_base
        .map(|cb| format!("its base {}", cb.short()))
        .unwrap_or_default();
    let mut excludable = vec![];
    for (i, s) in c.sides.iter().enumerate() {
        let landing = match s.change {
            Some(ch) => store.landing_of(&ch)?,
            None => None,
        };
        if frozen_side == Some(i) {
            which = match (s.change, landing) {
                (Some(ch), Some(l)) => format!("side {} landed in {}", ch.short(), l.short()),
                (Some(ch), None) => format!("side {}", ch.short()),
                (None, _) => "a side in canon".to_string(),
            };
        }
        if let Some(ch) = s.change
            && included.contains(&ch)
        {
            excludable.push(ch.to_string());
        }
    }
    let supersede = format!("write a new decision that supersedes or revokes {id}");
    let others = match excludable.as_slice() {
        [] => supersede,
        [one] => {
            format!("exclude the change {one} from the view (rdy view exclude), or {supersede}")
        }
        many => format!(
            "exclude the changes {} from the view (rdy view exclude), or {supersede}",
            many.join(", ")
        ),
    };
    let fix = match landed_side(c, store)? {
        Some(_) => {
            format!("take the landed side (rdy view resolve {view} {id} --landed), or {others}")
        }
        None => others,
    };
    Err(RdyError::new(
        E_FROZEN,
        format!(
            "{id} is an accepted or rejected decision ({which}) and cannot change; this conflict cannot be resolved by an edit"
        ),
        crate::reaffirm::frozen_fix(fix, reaffirmable),
    ))
}

/// Whether any version in conflict `c`, its base or a side, is a frozen decision.
pub(crate) fn conflict_has_frozen(
    c: &Conflict,
    store: &rdy_store::Store,
) -> Result<bool, RdyError> {
    for v in c
        .base
        .iter()
        .chain(c.sides.iter().filter_map(|s| s.version.as_ref()))
    {
        if decision::is_frozen(&store.get_block(v)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// cf1 (D-X4GMH): the landed side of conflict `c`, the version the landing froze: canon's
/// version of the block, when a side holds it and it is a frozen decision. None otherwise (canon
/// moved past every side, or its version is not frozen). Reads canon's state, so call it only
/// for a conflict that holds something frozen.
pub(crate) fn landed_side(
    c: &Conflict,
    store: &rdy_store::Store,
) -> Result<Option<Hash>, RdyError> {
    let Some(cv) = store.canon_state()?.blocks.get(&c.block).copied() else {
        return Ok(None);
    };
    if !c.sides.iter().any(|s| s.version == Some(cv)) {
        return Ok(None);
    }
    Ok(decision::is_frozen(&store.get_block(&cv)?).then_some(cv))
}

/// cf1: the landed side's block `b` against view `m`, as a write of it is checked: the view
/// reads the conflict's base, so since the landing another block may have taken `b`'s key, and
/// the view may have deleted its parent, a link target or a block it cites, or moved its parent
/// under it (cres1).
pub(crate) fn check_landed_block(pack: &Pack, m: &Materialized, b: &Block) -> Result<(), RdyError> {
    Resolver::new(m).check_key(b)?;
    let cited = b.links.values().flatten().map(|l| &l.target);
    for target in cited.chain(b.pins.keys()) {
        if *target != b.id && m.get(target).is_none() {
            return Err(RdyError::new(
                E_UNKNOWN_REF,
                format!(
                    "{} links to or cites {target}, which does not exist in this view",
                    b.id
                ),
                format!(
                    "bring {target} back into the view (exclude or undo the change that deleted it), then resolve again"
                ),
            ));
        }
    }
    // the parents above the landed parent, as the view holds them, must not reach `b` (a damaged
    // store may hold a loop of its own: never walk more steps than there are blocks)
    if let Some(p) = &b.parent {
        let mut walk = Some(p.clone());
        let mut steps = 0;
        while let Some(w) = walk {
            if w == b.id {
                return Err(bad(
                    format!(
                        "the landed side puts {} under {p}, which this view holds under {}: that would make a parent loop",
                        b.id, b.id
                    ),
                    format!(
                        "move {p} out from under {} in this view, then resolve again",
                        b.id
                    ),
                ));
            }
            steps += 1;
            if steps > m.len() {
                break;
            }
            walk = m.get(&w).and_then(|x| x.parent.clone());
        }
    }
    let issues = validate_block(pack, b, b.parent.as_ref().and_then(|p| m.get(p)));
    if let Some(e) = crate::error::nesting_refusal(&issues) {
        return Err(e);
    }
    if has_errors(&issues) {
        let first = issues
            .iter()
            .find(|i| i.severity == rdy_schema::Severity::Error)
            .expect("an error");
        let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
        e.issues = issues;
        return Err(e);
    }
    Ok(())
}

/// The refusal of a tracking edit with no editorial reason (`rdy edit` gives its own fix).
pub(crate) const TRACKING_NEEDS_REASON: &str =
    "a tracking field of an accepted or rejected decision changes only with an editorial reason";

/// trk1 (D-DZC1J): an update of a frozen decision passes only when it changes tracking fields
/// and nothing else, and then only with an editorial justification that gives a reason. Returns
/// whether `new` is such a tracking edit.
fn check_tracking_edit(
    id: &BlockId,
    cur: &Block,
    new: &Block,
    tracking: &rdy_model::Tracking,
    justification: Option<&Justification>,
    reaffirmable: &dyn Fn(&BlockId) -> bool,
) -> Result<bool, RdyError> {
    if !decision::is_frozen(cur) {
        return Ok(false);
    }
    if !decision::only_tracking_changed(cur, new, tracking) {
        check_not_frozen(id, cur, reaffirmable)?;
    }
    match justification {
        Some(Justification::Editorial { reason, .. }) if !reason.trim().is_empty() => Ok(true),
        _ => Err(bad(
            TRACKING_NEEDS_REASON,
            "pass editorial: {class: \"clarify\", reason: \"<why>\"} with the write",
        )),
    }
}

/// `E_FROZEN` for a frozen `cur`; the fix names the reaffirm op when `reaffirmable(id)`.
fn check_not_frozen(
    id: &BlockId,
    cur: &Block,
    reaffirmable: &dyn Fn(&BlockId) -> bool,
) -> Result<(), RdyError> {
    if decision::is_frozen(cur) {
        return Err(RdyError::new(
            E_FROZEN,
            format!("{id} is an accepted or rejected decision and cannot change"),
            crate::reaffirm::frozen_fix(
                format!("write a new decision that supersedes or revokes {id}"),
                reaffirmable(id),
            ),
        ));
    }
    Ok(())
}

/// What `build_ops` makes: the ops, created placeholders → ids, the blocks touched, the names
/// rdy assigned and the keys rdy assigned (both by block id), the decisions re-affirmed, the
/// blocks a repin with `code: true` names, and what a `reflects` input resolved to.
type Built = (
    Vec<Op>,
    BTreeMap<String, String>,
    BTreeSet<BlockId>,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    Vec<crate::reaffirm::Reaffirmed>,
    BTreeSet<BlockId>,
    Option<ReflectsBuilt>,
);

/// rf8: what a `reflects` write resolved in its overlay: the normalized justification the change
/// stores, and with `code: true` the blocks whose code state it records.
struct ReflectsBuilt {
    justification: Justification,
    code: BTreeSet<BlockId>,
}

/// The path of `id` in the overlay: each tree ancestor's name, itself included, joined with `/`.
/// `None` when a doc or folder on the chain has no name, a parent is missing, or the chain is
/// longer than `tree::MAX_FOLDER_DEPTH + 2` (a damaged store's parent loop cannot spin).
fn tree_path(ov: &Overlay, id: &BlockId) -> Option<String> {
    let mut parts = vec![];
    let mut b = ov.get(id)?;
    for _ in 0..tree::MAX_FOLDER_DEPTH + 2 {
        if tree::is_tree_type(&b.block_type) {
            parts.push(ov.name(b)?);
        }
        match &b.parent {
            None => {
                parts.reverse();
                return Some(parts.join("/"));
            }
            Some(p) => b = ov.get(p)?,
        }
    }
    None
}

type Prior = (bool, BTreeMap<BlockId, Hash>);

/// How many folders are on the chain from `parent` up, stopping past `tree::MAX_FOLDER_DEPTH`.
fn folders_from(ov: &Overlay, parent: Option<&BlockId>) -> usize {
    let mut n = 0;
    let mut cur = parent.and_then(|p| ov.get(p));
    for _ in 0..=ov.base.len() + ov.pending.len() {
        let Some(b) = cur else { break };
        if b.block_type == tree::FOLDER {
            n += 1;
            if n > tree::MAX_FOLDER_DEPTH {
                break;
            }
        }
        cur = b.parent.as_ref().and_then(|p| ov.get(p));
    }
    n
}

/// How many levels of folders lie below `id` in this write's view; stops past the limit.
fn folders_below(ov: &Overlay, id: &BlockId) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(id.clone(), 0usize)];
    while let Some((cur, level)) = stack.pop() {
        deepest = deepest.max(level);
        if level > tree::MAX_FOLDER_DEPTH {
            continue;
        }
        let kids: Vec<BlockId> = ov.with_sibs(Some(&cur), |s| {
            s.order.iter().map(|(_, k)| k.clone()).collect()
        });
        for k in kids {
            if ov.get(&k).is_some_and(|b| b.block_type == tree::FOLDER) {
                stack.push((k, level + 1));
            }
        }
    }
    deepest
}

/// Refuses a folder or doc that would sit (or hold folders) more than `MAX_FOLDER_DEPTH` deep.
fn check_depth(ov: &Overlay, block: &Block, below: usize) -> Result<(), RdyError> {
    if !tree::is_tree_type(&block.block_type) {
        return Ok(());
    }
    let own = usize::from(block.block_type == tree::FOLDER);
    let depth = folders_from(ov, block.parent.as_ref()) + own + below;
    if depth > tree::MAX_FOLDER_DEPTH {
        return Err(bad(
            format!(
                "{} would put folders {depth} deep (at most {} allowed)",
                block.id,
                tree::MAX_FOLDER_DEPTH
            ),
            format!(
                "keep folders at most {} deep: put it higher in the tree",
                tree::MAX_FOLDER_DEPTH
            ),
        ));
    }
    Ok(())
}

/// Checks a doc's or folder's name: valid, and unique among its siblings ignoring case.
/// `no_fields` names the op when it takes no fields (a move, or a repin resolving a conflict from
/// a side that carries another name), so the fix says to rename first.
fn check_name(
    ov: &Overlay,
    block: &Block,
    op: usize,
    no_fields: Option<&str>,
) -> Result<(), RdyError> {
    if !tree::is_tree_type(&block.block_type) {
        return Ok(());
    }
    let Some(name) = block.field_str("name") else {
        return Ok(()); // validation reports the missing field
    };
    let me = Some(&block.id);
    ov.with_sibs(block.parent.as_ref(), |sibs| {
        let taken = |lower: &str| sibs.holder(lower, me).is_some();
        if let Some(why) = tree::name_problem(name) {
            let fixed = tree::dedupe_with(&tree::default_name(Some(name), &block.id), taken);
            return Err(RdyError::new(
                E_BAD_NAME,
                format!("{name:?} is not a valid name: {why}"),
                format!("use a name like {fixed:?}"),
            )
            .with_corrected(serde_json::json!({"op": op, "fields": {"name": fixed}})));
        }
        if let Some(other) = sibs.holder(&name.to_ascii_lowercase(), me) {
            let fixed = tree::dedupe_with(name, taken);
            let err = RdyError::new(
                E_PATH_CLASH,
                format!(
                    "{} would be named {name:?}, which {other} already uses here (names are unique ignoring case)",
                    block.id
                ),
                match no_fields {
                    Some(kind) => format!("a {kind} takes no fields: rename {} first with an update (e.g. to {fixed:?}), then {kind} it", block.id),
                    None => format!("choose another name, e.g. {fixed:?}"),
                },
            );
            return Err(if no_fields.is_some() {
                err
            } else {
                err.with_corrected(serde_json::json!({"op": op, "fields": {"name": fixed}}))
            });
        }
        Ok(())
    })
}

/// blob2: an asset block's `blob` must parse as a hash naming a blob (a manifest) in the store.
/// The check keys on the type's `asset` trait, not on any field named `blob`.
fn check_asset_blob(pack: &Pack, store: &rdy_store::Store, block: &Block) -> Result<(), RdyError> {
    let is_asset = pack.is_asset(&block.block_type);
    let Some(v) = block.fields.get("blob").filter(|_| is_asset) else {
        return Ok(());
    };
    let fix = "upload the file with rdy_asset and set blob to the hash it returns";
    let Some(h) = v.as_str().and_then(|s| s.parse::<Hash>().ok()) else {
        return Err(RdyError::new(
            E_VALIDATION,
            format!(
                "{}'s blob {:?} is not a blob hash",
                block.id,
                v.as_str().unwrap_or("(not a string)")
            ),
            fix,
        ));
    };
    if !store.has_blob(&h)? {
        return Err(RdyError::new(
            E_VALIDATION,
            format!("{}'s blob {h} is not a blob in this project", block.id),
            fix,
        ));
    }
    Ok(())
}

/// dec1: only the gate writes when a decision was decided (spec §4.3), so a role that can't land
/// may not set or change `decided_at`, just as it may not set `status`.
fn check_decided_at(
    session: &Session,
    block: &Block,
    before: Option<&Block>,
) -> Result<(), RdyError> {
    if block.block_type != decision::DECISION || session.role.can_land() {
        return Ok(());
    }
    if block.fields.get("decided_at") != before.and_then(|b| b.fields.get("decided_at")) {
        return Err(RdyError::new(
            E_PERMISSION,
            format!(
                "{} sets decided_at, which only the gate writes when the decision is accepted",
                block.id
            ),
            "remove decided_at; it is set when the proposal lands",
        ));
    }
    Ok(())
}

/// The refusal of a type the pack does not define.
fn unknown_type(pack: &Pack, block_type: &str) -> RdyError {
    RdyError::new(
        E_VALIDATION,
        format!("unknown block type {block_type:?}"),
        format!(
            "use one of: {}",
            pack.block_types
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// One validation issue as the error a write returns.
fn invalid(block: &BlockId, code: &'static str, message: String, fix: String) -> RdyError {
    let mut e = RdyError::new(E_VALIDATION, message.clone(), fix.clone());
    e.issues = vec![Issue {
        severity: rdy_schema::Severity::Error,
        code,
        block: Some(block.clone()),
        message,
        fix,
    }];
    e
}

/// rtyp1: changes `block`'s type to `t` in place; the block keeps its id (FW-F5). Docs, folders
/// and supporting types keep their type, and the new type must hold every current child.
fn retype(pack: &Pack, ov: &Overlay, block: &mut Block, t: String) -> Result<(), RdyError> {
    let (id, cur) = (&block.id, block.block_type.as_str());
    if tree::is_tree_type(cur) || tree::is_tree_type(&t) {
        return Err(bad(
            format!("{id} is a {cur}: docs and folders keep their type"),
            format!("create a new {t} and move the content into it"),
        ));
    }
    let def = pack
        .block_types
        .get(&t)
        .ok_or_else(|| unknown_type(pack, &t))?;
    if pack.class_of(cur) == TypeClass::Supporting || pack.class_of(&t) == TypeClass::Supporting {
        return Err(bad(
            format!("{id} is a {cur} and cannot become a {t}: supporting types keep their type"),
            format!(
                "record a new block with the type's own tool (rdy_decide for decisions), or create a new {t}"
            ),
        ));
    }
    // the briefing rules skip a presentational block, so none becomes one or stops being one
    if pack.is_presentational(cur) != pack.is_presentational(&t) {
        return Err(bad(
            format!(
                "{id} is a {cur} and cannot become a {t}: presentational types keep their type"
            ),
            format!("create a new {t} and move the content into it"),
        ));
    }
    let kids: Vec<BlockId> = ov.with_sibs(Some(id), |s| {
        s.order.iter().map(|(_, k)| k.clone()).collect()
    });
    for k in kids {
        if let Some(child) = ov.get(&k)
            && !def.children.contains(&child.block_type)
        {
            return Err(invalid(
                id,
                "E_CHILD_TYPE",
                format!("a {t} cannot contain a {} ({k})", child.block_type),
                "move or delete that child first".into(),
            ));
        }
    }
    block.block_type = t;
    Ok(())
}

/// Places `block` (the edit of `id`) under `parent`, after or before a sibling (none: appended;
/// no parent: the top level), with a move's checks in a move's order. Returns whether its parent
/// or order key changed, and the siblings that need new keys for it to fit (see `between`).
#[allow(clippy::too_many_arguments)]
fn relocate(
    ov: &Overlay,
    res: &mut Resolver,
    block: &mut Block,
    id: &BlockId,
    parent: Option<String>,
    after: Option<String>,
    before: Option<String>,
    i: usize,
) -> Result<(bool, Rekeys), RdyError> {
    let parent = parent.as_deref().map(|p| res.id(p)).transpose()?;
    let after = after.as_deref().map(|a| res.id(a)).transpose()?;
    let before = before.as_deref().map(|b| res.id(b)).transpose()?;
    if before.as_ref() == Some(id) {
        return Err(bad(
            format!("cannot move {id} before itself"),
            "name another sibling as before, or omit it",
        ));
    }
    if let Some(p) = &parent {
        if ov.get(p).is_none() {
            return Err(RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("parent {p} does not exist"),
                "check the parent id",
            ));
        }
        // a damaged store may hold a parent loop: never walk more steps than there are blocks
        let mut walk = Some(p.clone());
        let mut steps = 0;
        while let Some(w) = walk {
            if w == *id {
                return Err(bad(
                    format!("cannot move {id} under itself or one of its descendants"),
                    "choose a parent outside the subtree being moved",
                ));
            }
            steps += 1;
            if steps > ov.base.len() + ov.pending.len() {
                return Err(bad(
                    format!("the parents above {p} loop back on themselves"),
                    format!(
                        "move {p} or one of its parents out of the loop first; rdy fsck lists it"
                    ),
                ));
            }
            walk = ov.get(&w).and_then(|b| b.parent.clone());
        }
    }
    let was = (block.parent.take(), std::mem::take(&mut block.order));
    block.parent = parent;
    let (order, rekeys) = place(
        ov,
        block.parent.as_ref(),
        after.as_ref(),
        before.as_ref(),
        Some(id),
    )?;
    block.order = order;
    check_name(ov, block, i, Some("move"))?;
    res.check_rekey(block, ov.get(id), false)?;
    if block.block_type == tree::FOLDER {
        check_depth(ov, block, folders_below(ov, id))?;
    } else {
        check_depth(ov, block, 0)?;
    }
    Ok((block.parent != was.0 || block.order != was.1, rekeys))
}

/// The fields rdy sets and a write's input never does. rkey1: `key_auto` marks a key rdy chose
/// (a create without a key, an import, `rdy keys assign`); a copied marker would let rdy
/// re-derive a key the agent chose. p-8YP1G: the engine metadata fields (`reaffirmed`,
/// `reflects_at`) are written only by a reaffirm op or a code repin, never set or unset by a
/// create or an update.
fn refuse_engine_input(
    fields: &BTreeMap<String, serde_json::Value>,
    unset: &[String],
    at: &str,
) -> Result<(), RdyError> {
    let (message, fix) = if fields.contains_key(rdy_model::key::AUTO_FIELD) {
        (
            format!(
                "{at} sets {:?}, and rdy sets it: it marks a key rdy derived from the path",
                rdy_model::key::AUTO_FIELD
            ),
            format!(
                "remove {}; set key to choose the key yourself, or leave key out and rdy derives one",
                rdy_model::key::AUTO_FIELD
            ),
        )
    } else if let Some(f) = rdy_model::engine_fields::ENGINE_FIELDS
        .iter()
        .find(|f| fields.contains_key(**f) || unset.iter().any(|u| u == *f))
    {
        (
            format!("{at} sets {f:?}, and rdy sets it"),
            format!("remove {f}; use reaffirm or repin with code: true"),
        )
    } else {
        return Ok(());
    };
    let mut e = RdyError::new(E_VALIDATION, message.clone(), fix.clone());
    e.issues = vec![Issue {
        severity: rdy_schema::Severity::Error,
        code: "E_FIELD_UNKNOWN",
        block: BlockId::parse(at).ok(),
        message,
        fix,
    }];
    Err(e)
}

/// Builds the ops of `inputs` against `base`. `rdy_keys` names the blocks whose key an update
/// in `inputs` sets on rdy's behalf (`rdy keys assign`): those keys get `key_auto`; any other
/// key an update sets is the caller's. `selection` holds what each reaffirm op selected on
/// `base` (`reaffirm::select`, run before the briefing check).
#[allow(clippy::too_many_arguments)]
fn build_ops(
    pack: &Pack,
    store: &rdy_store::Store,
    base: &Materialized,
    session: &Session,
    inputs: Vec<OpInput>,
    conflicts: &ViewConflicts<'_>,
    justification: Option<&Justification>,
    rdy_keys: &BTreeSet<BlockId>,
    selection: &crate::reaffirm::Selection,
    reflects: Option<&ReflectsInput>,
) -> Result<Built, RdyError> {
    let mut ov = Overlay::new(base);
    ov.conflicted = conflicts.list.iter().map(|c| c.block.clone()).collect();
    let mut res = Resolver::new(base);
    let mut ops = vec![];
    let mut created = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut touched = BTreeSet::new();
    // sus3 (F5): the blocks whose content this write changes; `touched` less the repin-only ones
    let mut content_touched: BTreeSet<BlockId> = BTreeSet::new();
    // D-YQCY9: the blocks this write creates or edits, which a write-level `implements` links
    let mut rewritten: BTreeSet<BlockId> = BTreeSet::new();
    let mut rekey_only = RekeyOnly::new();
    let mut prior: Vec<Prior> = vec![];
    let mut noop_rebalances: Vec<String> = vec![];
    let mut repinned: Vec<BlockId> = vec![];
    // trk1: frozen decisions this write changes only in tracking fields
    let mut tracking_edits: BTreeSet<BlockId> = BTreeSet::new();
    // D-EJBPD: decisions this write re-affirms (metadata and pins only), with their causes
    let mut reaffirmed: BTreeMap<BlockId, Vec<String>> = BTreeMap::new();
    // D-01QX5: the blocks a repin with `code: true` names
    let mut code_repins: BTreeSet<BlockId> = BTreeSet::new();
    // D-2WR0G: the field names each create or editing update supplies, for the decision basis
    // and time check (`decision_fields::validate_set`)
    let mut supplied: BTreeMap<BlockId, BTreeSet<String>> = BTreeMap::new();
    // rf8: the blocks this write creates or edits (code: true stamps those with globs), those a
    // `reflects` justification links (less the updates naming the `reflects` kind), and those
    // whose op names the `reflects` kind
    let mut written: BTreeSet<BlockId> = BTreeSet::new();
    let mut reflects_recipients: BTreeSet<BlockId> = BTreeSet::new();
    let mut explicit_reflects: BTreeSet<BlockId> = BTreeSet::new();

    for (i, input) in inputs.into_iter().enumerate() {
        match input {
            OpInput::Create {
                block_type,
                id,
                parent,
                after,
                before,
                fields,
                body,
                links,
            } => {
                let def = pack
                    .block_types
                    .get(&block_type)
                    .ok_or_else(|| unknown_type(pack, &block_type))?;
                let new_id = match id.as_deref() {
                    Some(p) if p.starts_with('$') => {
                        if res.placeholders.contains_key(p) {
                            return Err(bad(
                                format!("placeholder {p} is defined twice in this write"),
                                "use a different $name for each created block",
                            ));
                        }
                        let mut g = BlockId::new(&def.prefix);
                        while ov.get(&g).is_some() {
                            g = BlockId::new(&def.prefix);
                        }
                        res.placeholders.insert(p.to_string(), g.clone());
                        created.insert(p.to_string(), g.to_string());
                        g
                    }
                    Some(explicit) => {
                        let g = res.id(explicit)?;
                        // R-P1: an id's prefix is the type it was created with, so only a create
                        // checks it
                        if g.prefix() != def.prefix {
                            return Err(invalid(
                                &g,
                                "E_PREFIX",
                                format!(
                                    "id {g} has prefix {:?} but type {block_type} uses {:?}",
                                    g.prefix(),
                                    def.prefix
                                ),
                                format!(
                                    "omit the id to let rdy generate one, or use the prefix {:?}",
                                    def.prefix
                                ),
                            ));
                        }
                        created.insert(format!("op{i}"), g.to_string());
                        g
                    }
                    None => {
                        let mut g = BlockId::new(&def.prefix);
                        while ov.get(&g).is_some() {
                            g = BlockId::new(&def.prefix);
                        }
                        created.insert(format!("op{i}"), g.to_string());
                        g
                    }
                };
                if touched.contains(&new_id) {
                    return Err(bad(
                        format!("{new_id} is changed more than once in this write"),
                        "combine the edits into one op",
                    ));
                }
                if ov.get(&new_id).is_some() {
                    return Err(RdyError::new(
                        E_ALREADY_EXISTS,
                        format!("block {new_id} already exists"),
                        "omit id to let rdy generate one",
                    ));
                }
                // msw1 (#1160): a create/create conflict has no base, so the view reads the
                // block as absent; a create under its id would silently resolve the conflict
                if let Some(c) = conflicts.list.iter().find(|c| c.block == new_id) {
                    return Err(RdyError::new(
                        E_ALREADY_EXISTS,
                        format!(
                            "block {new_id} already exists: it is conflicted ({})",
                            c.id.short()
                        ),
                        "update it with base=<one of its sides' versions> to resolve the conflict (rdy_context shows them), or omit id to create a new block",
                    ));
                }
                let parent = parent.as_deref().map(|p| res.id(p)).transpose()?;
                if let Some(p) = &parent
                    && ov.get(p).is_none()
                {
                    return Err(RdyError::new(
                        E_UNKNOWN_BLOCK,
                        format!("parent {p} does not exist"),
                        "create the parent first or check its id",
                    ));
                }
                let after = after.as_deref().map(|a| res.id(a)).transpose()?;
                let before = before.as_deref().map(|b| res.id(b)).transpose()?;
                let (order, rekeys) =
                    place(&ov, parent.as_ref(), after.as_ref(), before.as_ref(), None)?;
                apply_rekeys(
                    &mut ov,
                    rekeys,
                    &mut touched,
                    &mut content_touched,
                    &mut rekey_only,
                    conflicts,
                    store,
                    &mut ops,
                    &mut prior,
                )?;
                let body = res.text(&body, RefMode::for_body(&block_type))?;
                refuse_engine_input(&fields, &[], &format!("op {i}"))?;
                let sets_key = fields.contains_key("key");
                supplied.insert(new_id.clone(), fields.keys().cloned().collect());
                if links.contains_key(supporting::REFLECTS_LINK) {
                    explicit_reflects.insert(new_id.clone());
                }
                let mut block = Block {
                    id: new_id.clone(),
                    block_type,
                    parent,
                    order,
                    fields: res.fields(fields)?,
                    body,
                    links: res.links(links)?,
                    pins: BTreeMap::new(),
                    read_identity: None,
                };
                if tree::is_tree_type(&block.block_type) && !block.fields.contains_key("name") {
                    let base = tree::default_name(block.field_str("title"), &block.id);
                    let name = ov.with_sibs(block.parent.as_ref(), |s| {
                        tree::dedupe_with(&base, |lower| s.holder(lower, None).is_some())
                    });
                    block
                        .fields
                        .insert("name".into(), FieldValue::Str(name.clone()));
                    names.insert(block.id.to_string(), name);
                } else {
                    check_name(&ov, &block, i, None)?;
                }
                check_decided_at(session, &block, None)?;
                check_depth(&ov, &block, 0)?;
                if sets_key {
                    res.check_key(&block)?;
                    res.note_key(&new_id, Some(&block));
                }
                touched.insert(new_id.clone());
                content_touched.insert(new_id.clone());
                rewritten.insert(new_id.clone());
                written.insert(new_id.clone());
                reflects_recipients.insert(new_id.clone());
                ov.set(new_id, Some(block.clone()));
                ops.push(Op::Create { block });
                prior.push((false, BTreeMap::new()));
            }
            OpInput::Update {
                id,
                base: b,
                fields,
                unset,
                body,
                links,
                block_type,
                move_to,
            } => {
                let id = res.id(&id)?;
                refuse_engine_input(&fields, &unset, id.as_str())?;
                let (base_hash, cur) =
                    current_for_edit(&ov, &id, &b, &touched, &rekey_only, conflicts, store, true)?;
                let cur: &Block = &cur;
                let mut block = cur.clone();
                // a doc from M1 stores the name readers already use for it on its first write
                if block.block_type == tree::DOC
                    && !block.fields.contains_key("name")
                    && let Some(n) = ov.base.name_of(&id)
                {
                    block.fields.insert("name".into(), FieldValue::Str(n));
                }
                // the content of a removed version's projection as the edit found it: an edit
                // that changes it authors a normal version (below)
                let projected = cur.read_identity.is_some().then(|| block.clone());
                let retyped = block_type.filter(|t| *t != cur.block_type);
                let edits = retyped.is_some()
                    || !fields.is_empty()
                    || !unset.is_empty()
                    || body.is_some()
                    || links.is_some();
                // an update that names `implements` sets that list exactly (link1)
                let names_implements = links
                    .as_ref()
                    .is_some_and(|l| l.contains_key(decision::IMPLEMENTS));
                // rf8: and one naming `reflects` sets that list exactly
                let names_reflects = links
                    .as_ref()
                    .is_some_and(|l| l.contains_key(supporting::REFLECTS_LINK));
                // D-2WR0G: the names this update sets, even to the value held; a type change to
                // decision checks every time field it holds
                let mut names: BTreeSet<String> = fields.keys().cloned().collect();
                if retyped.as_deref() == Some(decision::DECISION) {
                    names.extend(crate::decision_fields::TIME_FIELDS.map(String::from));
                }
                if let Some(t) = retyped {
                    retype(pack, &ov, &mut block, t)?;
                }
                let sets_key = fields.contains_key("key");
                block.fields.extend(res.fields(fields)?);
                for k in unset {
                    block.fields.remove(&k);
                }
                // K1: a key the caller writes is the caller's, even the same value, unless rdy
                // writes it (`rdy keys assign`); no key, no marker
                if sets_key && rdy_keys.contains(&id) && block.fields.contains_key("key") {
                    block
                        .fields
                        .insert(rdy_model::key::AUTO_FIELD.into(), FieldValue::Bool(true));
                } else if sets_key || block.field_str("key").is_none() {
                    block.fields.remove(rdy_model::key::AUTO_FIELD);
                }
                if let Some(body) = body {
                    block.body = res.text(&body, RefMode::for_body(&block.block_type))?;
                }
                if let Some(links) = links {
                    // link1: an update replaces only the link kinds it names; "<kind>": [] removes one
                    for (kind, targets) in res.links(links)? {
                        if targets.is_empty() {
                            block.links.remove(&kind);
                        } else {
                            block.links.insert(kind, targets);
                        }
                    }
                }
                // final review finding 3: a person's fresh type, fields, body or links over a
                // removed version's projection is new content, not the projection: it is
                // written as a normal version (no read identity, its own version hash, the
                // projection's version as its base). The frozen and schema checks below still
                // compare it with the projection (`cur`), and an edit that leaves the projected
                // content as it was keeps the read identity, so the store's and the engine's
                // input boundaries refuse it (`E_REDACTED`), as they refuse a version hash a
                // redaction removed.
                if let Some(p) = &projected
                    && (block.block_type != p.block_type
                        || block.fields != p.fields
                        || block.body != p.body
                        || block.links != p.links)
                {
                    block.read_identity = None;
                }
                let moves = move_to.is_some();
                let mut rekeys = Rekeys::new();
                if let Some(m) = move_to {
                    rekeys = relocate(
                        &ov, &mut res, &mut block, &id, m.parent, m.after, m.before, i,
                    )?
                    .1;
                }
                // after the move: a frozen decision that moves is not a tracking-only edit
                if check_tracking_edit(
                    &id,
                    cur,
                    &block,
                    store.tracking(),
                    justification,
                    conflicts.reaffirmable,
                )? {
                    tracking_edits.insert(id.clone());
                }
                check_decided_at(session, &block, Some(cur))?;
                // a move-only update keeps the pins, as a move does
                let own_prior = (moves && !edits, cur.pins.clone());
                // names and keys against what the view held, not the side an edit starts from
                let was = ov.get(&id);
                if block.field_str("name") != was.and_then(|w| w.field_str("name")) {
                    check_name(&ov, &block, i, None)?;
                }
                res.check_rekey(&block, was, sets_key)?;
                apply_rekeys(
                    &mut ov,
                    rekeys,
                    &mut touched,
                    &mut content_touched,
                    &mut rekey_only,
                    conflicts,
                    store,
                    &mut ops,
                    &mut prior,
                )?;
                // the re-keyed siblings come first, so `prior` stays in step with `ops`
                drop_rekey_op(&id, &mut rekey_only, &mut ops, &mut prior);
                prior.push(own_prior);
                touched.insert(id.clone());
                content_touched.insert(id.clone());
                if edits && !names_implements {
                    rewritten.insert(id.clone());
                }
                if edits {
                    supplied.insert(id.clone(), names);
                    written.insert(id.clone());
                    if names_reflects {
                        explicit_reflects.insert(id.clone());
                    } else {
                        reflects_recipients.insert(id.clone());
                    }
                }
                ov.set(id, Some(block.clone()));
                ops.push(Op::Update {
                    base: base_hash,
                    block,
                });
            }
            OpInput::Move {
                id,
                base: b,
                parent,
                after,
                before,
            } => {
                let id = res.id(&id)?;
                let (base_hash, cur) =
                    current_for_edit(&ov, &id, &b, &touched, &rekey_only, conflicts, store, false)?;
                let mut block = cur.into_owned();
                if block.block_type == tree::DOC
                    && !block.fields.contains_key("name")
                    && let Some(n) = ov.base.name_of(&id)
                {
                    block.fields.insert("name".into(), FieldValue::Str(n));
                }
                let (_, rekeys) =
                    relocate(&ov, &mut res, &mut block, &id, parent, after, before, i)?;
                apply_rekeys(
                    &mut ov,
                    rekeys,
                    &mut touched,
                    &mut content_touched,
                    &mut rekey_only,
                    conflicts,
                    store,
                    &mut ops,
                    &mut prior,
                )?;
                drop_rekey_op(&id, &mut rekey_only, &mut ops, &mut prior);
                prior.push((true, block.pins.clone()));
                touched.insert(id.clone());
                content_touched.insert(id.clone());
                ov.set(id, Some(block.clone()));
                ops.push(Op::Update {
                    base: base_hash,
                    block,
                });
            }
            OpInput::Delete { id, base: b } => {
                let id = res.id(&id)?;
                let (base_hash, cur) =
                    current_for_edit(&ov, &id, &b, &touched, &rekey_only, conflicts, store, false)?;
                let cur: &Block = &cur;
                if cur.block_type == decision::QUESTION {
                    return Err(bad(
                        format!(
                            "{id} is a question: questions are closed by a decision's answers link, never deleted"
                        ),
                        format!("record a decision that links answers: {id}"),
                    ));
                }
                // D-Z8P09 (TASK-507 (4)): a finding keeps its evidence; a source may go
                if cur.block_type == supporting::FINDING {
                    return Err(bad(
                        format!(
                            "{id} is a finding: findings keep their evidence and are never deleted"
                        ),
                        "update the finding, or record a new finding",
                    ));
                }
                if ov.with_sibs(Some(&id), |s| !s.order.is_empty()) {
                    return Err(RdyError::new(
                        E_HAS_CHILDREN,
                        format!("{id} still has children"),
                        "delete or move its children first",
                    ));
                }
                // the key the view held (a conflicted block's base), not the side's
                if ov.get(&id).and_then(keys::key_of).is_some() {
                    res.note_key(&id, None);
                }
                drop_rekey_op(&id, &mut rekey_only, &mut ops, &mut prior);
                touched.insert(id.clone());
                content_touched.insert(id.clone());
                ov.set(id.clone(), None);
                prior.push((false, BTreeMap::new()));
                ops.push(Op::Delete {
                    base: base_hash,
                    id,
                });
            }
            OpInput::Rebalance { parent } => {
                let parent = parent.as_deref().map(|p| res.id(p)).transpose()?;
                if let Some(p) = &parent
                    && ov.get(p).is_none()
                {
                    return Err(RdyError::new(
                        E_UNKNOWN_BLOCK,
                        format!("parent {p} does not exist"),
                        "check the parent id or path",
                    ));
                }
                // sorted by (order, id)
                let kids: Vec<Block> = ov.with_sibs(parent.as_ref(), |s| {
                    s.order
                        .iter()
                        .filter_map(|(_, id)| ov.get(id).cloned())
                        .collect()
                });
                let where_ = parent
                    .as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "the top level".into());
                let move_fix = format!(
                    "move the movable siblings under {where_} to another parent (they get fresh keys there)"
                );
                let mut items = Vec::with_capacity(kids.len());
                for b in &kids {
                    if !decision::is_frozen(b) {
                        // every movable child is re-keyed, so none may be touched by an earlier op
                        if touched.contains(&b.id) {
                            return Err(bad(
                                format!(
                                    "{} is changed earlier in this write, and rebalance re-keys it",
                                    b.id
                                ),
                                "rebalance in a write of its own",
                            ));
                        }
                        items.push(None);
                    } else if rdy_model::valid_order_key(&b.order) {
                        items.push(Some(b.order.clone()));
                    } else {
                        return Err(RdyError::new(
                            E_ORDER_DEPTH,
                            format!(
                                "{} is a frozen decision whose order key {:?} is malformed or longer than {} characters, and it cannot be re-keyed",
                                b.id,
                                b.order,
                                rdy_model::MAX_ORDER_KEY_LEN
                            ),
                            move_fix,
                        ));
                    }
                }
                // frozen anchors can leave no room for the movables between them (ties, depth)
                let keys = rdy_model::rebalance_keys(&items).map_err(|e| {
                    RdyError::new(
                        E_ORDER_DEPTH,
                        format!("cannot re-key the children of {where_} around its frozen decisions: {e}"),
                        move_fix.clone(),
                    )
                })?;
                let before = ops.len();
                for (b, k) in kids.iter().zip(keys) {
                    if decision::is_frozen(b) || b.order == k {
                        continue;
                    }
                    let (base_hash, cur) = current_for_edit(
                        &ov,
                        &b.id,
                        &None,
                        &touched,
                        &rekey_only,
                        conflicts,
                        store,
                        false,
                    )?;
                    let cur: &Block = &cur;
                    let mut block = cur.clone();
                    // like a move, a doc from M1 stores the name readers already use for it
                    if block.block_type == tree::DOC
                        && !block.fields.contains_key("name")
                        && let Some(n) = ov.base.name_of(&block.id)
                    {
                        block.fields.insert("name".into(), FieldValue::Str(n));
                    }
                    block.order = k;
                    prior.push((true, block.pins.clone())); // like a move: pins unchanged
                    touched.insert(block.id.clone());
                    content_touched.insert(block.id.clone());
                    ov.set(block.id.clone(), Some(block.clone()));
                    ops.push(Op::Update {
                        base: base_hash,
                        block,
                    });
                }
                if ops.len() == before {
                    noop_rebalances.push(where_);
                }
            }
            OpInput::Repin {
                id,
                base: b,
                targets,
                code,
            } => {
                let id = res.id(&id)?;
                let (base_hash, cur) =
                    current_for_edit(&ov, &id, &b, &touched, &rekey_only, conflicts, store, false)?;
                let cur: &Block = &cur;
                if code && crate::reflects::glob_set(cur).is_empty() {
                    return Err(bad(
                        format!("{id} has no reflects globs"),
                        "set reflects before repinning code",
                    ));
                }
                let targets_given = !targets.is_empty();
                let tracking = store.tracking();
                let selected = repin_targets(
                    cur,
                    &id,
                    targets_given.then_some(targets.as_slice()),
                    &|t| res.id(t),
                    &|t| ov.get(t).map(|x| x.content_hash_with(tracking)),
                )?;
                let wanted: Vec<BlockId> = selected.iter().map(|(t, _)| t.clone()).collect();
                let mut block = cur.clone();
                block.pins.extend(selected);
                // like a no-op rebalance, a repin that changes no pin is refused, per op; a code
                // repin records the code state, which is a change of its own (p-6XMAK)
                if block.pins == cur.pins && !code {
                    let what = if targets_given {
                        wanted
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else {
                        "everything it cites".to_string()
                    };
                    return Err(bad(
                        format!(
                            "{id} already pins the current version of {what}; nothing to repin"
                        ),
                        "a superseded or revoked suspect clears only when the citation changes, not by repinning; rdy_context lists the block's suspects",
                    ));
                }
                // a repin from a conflict's side carries the side's name and key
                let was = ov.get(&id);
                if block.field_str("name") != was.and_then(|w| w.field_str("name")) {
                    check_name(&ov, &block, i, Some("repin"))?;
                }
                res.check_rekey(&block, was, false)?;
                // a repin changes only pins, which the content hash leaves out: not in
                // `content_touched` (p-WYFR3). But a repin of a conflicted block from a side's
                // version starts from the side's block, while the view reads the conflict's base:
                // it writes the side's content, so that block's content changes (sus3)
                if was.is_none_or(|w| {
                    w.content_hash_with(tracking) != cur.content_hash_with(tracking)
                }) {
                    content_touched.insert(id.clone());
                }
                repinned.extend(wanted);
                if code {
                    code_repins.insert(id.clone());
                }
                drop_rekey_op(&id, &mut rekey_only, &mut ops, &mut prior);
                prior.push((true, block.pins.clone())); // pins set here; the second pass keeps them
                touched.insert(id.clone());
                ov.set(id, Some(block.clone()));
                ops.push(Op::Update {
                    base: base_hash,
                    block,
                });
            }
            OpInput::Reaffirm { id, base: b, .. } => {
                let (Some(sel), Some(g)) = (selection.ops.get(&i), selection.graph.as_deref())
                else {
                    return Err(RdyError::new(
                        E_INTERNAL,
                        format!("op {i}: a reaffirm op was not selected before the write"),
                        "report this as a bug in rdy",
                    ));
                };
                // only `id` takes a base (`because` with one is refused by the selection)
                let b = if id.is_some() { b } else { None };
                for s in &sel.decisions {
                    // the strict base, stale-base and conflict checks of every edit; a clean
                    // frozen decision passes here, as for an update (the dedicated path)
                    let (base_hash, cur) = current_for_edit(
                        &ov,
                        &s.id,
                        &b,
                        &touched,
                        &rekey_only,
                        conflicts,
                        store,
                        true,
                    )?;
                    let tracking = store.tracking();
                    let block =
                        crate::reaffirm::reaffirmed_block(g, &cur, &s.causes, &sel.reason, &|t| {
                            ov.get(t).map(|x| x.content_hash_with(tracking))
                        })?;
                    // like a repin: re-affirming against a target this write changes is refused
                    // below, and the content hash leaves the metadata and pins out
                    repinned.extend(s.causes.iter().cloned());
                    reaffirmed.insert(
                        s.id.clone(),
                        s.causes.iter().map(ToString::to_string).collect(),
                    );
                    drop_rekey_op(&s.id, &mut rekey_only, &mut ops, &mut prior);
                    prior.push((true, block.pins.clone())); // the second pass keeps these pins
                    touched.insert(s.id.clone());
                    ov.set(s.id.clone(), Some(block.clone()));
                    ops.push(Op::Update {
                        base: base_hash,
                        block,
                    });
                }
            }
        }
    }
    // akey1 (FW-PR3): a doc or folder created without a key gets one from its path, once every
    // op has applied, so the keys of the view, of earlier ops and every explicit key anywhere in
    // the batch are taken, and so is the name of every other top-level doc or folder (a key that
    // is another block's path would make that path unreachable); it runs before the second
    // pass, so pins and validation see the key
    let mut assigned = BTreeMap::new();
    // kfol1 runs first (the yield to new top-level names, then the renamed and moved blocks), so
    // a key a renamed block leaves or a yielded key frees is free for a block created here
    let followed = follow_keys(
        &mut ov,
        &mut res,
        &mut ops,
        &mut prior,
        &touched,
        conflicts.list,
        &mut assigned,
    );
    touched.extend(followed.iter().cloned());
    // a re-key changes the block's key, so its content: every block this pass re-keyed, a
    // repinned one included (`assigned` holds only this pass's keys so far)
    content_touched.extend(assigned.keys().filter_map(|id| BlockId::parse(id).ok()));
    for op in &mut ops {
        let Op::Create { block } = op else { continue };
        if !tree::is_tree_type(&block.block_type) || block.fields.contains_key("key") {
            continue;
        }
        // a chain past the folder depth limit gets no key: the depth check reports the damage
        let Some(path) = tree_path(&ov, &block.id) else {
            continue;
        };
        let k = keys::auto_key(&path, &block.block_type, &|c| {
            res.key_holder(c).is_some()
                || ov.with_sibs(None, |s| {
                    s.holder(&rdy_model::key::fold(c), Some(&block.id))
                        .is_some()
                })
        });
        block
            .fields
            .insert("key".into(), FieldValue::Str(k.clone()));
        block
            .fields
            .insert(rdy_model::key::AUTO_FIELD.into(), FieldValue::Bool(true));
        res.note_key(&block.id, Some(block));
        ov.set(block.id.clone(), Some(block.clone()));
        assigned.insert(block.id.to_string(), k);
    }

    // only rebalances that changed nothing: refuse rather than commit an empty change
    if ops.is_empty() && !noop_rebalances.is_empty() {
        return Err(bad(
            format!("nothing to rebalance under {}", noop_rebalances.join(", ")),
            "the children's keys are already short and in order; no write is needed",
        ));
    }

    // sus3: a repin to a block whose content this write also changes would pin a version that
    // never lands; a block the write only repins keeps its content (F5)
    if let Some(t) = repinned.iter().find(|t| content_touched.contains(*t)) {
        return Err(bad(
            format!("{t} changes in this write, so a repin to it would pin an old version"),
            "repin in a separate write, after the write that changes the target",
        ));
    }

    // D-YQCY9: a write-level `implements` links the content blocks it creates or edits
    implements_links(pack, &mut ov, &mut ops, &rewritten, justification);
    // rf8 (O10): a `reflects` input resolves in the final overlay, before the second pass. An
    // explicit `reflects` link names a held source on every write, whatever its justification.
    let reflects = match reflects {
        Some(raw) => Some(reflects_links(
            pack,
            &mut ov,
            &res,
            &mut ops,
            raw,
            &ReflectsSets {
                written: &written,
                recipients: &reflects_recipients,
                explicit: &explicit_reflects,
            },
        )?),
        None => {
            explicit_reflects_held(&ov, &ops, &explicit_reflects)?;
            None
        }
    };

    // second pass: references, decision permissions, schema validation, pins. `ov` holds every
    // block of this write at its final content (content does not depend on pins), so a target
    // in this write, a reference cycle included, is pinned to its final content here (lpin1)
    let mut issues = vec![];
    for (op, (is_move, old_pins)) in ops.iter_mut().zip(prior) {
        let block = match op {
            Op::Create { block } | Op::Update { block, .. } => block,
            Op::Delete { .. } => continue,
        };
        let mut targets = vec![];
        // p-QMGB4 (task-5 R1): an accepted decision's citation of a deleted target it records as
        // `deleted` is kept, not dangling; it keeps the pin it holds
        let kept = |target: &BlockId, targets: &mut Vec<(BlockId, Hash)>| {
            let keep = ov.get(target).is_none() && rdy_graph::keeps_deleted_citation(block, target);
            if let Some(pin) = old_pins.get(target).filter(|_| keep) {
                targets.push((target.clone(), *pin));
            }
            keep
        };
        for tok in block.ref_tokens() {
            if let Some(name) = tok.strip_prefix('$') {
                return Err(bad(
                    format!("placeholder ${name} is used before it is created"),
                    "create the block earlier in the same batch, or use a real id",
                ));
            }
            let target = BlockId::parse(&tok).map_err(|_| {
                RdyError::new(
                    E_UNKNOWN_REF,
                    format!(
                        "{{{{{tok}}}}} in {} is neither a block id nor a key in this view",
                        block.id
                    ),
                    format!(
                        "reference blocks as {{{{id}}}} or {{{{KEY}}}}, e.g. {{{{t-MEDGR}}}}; find ids and keys with rdy_read; {}",
                        crate::keys::literal_hint(&tok)
                    ),
                )
            })?;
            if kept(&target, &mut targets) {
                continue;
            }
            let t = ov.get(&target).ok_or_else(|| {
                RdyError::new(
                    E_UNKNOWN_REF,
                    format!(
                        "{} references {{{{{target}}}}}, which does not exist in this view",
                        block.id
                    ),
                    format!(
                        "fix the id, or create the target in the same write; {}",
                        crate::keys::literal_hint(&tok)
                    ),
                )
            })?;
            targets.push((target, t.content_hash_with(store.tracking())));
        }
        for link in block.links.values().flatten() {
            if kept(&link.target, &mut targets) {
                continue;
            }
            let t = ov.get(&link.target).ok_or_else(|| {
                RdyError::new(
                    E_UNKNOWN_REF,
                    format!(
                        "{} links to {}, which does not exist in this view",
                        block.id, link.target
                    ),
                    "fix the link target id",
                )
            })?;
            targets.push((link.target.clone(), t.content_hash_with(store.tracking())));
        }
        // a tracking edit keeps the status the decision already has (trk1), and so does a
        // re-affirmation (D-EJBPD)
        if block.block_type == decision::DECISION
            && block.field_str("status") != Some("proposed")
            && !session.role.can_land()
            && !tracking_edits.contains(&block.id)
            && !reaffirmed.contains_key(&block.id)
        {
            return Err(RdyError::new(
                E_PERMISSION,
                format!(
                    "{} sets decision status {:?}",
                    block.id,
                    block.field_str("status").unwrap_or("")
                ),
                "write status: proposed; the decision is accepted when its proposal lands",
            ));
        }
        let parent = block.parent.as_ref().and_then(|p| ov.get(p));
        let mut found = validate_block(pack, block, parent);
        name_unknown_evidence_key(block, &mut found);
        if let Some(names) = supplied.get(&block.id) {
            crate::decision_fields::merge_set(block, names, &mut found);
        }
        issues.extend(found);
        if !is_move {
            check_asset_blob(pack, store, block)?;
            // a pin the block already holds is kept; every other cited target is pinned now.
            // A block is never pinned to itself: its own edits would make it its own suspect
            let mut pins = BTreeMap::new();
            for (target, current) in targets.into_iter().filter(|(t, _)| *t != block.id) {
                let h = old_pins.get(&target).copied().unwrap_or(current);
                pins.insert(target, h);
            }
            // msw7: a tracking edit of a frozen decision keeps the set of targets it pins; a new
            // or dropped {{id}} in a tracking field would repin it, which the ruling forbids
            if tracking_edits.contains(&block.id) && !pins.keys().eq(old_pins.keys()) {
                let id = &block.id;
                return Err(RdyError::new(
                    E_FROZEN,
                    format!(
                        "{id}: a tracking edit cannot add or drop a reference of an accepted or rejected decision"
                    ),
                    format!(
                        "keep its references as they are; write a new decision that supersedes {id} to change what it cites"
                    ),
                ));
            }
            block.pins = pins;
        }
    }
    if let Some(e) = crate::error::nesting_refusal(&issues) {
        return Err(e);
    }
    if has_errors(&issues) {
        let first = issues
            .iter()
            .find(|i| i.severity == rdy_schema::Severity::Error)
            .expect("an error");
        let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
        e.issues = issues;
        return Err(e);
    }
    // a block whose only change is a key that yielded to a top-level name is not one the caller
    // edited
    let touched: BTreeSet<BlockId> = touched.difference(&followed).cloned().collect();
    let reaffirmed = reaffirmed
        .into_iter()
        .map(|(id, causes)| crate::reaffirm::Reaffirmed {
            id: id.to_string(),
            causes,
        })
        .collect();
    Ok((
        ops,
        created,
        touched,
        names,
        assigned,
        reaffirmed,
        code_repins,
        reflects,
    ))
}

/// D-Z8P09: gives each create of a finding or a source in `ops` that names no parent the
/// decision home of `view` (`Rdy::decision_parent`), with its refusals; an explicit parent is
/// kept. The home is looked up once, and only when such a create exists.
fn home_supporting_parents(r: &Rdy, view: &str, ops: &mut [OpInput]) -> Result<(), RdyError> {
    let mut home: Option<String> = None;
    for op in ops {
        if let OpInput::Create {
            block_type, parent, ..
        } = op
            && parent.is_none()
            && (block_type == supporting::FINDING || block_type == supporting::SOURCE)
        {
            if home.is_none() {
                home = Some(r.decision_parent(view, None)?);
            }
            *parent = home.clone();
        }
    }
    Ok(())
}

/// The keys an unknown evidence key is matched against for a did-you-mean, in order.
const EVIDENCE_HINT_KEYS: [&str; 4] = ["quote", "session_ref", "question", "source"];

/// TASK-507 (4): when `block` is a finding whose evidence has an unknown key, the schema's one
/// evidence issue in `issues` names it instead: `evidence entry <n>: unknown key "<key>"` (`n`
/// from 0, as ops are numbered), and `; did you mean "<known>"?` when one is near. The fix stays
/// the evidence fix. The shape is checked once, by the schema.
pub(crate) fn name_unknown_evidence_key(block: &Block, issues: &mut [Issue]) {
    if block.block_type != supporting::FINDING {
        return;
    }
    let Some((n, key)) = block
        .fields
        .get(supporting::EVIDENCE_FIELD)
        .and_then(supporting::first_unknown_evidence_key)
    else {
        return;
    };
    let mut message = format!("evidence entry {n}: unknown key \"{key}\"");
    if let Some(known) = crate::did_you_mean::nearest(key, &EVIDENCE_HINT_KEYS) {
        message.push_str(&format!("; did you mean \"{known}\"?"));
    }
    for i in issues.iter_mut() {
        if i.code == "E_FIELD_KIND" && i.message == supporting::EVIDENCE_MESSAGE {
            i.message = message.clone();
        }
    }
}

/// The ruling on D-YQCY9's task: `implements` names the decision a change carries out, which
/// the write's view holds. Checked once, before any op applies, for every write that carries
/// the justification (a write that only moves or repins included): a block of another type is
/// refused, and so is an id the view does not hold (one decided in another view and not
/// landed, or a typo). The ruling after G1: a held decision must be accepted (and in force) or
/// proposed in this view, so a rejected, superseded or revoked one is refused too; the graph is
/// read only for an accepted decision.
fn check_implements(
    st: &ReadState,
    scope: &Scope,
    m: &Materialized,
    j: Option<&Justification>,
) -> Result<(), RdyError> {
    let Some(Justification::Implements(ids)) = j else {
        return Ok(());
    };
    for id in ids {
        match m.get(id) {
            Some(b) if b.block_type == decision::DECISION => {
                let status = match decision::status(b) {
                    Some(decision::DecisionStatus::Rejected) => Some("rejected"),
                    Some(decision::DecisionStatus::Accepted) => match st.graph(scope)?.status(id) {
                        Some(
                            s @ (rdy_graph::DerivedStatus::Superseded { .. }
                            | rdy_graph::DerivedStatus::Revoked { .. }),
                        ) => Some(s.tag()),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(status) = status {
                    return Err(bad(
                        format!(
                            "implements names {id}, which is {status}; implements names an accepted decision or one proposed in this view"
                        ),
                        "name the decision that replaced it, or give an editorial reason",
                    ));
                }
            }
            Some(b) => {
                return Err(bad(
                    format!(
                        "implements names {id}, a {}; implements names the decision this change carries out",
                        b.block_type
                    ),
                    "name the decision (D-…), or give an editorial reason",
                ));
            }
            None => {
                return Err(bad(
                    format!("implements names {id}, which this view does not hold"),
                    format!(
                        "include the change that records {id} (rdy_view include), or wait until {id} lands"
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// D-YQCY9 (FW-DF1): a write's `implements: [D…]` adds `{target: D}` to the `implements` links
/// of each block in `rewritten` (created, or updated with an edit and not naming `implements`),
/// when the block is not a decision, its type is neither supporting nor structural (doc, folder,
/// section), the pack has the `implements` link kind, and the block does not link to D already.
/// Existing links come first, then the new ones in the justification's order. The pending op's
/// block and the overlay both change; the second pass pins each new target.
fn implements_links(
    pack: &Pack,
    ov: &mut Overlay,
    ops: &mut [Op],
    rewritten: &BTreeSet<BlockId>,
    j: Option<&Justification>,
) {
    let Some(Justification::Implements(decisions)) = j else {
        return;
    };
    if decisions.is_empty() || !pack.link_types.contains(decision::IMPLEMENTS) {
        return;
    }
    for op in ops.iter_mut() {
        let block = match op {
            Op::Create { block } | Op::Update { block, .. } => block,
            Op::Delete { .. } => continue,
        };
        if !rewritten.contains(&block.id) || !link_recipient(pack, block) {
            continue;
        }
        let mut added = false;
        for d in decisions {
            let linked = block
                .links
                .get(decision::IMPLEMENTS)
                .is_some_and(|l| l.iter().any(|l| l.target == *d));
            if linked {
                continue;
            }
            block
                .links
                .entry(decision::IMPLEMENTS.to_string())
                .or_default()
                .push(Link {
                    target: d.clone(),
                    part: None,
                });
            added = true;
        }
        if added {
            ov.set(block.id.clone(), Some(block.clone()));
        }
    }
}

/// Whether a write-level justification links `block` (D-YQCY9, rf8): not a decision, and of a
/// type that is neither supporting nor structural (doc, folder, section).
fn link_recipient(pack: &Pack, block: &Block) -> bool {
    block.block_type != decision::DECISION
        && pack.class_of(&block.block_type) != TypeClass::Supporting
        && !pack
            .block_types
            .get(&block.block_type)
            .is_some_and(|t| t.traits.contains(&rdy_schema::Trait::Structural))
}

/// The blocks of a write that a `reflects` input reads (`build_ops` gathers them in its op walk).
struct ReflectsSets<'s> {
    /// Created, or updated with an edit: `code: true` stamps those with reflects globs.
    written: &'s BTreeSet<BlockId>,
    /// `written` less the updates that name the `reflects` link kind: each gets the sources.
    recipients: &'s BTreeSet<BlockId>,
    /// Those whose op names the `reflects` link kind: each target must be a held source.
    explicit: &'s BTreeSet<BlockId>,
}

/// `E_BAD_INPUT`: `what` (an id, or the text of a selector that resolved to nothing) is not held.
fn reflects_not_held(what: &str) -> RdyError {
    bad(
        format!("reflects names {what}, which this view does not hold"),
        format!("include the change that records {what}, or create the source in this write"),
    )
}

/// `id` as a `reflects` target: a source the overlay holds once every op has applied.
fn held_source(ov: &Overlay, id: &BlockId) -> Result<(), RdyError> {
    match ov.get(id) {
        None => Err(reflects_not_held(id.as_str())),
        Some(b) if b.block_type != supporting::SOURCE => Err(bad(
            format!(
                "reflects names {id}, a {}; reflects names a source",
                b.block_type
            ),
            "name a source (S-…), or create one with a source op",
        )),
        Some(_) => Ok(()),
    }
}

/// rf8 (D-4WC6R): each target of an explicit `reflects` link (the ops of `explicit`, which name
/// the kind) is a source the final overlay holds. Every write checks them, justified or not.
fn explicit_reflects_held(
    ov: &Overlay,
    ops: &[Op],
    explicit: &BTreeSet<BlockId>,
) -> Result<(), RdyError> {
    if explicit.is_empty() {
        return Ok(());
    }
    for op in ops {
        let (Op::Create { block } | Op::Update { block, .. }) = op else {
            continue;
        };
        if !explicit.contains(&block.id) {
            continue;
        }
        for link in block
            .links
            .get(supporting::REFLECTS_LINK)
            .into_iter()
            .flatten()
        {
            held_source(ov, &link.target)?;
        }
    }
    Ok(())
}

/// rf8 (D-4WC6R, D-XC9B9; O10): resolves `raw` in the write's final overlay, once every op has
/// applied and before the second pass validates and pins. Each selector resolves as an op's id
/// does (a `$name` placeholder, an id, a key or a path), with keys and paths read as this write
/// leaves them; a source created anywhere in the write counts, one it deletes does not. The
/// sources are kept in first occurrence. Explicit `reflects` links of the write must name held sources too. With `code`, at
/// least one written content block must declare reflects globs; those are the blocks to stamp.
/// Then each recipient (`link_recipient`, not naming the kind itself) gets every source as a
/// `reflects` link it does not hold yet, after its existing ones, in the pending op and the
/// overlay; the second pass pins them.
fn reflects_links(
    pack: &Pack,
    ov: &mut Overlay,
    res: &Resolver,
    ops: &mut [Op],
    raw: &ReflectsInput,
    sets: &ReflectsSets,
) -> Result<ReflectsBuilt, RdyError> {
    let mut sources: Vec<BlockId> = vec![];
    let mut seen: BTreeSet<BlockId> = BTreeSet::new();
    // the view as the write leaves it, built only for a selector that may be a path
    let mut fin: Option<Materialized> = None;
    for text in &raw.sources {
        let resolved = if text.starts_with('$') || BlockId::parse(text).is_ok() {
            res.id(text)
        } else {
            let m = fin.get_or_insert_with(|| res.base.with(ops));
            res.id_in(m, text)
        };
        let id = match resolved {
            Ok(id) => id,
            // a key or path that names nothing: the text as given
            Err(e) if e.code == E_UNKNOWN_BLOCK && e.message.starts_with("no folder or doc at") => {
                return Err(reflects_not_held(text));
            }
            Err(e) => return Err(e),
        };
        held_source(ov, &id)?;
        if seen.insert(id.clone()) {
            sources.push(id);
        }
    }
    explicit_reflects_held(ov, ops, sets.explicit)?;
    let mut code = BTreeSet::new();
    if raw.code {
        for op in ops.iter() {
            let (Op::Create { block } | Op::Update { block, .. }) = op else {
                continue;
            };
            if sets.written.contains(&block.id)
                && link_recipient(pack, block)
                && !crate::reflects::glob_set(block).is_empty()
            {
                code.insert(block.id.clone());
            }
        }
        if code.is_empty() {
            return Err(bad(
                "reflects code: no block this write changes declares reflects globs",
                "set reflects on the block, or name a source",
            ));
        }
    }
    if sources.is_empty() {
        return Ok(ReflectsBuilt {
            justification: Justification::Reflects {
                sources,
                code: raw.code,
            },
            code,
        });
    }
    // each recipient's missing sources, counted before any link is built: the change is one
    // record, so links whose ids alone pass its bound are refused as the record check would
    // refuse them (5000 sources on 5000 blocks would otherwise build millions of links)
    let mut adds: Vec<(usize, Vec<&BlockId>)> = vec![];
    let mut bytes = 0usize;
    for (i, op) in ops.iter().enumerate() {
        let (Op::Create { block } | Op::Update { block, .. }) = op else {
            continue;
        };
        if !sets.recipients.contains(&block.id) || !link_recipient(pack, block) {
            continue;
        }
        let held: BTreeSet<&BlockId> = block
            .links
            .get(supporting::REFLECTS_LINK)
            .into_iter()
            .flatten()
            .map(|l| &l.target)
            .collect();
        let missing: Vec<&BlockId> = sources.iter().filter(|s| !held.contains(s)).collect();
        bytes = missing
            .iter()
            .fold(bytes, |n, s| n.saturating_add(s.as_str().len()));
        if bytes > rdy_store::bounded::MAX_RECORD_BYTES {
            return Err(RdyError::new(
                E_LIMIT,
                format!(
                    "{} exceeds {} {}",
                    crate::reaffirm::WRITE_CHANGE,
                    rdy_store::bounded::MAX_RECORD_BYTES,
                    rdy_store::bounded::RECORD_BYTES
                ),
                crate::reaffirm::WRITE_FIX,
            ));
        }
        if !missing.is_empty() {
            adds.push((i, missing));
        }
    }
    let adds: Vec<(usize, Vec<BlockId>)> = adds
        .into_iter()
        .map(|(i, m)| (i, m.into_iter().cloned().collect()))
        .collect();
    for (i, missing) in adds {
        let (Op::Create { block } | Op::Update { block, .. }) = &mut ops[i] else {
            continue;
        };
        block
            .links
            .entry(supporting::REFLECTS_LINK.to_string())
            .or_default()
            .extend(
                missing
                    .into_iter()
                    .map(|target| Link { target, part: None }),
            );
        ov.set(block.id.clone(), Some(block.clone()));
    }
    Ok(ReflectsBuilt {
        justification: Justification::Reflects {
            sources,
            code: raw.code,
        },
        code,
    })
}

/// kfol1 (FW-PR3a, FW-PR3b): on a move or rename, a key rdy assigned (`key_auto`) follows the
/// new path, for the block the write itself renames or moves only: the docs and folders under a
/// moved or renamed folder keep their keys, so a folder move changes no block under it (no
/// suspects, no review marks, no conflicts). A doc or folder that a write edits without renaming
/// or moving it keeps its key, even if its path changed with a folder's. A conflicted block is
/// never re-keyed here unless the write edits it, as it is never one an op did not name.
///
/// First the yield (FW-PR3b): a new or renamed top-level doc or folder name equal (ignoring
/// case) to a key rdy assigned to another block makes that key yield. It is re-derived from
/// its holder's path with the suffix rule, and the holder gets an update op of its own, keeping
/// its pins, when this write does not edit it. An explicit key does not yield (K2 refuses the
/// ambiguous text and names both ids). Then the roots: the docs and folders whose name or
/// parent an op changed, re-derived in (new path, id) order, after all of them give up their old
/// keys, so one can take a key another left.
///
/// Taken, as on create: every key the view and this write's ops hold, and the name of every
/// other top-level doc or folder. Each new key goes into `assigned`; returns the blocks that only
/// this pass changed.
fn follow_keys(
    ov: &mut Overlay,
    res: &mut Resolver,
    ops: &mut Vec<Op>,
    prior: &mut Vec<Prior>,
    touched: &BTreeSet<BlockId>,
    conflicts: &[Conflict],
    assigned: &mut BTreeMap<String, String>,
) -> BTreeSet<BlockId> {
    let base = ov.base;
    let is_auto =
        |b: &Block| b.fields.get(rdy_model::key::AUTO_FIELD) == Some(&FieldValue::Bool(true));
    let mut only = BTreeSet::new();

    // the yield: new top-level names, in order
    let mut names: Vec<(String, BlockId)> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Create { block } | Op::Update { block, .. }
                if tree::is_tree_type(&block.block_type) && block.parent.is_none() =>
            {
                let now = tree_path(ov, &block.id)?;
                (Some(&now) != base.path_of(&block.id).as_ref()).then(|| (now, block.id.clone()))
            }
            _ => None,
        })
        .collect();
    names.sort();
    for (name, by) in names {
        let Some((_, h)) = res.key_holder(&name) else {
            continue;
        };
        if h == by || (!touched.contains(&h) && conflicts.iter().any(|c| c.block == h)) {
            continue;
        }
        if !ov.get(&h).is_some_and(is_auto) {
            continue; // an explicit key wins
        }
        let Some(path) = tree_path(ov, &h) else {
            continue;
        };
        res.note_key(&h, None);
        if rekey(ov, res, ops, prior, &h, &path, assigned) {
            only.insert(h);
        }
    }

    // the roots: blocks this write renames or moves
    let mut follow: Vec<(String, BlockId)> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Update { block, .. }
                if tree::is_tree_type(&block.block_type)
                    && is_auto(block)
                    && keys::key_of(block).is_some() =>
            {
                let now = tree_path(ov, &block.id)?;
                let was = base.path_of(&block.id)?;
                let leaf = |p: &str| p.rsplit('/').next().map(str::to_string);
                let moved = base.get(&block.id).map(|b| &b.parent) != Some(&block.parent);
                let renamed = leaf(&now) != leaf(&was);
                let from = |p: &str| rdy_model::key::from_path(p, &block.block_type);
                ((moved || renamed) && from(&now) != from(&was)).then(|| (now, block.id.clone()))
            }
            _ => None,
        })
        .collect();
    follow.sort();
    for (_, id) in &follow {
        res.note_key(id, None);
    }
    for (path, id) in follow {
        if rekey(ov, res, ops, prior, &id, &path, assigned) {
            only.insert(id);
        }
    }
    only
}

/// Re-derives the key rdy assigned to `id` from `path` (whose old key `res` no longer counts),
/// in the op this write has for it or, if none, in an update op of its own that keeps its pins.
/// Returns whether the op is new.
fn rekey(
    ov: &mut Overlay,
    res: &mut Resolver,
    ops: &mut Vec<Op>,
    prior: &mut Vec<Prior>,
    id: &BlockId,
    path: &str,
    assigned: &mut BTreeMap<String, String>,
) -> bool {
    let cur = ov.get(id).expect("a re-keyed block is in the view");
    let was = cur.version_hash();
    let mut block = cur.clone();
    let k = keys::auto_key(path, &block.block_type, &|c| {
        res.key_holder(c).is_some()
            || ov.with_sibs(None, |s| {
                s.holder(&rdy_model::key::fold(c), Some(id)).is_some()
            })
    });
    block
        .fields
        .insert("key".into(), FieldValue::Str(k.clone()));
    res.note_key(id, Some(&block));
    let in_write = ops.iter_mut().find_map(|op| match op {
        Op::Create { block: b } | Op::Update { block: b, .. } if b.id == *id => Some(b),
        _ => None,
    });
    let new = match in_write {
        Some(b) => {
            *b = block.clone();
            false
        }
        None => {
            prior.push((true, block.pins.clone()));
            ops.push(Op::Update {
                base: was,
                block: block.clone(),
            });
            true
        }
    };
    ov.set(id.clone(), Some(block));
    assigned.insert(id.to_string(), k);
    new
}

impl Rdy {
    pub fn write(&mut self, session: &Session, req: WriteRequest) -> Result<WriteResult, RdyError> {
        self.write_briefed(session, req, None)
    }

    /// `write`, citing the briefing whose token is `brief` (bwr1): a write that touches stable
    /// content needs a current one; a draft-only write citing a stale one gets `W_BRIEF_STALE`.
    pub fn write_briefed(
        &mut self,
        session: &Session,
        req: WriteRequest,
        brief: Option<String>,
    ) -> Result<WriteResult, RdyError> {
        self.write_linked(session, req, ChangeLinks::default(), brief.as_deref())
    }

    /// `write`, with `links` (intent, alternative, client nonce) recorded on the change and the
    /// briefing `brief` cited (`write_briefed`). MCP and the CLI don't use it yet: M2d1 adds
    /// intents, and M2f the nonce. A write to a member view of an open alternative set with no
    /// `alternative` link gets one naming that set and the view (alts1).
    ///
    /// The change is checked against the view as the published state holds it, which is the
    /// view's present state only if every store write made outside the engine was followed by
    /// `invalidate()` (as `rdy_qa::fixtures::raw_commit` does). The new state is published
    /// before this returns.
    pub fn write_linked(
        &mut self,
        session: &Session,
        req: WriteRequest,
        links: ChangeLinks,
        brief: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        self.write_marked(session, req, links, brief, &BTreeSet::new(), None)
    }

    /// rf8 (D-4WC6R, D-XC9B9): `write_linked`, justified by `reflects`: its sources resolve in
    /// the write's own overlay (a source created in the same write counts if the final state
    /// holds it), each content block the write creates or rewrites links them, and with
    /// `code: true` each such block that declares reflects globs records its code state
    /// (`WriteResult::code_recorded`). The change stores the normalized `Reflects`. A request
    /// that carries a justification of its own is refused: a write takes one.
    pub fn write_reflecting(
        &mut self,
        session: &Session,
        req: WriteRequest,
        reflects: ReflectsInput,
        links: ChangeLinks,
        brief: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        if req.justification.is_some() {
            return Err(one_justification_error());
        }
        self.write_marked(session, req, links, brief, &BTreeSet::new(), Some(reflects))
    }

    /// `write_linked`, where the keys that updates set on the blocks in `rdy_keys` are rdy's
    /// (`key_auto`): the engine's own route for `rdy keys assign`, which agent input never takes.
    /// `reflects` is a raw `reflects` justification ([`Rdy::write_reflecting`]); a request whose
    /// own justification is `Reflects` goes through the same checks, its ids read as selectors.
    pub(crate) fn write_marked(
        &mut self,
        session: &Session,
        mut req: WriteRequest,
        mut links: ChangeLinks,
        brief: Option<&str>,
        rdy_keys: &BTreeSet<BlockId>,
        reflects: Option<ReflectsInput>,
    ) -> Result<WriteResult, RdyError> {
        let reflects = match (reflects, req.justification.take()) {
            (Some(_), Some(_)) => return Err(one_justification_error()),
            (None, Some(Justification::Reflects { sources, code })) => Some(ReflectsInput {
                sources: sources.iter().map(ToString::to_string).collect(),
                code,
            }),
            (raw, j) => {
                req.justification = j;
                raw
            }
        };
        if let Some(r) = &reflects {
            r.check()?;
        }
        if !session.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot write", session.role),
                "start the session as a writer",
            ));
        }
        if req.ops.is_empty() {
            return Err(bad("no ops given", "pass at least one op"));
        }
        let scope = Scope::View(req.view.clone());
        let st = self.read();
        check_view_access(&st, &req.view, session, views::Action::Commit)?;
        let claim = views::commit_claim(&st, session, &req.view)?;
        // alts1: a change written in a member view of an open set records the set
        if links.alternative.is_none()
            && let Some((set, _, _)) = st.alt_of_view(&req.view)?
        {
            links.alternative = Some(AltChoice {
                set,
                member: req.view.clone(),
            });
        }
        let conflicts = st.conflicts(&scope)?;
        let included = st.included(&req.view);
        let m = st.materialized(&scope)?;
        check_implements(&st, &scope, &m, req.justification.as_ref())?;
        // D-Z8P09: a finding or a source created with no parent goes to the decision home, as a
        // decision does; done before the briefing check, so the home is one of its subjects
        home_supporting_parents(self, &req.view, &mut req.ops)?;
        // D-EJBPD: what each reaffirm op selects, once, on the starting view, before the
        // briefing check (which must cover exactly those decisions) and before any op applies
        let selection = crate::reaffirm::select(&st, &scope, &req.ops)?;
        let brief_warnings = crate::brief::check(
            &self.pack,
            &self.briefs,
            &m,
            || st.graph(&scope),
            &req.ops,
            &selection,
            brief,
        )?;
        let reaffirmable = |id: &BlockId| crate::reaffirm::reaffirmable(&st, &scope, id);
        let (mut ops, created, touched, names, keys, reaffirmed, code_repins, reflected) =
            build_ops(
                &self.pack,
                &self.store,
                &m,
                session,
                req.ops,
                &ViewConflicts {
                    view: &req.view,
                    list: &conflicts,
                    included: &included,
                    reaffirmable: &reaffirmable,
                },
                req.justification.as_ref(),
                rdy_keys,
                &selection,
                reflects.as_ref(),
            )?;
        let no_code = BTreeSet::new();
        // D-01QX5: the code state of each reflects block this write creates, re-globs or
        // repins with `code: true`, read from the linked checkouts' committed trees; rf8: and
        // of each written block with globs that a `reflects` with `code: true` justifies
        let (code_warnings, stamped) = crate::reflects::stamp(
            &self.root,
            &self.store,
            &m,
            &mut ops,
            &code_repins,
            reflected.as_ref().map_or(&no_code, |r| &r.code),
        )?;
        refuse_removed_content(&self.store, &ops)?;
        let code_recorded = reflects.as_ref().filter(|r| r.code).map(|_| {
            let mut ids: Vec<String> = stamped.iter().map(ToString::to_string).collect();
            ids.sort();
            ids
        });
        let justification = match reflected {
            Some(r) => Some(r.justification),
            None => req.justification,
        };
        let justification_info = match &justification {
            Some(j @ Justification::Reflects { .. }) => Some(crate::JustificationInfo::from(j)),
            _ => None,
        };
        let mut change = Change {
            view: req.view.clone(),
            ops: ops.clone(),
            deps: vec![],
            provenance: session.provenance(),
            justification,
            message: req.message,
            links,
            read_identity: None,
        };
        // every change is one record, and a write grows with its blocks (a re-affirmation with
        // its decisions, D-EJBPD): the change must fit one record, or a later read or export of
        // the history would refuse it (FW-UP1a), checked before anything is written. The record
        // the store writes holds the change's deps, so the check measures the change with the
        // deps the commit below gives it (the same call on the same state; the commit sets them
        // again)
        change.deps = self
            .store
            .change_deps_resolving(&change, Some(&req.view), &conflicts)?;
        let (what, fix) = if reaffirmed.is_empty() {
            (crate::reaffirm::WRITE_CHANGE, crate::reaffirm::WRITE_FIX)
        } else {
            (
                crate::reaffirm::REAFFIRM_CHANGE,
                crate::reaffirm::WRITE_CHANGE_FIX,
            )
        };
        crate::reaffirm::change_fits(&change, what, fix)?;
        let id = self.store.commit_to_view_resolving(
            &req.view,
            change,
            &|id| m.version_of(id),
            &conflicts,
            claim,
        )?;
        self.publish(Delta::Commit {
            view: req.view.clone(),
            change: id,
            ops,
        })?;

        let m = self.read().materialized(&scope)?;
        let mut docs = BTreeSet::new();
        for t in &touched {
            if let Some(d) = m.doc_of(t) {
                docs.insert(d.id.clone());
            }
        }
        let mut warnings = vec![];
        for d in docs {
            let doc = m.get(&d).expect("doc exists");
            let titles: Vec<&str> = m
                .children(Some(&d))
                .iter()
                .filter(|b| b.block_type == "section")
                .filter_map(|b| b.field_str("title"))
                .collect();
            warnings.extend(skeleton_gaps(&self.pack, doc, &titles));
        }
        warnings.extend(brief_warnings);
        warnings.extend(code_warnings);
        // FW-I1: the authored words of each decision or question this write touched
        for t in &touched {
            if let Some(b) = m.get(t) {
                warnings.extend(crate::card_words::card_words_issues(
                    &b.id,
                    &b.block_type,
                    &b.fields,
                ));
            }
        }
        Ok(WriteResult {
            change: id.to_string(),
            created,
            names,
            keys,
            inherited: BTreeMap::new(),
            changes: vec![],
            reaffirmed,
            warnings,
            code_recorded,
            justification: justification_info,
        })
    }
}

/// The engine's input boundary for removed content (plan Redaction): a block that `ops` write
/// and that is a read projection of a removed version (a stub, with its recorded identity), or
/// whose version hash a redaction removed, is refused with `E_REDACTED` naming that redaction,
/// before anything is written. The store repeats both checks on every write.
fn refuse_removed_content(reader: &rdy_store::StoreReader, ops: &[Op]) -> Result<(), RdyError> {
    for op in ops {
        let (Op::Create { block } | Op::Update { block, .. }) = op else {
            continue;
        };
        let request = match &block.read_identity {
            Some(id) => Some(id.request),
            None => reader.redaction_of(rdy_store::kinds::BLOCK, &block.version_hash())?,
        };
        if let Some(request) = request {
            return Err(rdy_store::StoreError::Redacted { request }.into());
        }
    }
    Ok(())
}

/// May `s` do `action` on `view`? The view must be writable at all (`writable_view`; a proposal
/// skips this, so legacy `:` views stay proposable by the human). A human session may do
/// anything anywhere. An agent session never acts on the human's `work` view; otherwise it may
/// act on a view with no head yet (commit and include create it), on an unowned head only by
/// committing (the commit claims it), and on a head it owns (same actor and run).
pub(crate) fn check_view_access(
    st: &ReadState,
    view: &str,
    s: &Session,
    action: views::Action,
) -> Result<(), RdyError> {
    let propose = action == views::Action::Propose;
    non_empty_view(view)?;
    if !propose {
        writable_view(view)?;
    }
    if s.kind != SessionKind::Agent {
        return Ok(());
    }
    not_humans_view(view, s)?;
    let Some(h) = st.view_head(view) else {
        return Ok(());
    };
    match &h.owner {
        None if action == views::Action::Commit => Ok(()),
        None => Err(RdyError::new(
            E_VIEW_OWNER,
            format!("view {view:?} is unowned"),
            if propose {
                "commit to it to claim it, or include its changes into your own view and propose \
                 that"
                    .to_string()
            } else {
                format!("commit to it to claim it, or fork it (rdy view fork {view} <name>)")
            },
        )),
        Some(o) if (&o.actor, &o.run) == (&s.actor, &s.run) => Ok(()),
        Some(o) => Err(RdyError::new(
            E_VIEW_OWNER,
            format!(
                "view {view:?} is owned by {}",
                crate::identity::actor_run_text(&o.actor, o.run.as_deref())
            ),
            if propose && view != s.default_view() {
                format!(
                    "propose your own view ({}), or include its changes into your own view and \
                     propose that",
                    s.default_view()
                )
            } else {
                own_view_fix(view, s)
            },
        )),
    }
}

/// An agent session never acts on, creates or forks into the human's `work` view.
pub(crate) fn not_humans_view(view: &str, s: &Session) -> Result<(), RdyError> {
    if s.kind == SessionKind::Agent && view == "work" {
        return Err(RdyError::new(
            E_VIEW_OWNER,
            "view \"work\" is the human's default view",
            own_view_fix(view, s),
        ));
    }
    Ok(())
}

/// The `E_VIEW_OWNER` fix. When the refused view is the session's own default view, sending the
/// agent back there would loop: it is told to start a session with another run name, or fork.
fn own_view_fix(view: &str, s: &Session) -> String {
    let own = s.default_view();
    if view == own {
        "this is your default view and another session owns it: start a session with another run \
         name (RDY_RUN, or rdy_session's run) to write to a view of that name, or fork this view"
            .to_string()
    } else {
        format!(
            "write to your own view ({own}), fork it, or include its changes into your own view"
        )
    }
}

/// Whether `name` is reserved for the isolated landings of re-affirmation cards (D-CQMA4): it
/// starts with `reaffirm-` ([`rdy_store::REAFFIRM_VIEW_PREFIX`]). No public write, view op,
/// create, fork or session takes such a view; only the card's private writer creates one.
pub fn is_reserved_view(name: &str) -> bool {
    name.starts_with(rdy_store::REAFFIRM_VIEW_PREFIX)
}

/// The refusal of a view name [`is_reserved_view`] reserves.
pub fn reserved_view_error() -> RdyError {
    RdyError::new(
        E_RESERVED_VIEW,
        "view names that start with reaffirm- are reserved for re-affirmation cards",
        "pick a view name that does not start with reaffirm-",
    )
}

/// Views that can receive writes: not `canon`, not reserved for re-affirmation cards
/// ([`is_reserved_view`]), and no `:` (reserved for scope kinds such as `tag:` and `landing:`;
/// legacy M1 views with `:` stay readable and proposable via `view:<name>`).
pub(crate) fn writable_view(name: &str) -> Result<(), RdyError> {
    non_empty_view(name)?;
    if is_reserved_view(name) {
        return Err(reserved_view_error());
    }
    if name == "canon" {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            "canon cannot be written directly",
            "write to a view and land it with rdy_propose",
        ));
    }
    if name == crate::session::INFLIGHT_SCOPE {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            "inflight is a read scope (canon plus every live view's changes), not a view",
            "write to a view under another name",
        ));
    }
    if name.contains(':') {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            format!(
                "view names cannot contain ':' ({name:?}): ':' introduces scope kinds such as tag: and landing:"
            ),
            "write to a view name without ':' (for example replace it with '-')",
        ));
    }
    Ok(())
}

/// A view name has at most this many characters.
pub(crate) const MAX_VIEW_NAME: usize = 256;

/// The checks every view name a write or proposal targets passes: `E_BAD_INPUT` for the empty
/// name, `E_LIMIT` for one longer than `MAX_VIEW_NAME` characters (R10).
fn non_empty_view(name: &str) -> Result<(), RdyError> {
    if name.is_empty() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a view name is empty",
            "name a view, e.g. work",
        ));
    }
    if name.chars().count() > MAX_VIEW_NAME {
        return Err(RdyError::new(
            E_LIMIT,
            format!(
                "a view name is longer than {MAX_VIEW_NAME} characters ({})",
                views::clip(name)
            ),
            "use a shorter view name",
        ));
    }
    Ok(())
}

/// The editorial class named `name`; `E_BAD_INPUT` for any other name.
pub fn editorial_class(name: &str) -> Result<rdy_model::EditorialClass, RdyError> {
    use rdy_model::EditorialClass;
    match name {
        "typo" => Ok(EditorialClass::Typo),
        "restructure" => Ok(EditorialClass::Restructure),
        "clarify" => Ok(EditorialClass::Clarify),
        other => Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown editorial class {other:?}"),
            "use typo, restructure or clarify",
        )),
    }
}

/// A write's justification from its caller's `implements` (decision ids, keys or paths, each
/// resolved in `scope`, deduplicated in order), `editorial` (class and reason) or `reflects` (the
/// raw object, kept raw: its sources resolve in the write's overlay). More than one given,
/// `implements` empty, or `reflects` with `code: false` is `E_BAD_INPUT`. The one rule
/// `rdy_write` and the ctl `wc_commit` share.
pub fn parse_justification(
    read: &ReadState,
    scope: &Scope,
    implements: Option<Vec<String>>,
    editorial: Option<(String, String)>,
    reflects: Option<ReflectsArg>,
) -> Result<ParsedJustification, RdyError> {
    let given = usize::from(implements.is_some())
        + usize::from(editorial.is_some())
        + usize::from(reflects.is_some());
    if given > 1 {
        return Err(one_justification_error());
    }
    if let Some(raw) = reflects {
        return Ok(ParsedJustification {
            stored: None,
            reflects: Some(raw.input()?),
        });
    }
    let stored = match (implements, editorial) {
        (Some(_), Some(_)) => return Err(one_justification_error()),
        (Some(texts), None) => {
            if texts.is_empty() {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "implements is empty",
                    "name the decision this change carries out, or leave implements out",
                ));
            }
            let mut ids: Vec<BlockId> = vec![];
            for t in &texts {
                let id = read.resolve_id(scope, t)?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            Some(Justification::Implements(ids))
        }
        (None, Some((class, reason))) => Some(Justification::Editorial {
            class: editorial_class(&class)?,
            reason,
        }),
        (None, None) => None,
    };
    Ok(ParsedJustification {
        stored,
        reflects: None,
    })
}

/// sus3: the design pins a repin of `cur` (the block `id`) selects, each with the content hash it
/// pins: the `targets` given (`resolve` reads each id or key), or with none every block `cur`
/// references or links to except itself. A target `cur` does not cite, `cur` itself, and one
/// that `content` (a target's current content hash) cannot find are refused, in target order.
/// The pins already current are selected too. The write and [`Rdy::repin_selection`] share it.
fn repin_targets(
    cur: &Block,
    id: &BlockId,
    targets: Option<&[String]>,
    resolve: &dyn Fn(&str) -> Result<BlockId, RdyError>,
    content: &dyn Fn(&BlockId) -> Option<Hash>,
) -> Result<Vec<(BlockId, Hash)>, RdyError> {
    let cited: BTreeSet<BlockId> = cur
        .ref_tokens()
        .iter()
        .filter_map(|t| BlockId::parse(t).ok())
        .chain(cur.links.values().flatten().map(|l| l.target.clone()))
        .collect();
    // a block is never pinned to itself, so a repin of everything skips its own id
    let wanted: Vec<BlockId> = match targets {
        None | Some([]) => cited.iter().filter(|t| *t != id).cloned().collect(),
        Some(given) => given.iter().map(|t| resolve(t)).collect::<Result<_, _>>()?,
    };
    let mut out = Vec::with_capacity(wanted.len());
    for t in wanted {
        if t == *id {
            return Err(bad(
                format!("{id} is never pinned to itself"),
                "a block's own edits never make it a suspect; repin the other targets it cites",
            ));
        }
        if !cited.contains(&t) {
            return Err(bad(
                format!("{id} does not reference or link to {t}"),
                "repin only targets the block cites; rdy_context lists its suspects",
            ));
        }
        let h = content(&t).ok_or_else(|| {
            RdyError::new(
                E_UNKNOWN_REF,
                format!("{id} cites {t}, which does not exist in this view"),
                format!("remove the reference or link to {t} from {id} instead of re-pinning it"),
            )
        })?;
        out.push((t, h));
    }
    Ok(out)
}

impl Rdy {
    /// What a repin of `id` in `view` would select (sus3), read-only: the block's id and how many
    /// design pins the repin selects, already-current pins included: each distinct target once,
    /// however many arguments name it (by id or key), as the block holds one pin per target.
    /// `targets` as the repin op takes them (`None` or empty: every block it cites). The id and
    /// targets are checked as the write checks them; nothing is written.
    pub fn repin_selection(
        &self,
        session: &Session,
        view: &str,
        id: &str,
        targets: Option<&[String]>,
    ) -> Result<(BlockId, usize), RdyError> {
        let st = self.read();
        check_view_access(&st, view, session, views::Action::Commit)?;
        let m = st.materialized(&Scope::View(view.to_string()))?;
        let res = Resolver::new(&m);
        let id = res.id(id)?;
        let cur = m.get(&id).ok_or_else(|| {
            RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id} in this view"),
                "check the id with rdy_read",
            )
        })?;
        let tracking = self.store.tracking();
        let selected = repin_targets(cur, &id, targets, &|t| res.id(t), &|t| {
            m.get(t).map(|x| x.content_hash_with(tracking))
        })?;
        let pins: BTreeSet<&BlockId> = selected.iter().map(|(t, _)| t).collect();
        Ok((id, pins.len()))
    }
}
