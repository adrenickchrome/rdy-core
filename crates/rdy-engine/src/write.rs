use crate::error::*;
use crate::keys::{self, Keys};
use crate::{Delta, Materialized, Rdy, ReadState, Scope, Session, SessionKind};
use rdy_model::tree;
use rdy_model::{
    Block, BlockId, Change, ChangeLinks, FieldValue, Hash, Justification, Link, Op, RefMode,
    decision, order_between, ref_spans,
};
use rdy_schema::{Issue, Pack, has_errors, skeleton_gaps, validate_block};
use serde::{Deserialize, Serialize};
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

#[derive(Clone, Debug, Deserialize)]
pub struct LinkInput {
    pub target: String,
    #[serde(default)]
    pub part: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
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
        #[serde(default)]
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
        #[serde(default)]
        links: Option<BTreeMap<String, Vec<LinkInput>>>,
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
    },
}

pub struct WriteRequest {
    pub view: String,
    pub ops: Vec<OpInput>,
    pub justification: Option<Justification>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WriteResult {
    pub change: String,
    pub created: BTreeMap<String, String>,
    /// Names rdy assigned to created docs and folders, by block id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    /// sup1: links rdy_supersede copied from the old decision, by kind.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub inherited: BTreeMap<String, Vec<String>>,
    pub warnings: Vec<Issue>,
}

pub fn json_to_field(v: &serde_json::Value) -> Result<FieldValue, String> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => FieldValue::Null,
        J::Bool(b) => FieldValue::Bool(*b),
        J::Number(n) => FieldValue::Int(n.as_i64().ok_or_else(|| {
            format!("number {n} is not an integer: floats are not supported, write it as a string")
        })?),
        J::String(s) => FieldValue::Str(s.clone()),
        J::Array(a) => FieldValue::List(a.iter().map(json_to_field).collect::<Result<_, _>>()?),
        J::Object(o) => FieldValue::Map(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), json_to_field(v)?)))
                .collect::<Result<_, String>>()?,
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
}

impl<'a> Overlay<'a> {
    fn new(base: &'a Materialized) -> Overlay<'a> {
        Overlay {
            base,
            pending: BTreeMap::new(),
            pending_kids: BTreeMap::new(),
            sibs: RefCell::default(),
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
        if let Some(name) = s.strip_prefix('$') {
            return self.placeholders.get(s).cloned().ok_or_else(|| {
                bad(
                    format!("placeholder ${name} is used before it is created"),
                    "create the block earlier in the same batch, or use a real id",
                )
            });
        }
        // not an id: a key (as this write has left them), else a folder or doc path, read in the
        // view as it was before this write
        keys::resolve_with(self.base, s, &|t| self.key_holder(t).map(|(_, id)| id))
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
    fn text(&self, s: &str, mode: RefMode) -> String {
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
        keys::rewrite(&out, mode, &|t| self.key_holder(t).map(|(_, id)| id))
    }

    fn value(&self, v: FieldValue) -> FieldValue {
        match v {
            FieldValue::Str(s) => FieldValue::Str(self.text(&s, RefMode::Strict)),
            FieldValue::List(items) => {
                FieldValue::List(items.into_iter().map(|i| self.value(i)).collect())
            }
            FieldValue::Map(m) => {
                FieldValue::Map(m.into_iter().map(|(k, v)| (k, self.value(v))).collect())
            }
            other => other,
        }
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
                    self.value(json_to_field(&v).map_err(|e| {
                        bad(format!("field {k}: {e}"), "write the value as a string")
                    })?),
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

fn order_after(
    ov: &Overlay,
    parent: Option<&BlockId>,
    after: Option<&BlockId>,
) -> Result<String, RdyError> {
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
            .map_err(|e| order_error(e, parent)),
        Some(k) => {
            // siblings created in parallel views may share a key: insert after the whole tie
            let next = s
                .order
                .range((Bound::Excluded(k), Bound::Unbounded))
                .find(|(o, _)| *o != k.0);
            order_between(Some(&k.0), next.map(|(o, _)| o.as_str()))
                .map_err(|e| order_error(e, parent))
        }
    })
}

/// first1: a key just before sibling `before`. Siblings created in parallel views may share a
/// key, so the new block goes before the whole tie.
fn order_before(
    ov: &Overlay,
    parent: Option<&BlockId>,
    before: &BlockId,
) -> Result<String, RdyError> {
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
            .find(|(o, _)| *o != anchor.0);
        order_between(prev.map(|(o, _)| o.as_str()), Some(&anchor.0))
            .map_err(|e| order_error(e, parent))
    })
}

/// The order key for a block placed after or before a sibling, or appended.
fn place(
    ov: &Overlay,
    parent: Option<&BlockId>,
    after: Option<&BlockId>,
    before: Option<&BlockId>,
) -> Result<String, RdyError> {
    match (after, before) {
        (Some(_), Some(_)) => Err(bad(
            "give after or before, not both",
            "after puts the block behind a sibling, before ahead of one",
        )),
        (None, Some(b)) => order_before(ov, parent, b),
        (after, None) => order_after(ov, parent, after),
    }
}

fn current_for_edit<'b>(
    ov: &'b Overlay,
    id: &BlockId,
    base: &Option<String>,
    touched: &BTreeSet<BlockId>,
) -> Result<(Hash, &'b Block), RdyError> {
    if touched.contains(id) {
        return Err(bad(
            format!("{id} is changed more than once in this write"),
            "combine the edits into one op",
        ));
    }
    let cur = ov.get(id).ok_or_else(|| {
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no block {id} in this view"),
            "check the id with rdy_read",
        )
    })?;
    let cur_hash = cur.version_hash();
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
    if decision::is_frozen(cur) {
        return Err(RdyError::new(
            E_FROZEN,
            format!("{id} is an accepted or rejected decision and cannot change"),
            format!("write a new decision that supersedes or revokes {id}"),
        ));
    }
    Ok((cur_hash, cur))
}

type Built = (
    Vec<Op>,
    BTreeMap<String, String>,
    BTreeSet<BlockId>,
    BTreeMap<String, String>,
);

type Prior = (bool, BTreeMap<BlockId, Hash>);

/// Strongly connected components of the graph `deps` (node -> nodes it points at), numbered so
/// that every component is numbered after all components it points at (Tarjan, iterative).
fn components(deps: &[Vec<usize>]) -> Vec<usize> {
    const NONE: usize = usize::MAX;
    let n = deps.len();
    let (mut index, mut low, mut comp) = (vec![NONE; n], vec![0; n], vec![NONE; n]);
    let mut on_stack = vec![false; n];
    let (mut stack, mut next, mut ncomp) = (vec![], 0, 0);
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        let mut call = vec![(root, 0usize)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&(v, e)) = call.last() {
            if let Some(&w) = deps[v].get(e) {
                call.last_mut().expect("frame").1 += 1;
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if let Some(&(u, _)) = call.last() {
                    low[u] = low[u].min(low[v]);
                }
                if low[v] == index[v] {
                    loop {
                        let w = stack.pop().expect("on stack");
                        on_stack[w] = false;
                        comp[w] = ncomp;
                        if w == v {
                            break;
                        }
                    }
                    ncomp += 1;
                }
            }
        }
    }
    comp
}

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
fn check_name(ov: &Overlay, block: &Block, op: usize, is_move: bool) -> Result<(), RdyError> {
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
                if is_move {
                    format!("a move takes no fields: rename {} first with an update (e.g. to {fixed:?}), then move it", block.id)
                } else {
                    format!("choose another name, e.g. {fixed:?}")
                },
            );
            return Err(if is_move {
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
    let is_asset = pack
        .block_types
        .get(&block.block_type)
        .is_some_and(|d| d.traits.contains(&rdy_schema::Trait::Asset));
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

fn build_ops(
    pack: &Pack,
    store: &rdy_store::Store,
    base: &Materialized,
    session: &Session,
    inputs: Vec<OpInput>,
) -> Result<Built, RdyError> {
    let mut ov = Overlay::new(base);
    let mut res = Resolver::new(base);
    let mut ops = vec![];
    let mut created = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut touched = BTreeSet::new();
    let mut prior: Vec<Prior> = vec![];
    let mut noop_rebalances: Vec<String> = vec![];
    let mut repinned: Vec<BlockId> = vec![];

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
                let def = pack.block_types.get(&block_type).ok_or_else(|| {
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
                })?;
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
                let order = place(&ov, parent.as_ref(), after.as_ref(), before.as_ref())?;
                let body = res.text(&body, RefMode::for_body(&block_type));
                let sets_key = fields.contains_key("key");
                let mut block = Block {
                    id: new_id.clone(),
                    block_type,
                    parent,
                    order,
                    fields: res.fields(fields)?,
                    body,
                    links: res.links(links)?,
                    pins: BTreeMap::new(),
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
                    check_name(&ov, &block, i, false)?;
                }
                check_decided_at(session, &block, None)?;
                check_depth(&ov, &block, 0)?;
                if sets_key {
                    res.check_key(&block)?;
                    res.note_key(&new_id, Some(&block));
                }
                touched.insert(new_id.clone());
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
            } => {
                let id = res.id(&id)?;
                let (base_hash, cur) = current_for_edit(&ov, &id, &b, &touched)?;
                let mut block = cur.clone();
                // a doc from M1 stores the name readers already use for it on its first write
                if block.block_type == tree::DOC
                    && !block.fields.contains_key("name")
                    && let Some(n) = ov.base.name_of(&id)
                {
                    block.fields.insert("name".into(), FieldValue::Str(n));
                }
                let sets_key = fields.contains_key("key");
                block.fields.extend(res.fields(fields)?);
                for k in unset {
                    block.fields.remove(&k);
                }
                if let Some(body) = body {
                    block.body = res.text(&body, RefMode::for_body(&block.block_type));
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
                check_decided_at(session, &block, Some(cur))?;
                prior.push((false, cur.pins.clone()));
                let renamed = block.field_str("name") != cur.field_str("name");
                if renamed {
                    check_name(&ov, &block, i, false)?;
                }
                if sets_key {
                    res.check_key(&block)?;
                }
                if keys::key_of(&block) != keys::key_of(cur) {
                    res.note_key(&id, Some(&block));
                }
                touched.insert(id.clone());
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
                let parent = parent.as_deref().map(|p| res.id(p)).transpose()?;
                let after = after.as_deref().map(|a| res.id(a)).transpose()?;
                let before = before.as_deref().map(|b| res.id(b)).transpose()?;
                if before.as_ref() == Some(&id) {
                    return Err(bad(
                        format!("cannot move {id} before itself"),
                        "name another sibling as before, or omit it",
                    ));
                }
                let (base_hash, cur) = current_for_edit(&ov, &id, &b, &touched)?;
                let mut block = cur.clone();
                if block.block_type == tree::DOC
                    && !block.fields.contains_key("name")
                    && let Some(n) = ov.base.name_of(&id)
                {
                    block.fields.insert("name".into(), FieldValue::Str(n));
                }
                if let Some(p) = &parent {
                    if ov.get(p).is_none() {
                        return Err(RdyError::new(
                            E_UNKNOWN_BLOCK,
                            format!("parent {p} does not exist"),
                            "check the parent id",
                        ));
                    }
                    let mut walk = Some(p.clone());
                    while let Some(w) = walk {
                        if w == id {
                            return Err(bad(
                                format!("cannot move {id} under itself or one of its descendants"),
                                "choose a parent outside the subtree being moved",
                            ));
                        }
                        walk = ov.get(&w).and_then(|b| b.parent.clone());
                    }
                }
                block.parent = parent.clone();
                block.order = place(&ov, parent.as_ref(), after.as_ref(), before.as_ref())?;
                check_name(&ov, &block, i, true)?;
                if block.block_type == tree::FOLDER {
                    check_depth(&ov, &block, folders_below(&ov, &id))?;
                } else {
                    check_depth(&ov, &block, 0)?;
                }
                prior.push((true, block.pins.clone()));
                touched.insert(id.clone());
                ov.set(id, Some(block.clone()));
                ops.push(Op::Update {
                    base: base_hash,
                    block,
                });
            }
            OpInput::Delete { id, base: b } => {
                let id = res.id(&id)?;
                let (base_hash, cur) = current_for_edit(&ov, &id, &b, &touched)?;
                if cur.block_type == decision::QUESTION {
                    return Err(bad(
                        format!(
                            "{id} is a question: questions are closed by a decision's answers link, never deleted"
                        ),
                        format!("record a decision that links answers: {id}"),
                    ));
                }
                if ov.with_sibs(Some(&id), |s| !s.order.is_empty()) {
                    return Err(RdyError::new(
                        E_HAS_CHILDREN,
                        format!("{id} still has children"),
                        "delete or move its children first",
                    ));
                }
                if keys::key_of(cur).is_some() {
                    res.note_key(&id, None);
                }
                touched.insert(id.clone());
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
                    let (base_hash, cur) = current_for_edit(&ov, &b.id, &None, &touched)?;
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
            } => {
                let id = res.id(&id)?;
                let (base_hash, cur) = current_for_edit(&ov, &id, &b, &touched)?;
                let cited: BTreeSet<BlockId> = cur
                    .ref_tokens()
                    .iter()
                    .filter_map(|t| BlockId::parse(t).ok())
                    .chain(cur.links.values().flatten().map(|l| l.target.clone()))
                    .collect();
                let targets_given = !targets.is_empty();
                let wanted: Vec<BlockId> = if targets.is_empty() {
                    cited.iter().cloned().collect()
                } else {
                    targets
                        .iter()
                        .map(|t| res.id(t))
                        .collect::<Result<_, _>>()?
                };
                let mut block = cur.clone();
                for t in &wanted {
                    if !cited.contains(t) {
                        return Err(bad(
                            format!("{id} does not reference or link to {t}"),
                            "repin only targets the block cites; rdy_context lists its suspects",
                        ));
                    }
                    let target = ov.get(t).ok_or_else(|| {
                        RdyError::new(
                            E_UNKNOWN_REF,
                            format!("{id} cites {t}, which does not exist in this view"),
                            format!(
                                "remove the reference or link to {t} from {id} instead of re-pinning it"
                            ),
                        )
                    })?;
                    block.pins.insert(t.clone(), target.version_hash());
                }
                // like a no-op rebalance, a repin that changes no pin is refused, per op
                if block.pins == cur.pins {
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
                repinned.extend(wanted);
                prior.push((true, block.pins.clone())); // pins set here; the second pass keeps them
                touched.insert(id.clone());
                ov.set(id, Some(block.clone()));
                ops.push(Op::Update {
                    base: base_hash,
                    block,
                });
            }
        }
    }
    // only rebalances that changed nothing: refuse rather than commit an empty change
    if ops.is_empty() && !noop_rebalances.is_empty() {
        return Err(bad(
            format!("nothing to rebalance under {}", noop_rebalances.join(", ")),
            "the children's keys are already short and in order; no write is needed",
        ));
    }

    // sus3: a repin to a block this write also changes would pin a version that never lands
    if let Some(t) = repinned.iter().find(|t| touched.contains(*t)) {
        return Err(bad(
            format!("{t} changes in this write, so a repin to it would pin an old version"),
            "repin in a separate write, after the write that changes the target",
        ));
    }

    // second pass: references, decision permissions, schema validation; pins to blocks outside
    // this write are set here, pins to blocks touched by it wait for the third pass
    let slot: BTreeMap<BlockId, usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Op::Create { block } | Op::Update { block, .. } => Some((block.id.clone(), i)),
            Op::Delete { .. } => None,
        })
        .collect();
    let mut deps: Vec<Vec<usize>> = vec![vec![]; ops.len()];
    let mut pinsets: Vec<Option<BTreeMap<BlockId, Hash>>> = vec![None; ops.len()];
    let mut issues = vec![];
    for (i, (op, (is_move, old_pins))) in ops.iter().zip(prior).enumerate() {
        let block = match op {
            Op::Create { block } | Op::Update { block, .. } => block,
            Op::Delete { .. } => continue,
        };
        let mut targets = vec![];
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
                    "reference blocks as {{id}} or {{KEY}}, e.g. {{t-MEDGR}}; find ids and keys with rdy_read",
                )
            })?;
            let t = ov.get(&target).ok_or_else(|| {
                RdyError::new(
                    E_UNKNOWN_REF,
                    format!(
                        "{} references {target}, which does not exist in this view",
                        block.id
                    ),
                    "fix the id, or create the target in the same write",
                )
            })?;
            targets.push((target, t.version_hash()));
        }
        for link in block.links.values().flatten() {
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
            targets.push((link.target.clone(), t.version_hash()));
        }
        if !is_move {
            let mut pins = BTreeMap::new();
            for (target, current) in targets {
                if let Some(h) = old_pins.get(&target) {
                    pins.insert(target, *h);
                } else if let Some(&j) = slot.get(&target) {
                    deps[i].push(j);
                } else {
                    pins.insert(target, current);
                }
            }
            pinsets[i] = Some(pins);
        }
        if block.block_type == decision::DECISION
            && block.field_str("status") != Some("proposed")
            && !session.role.can_land()
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
        issues.extend(validate_block(pack, block, parent));
        if !is_move {
            check_asset_blob(pack, store, block)?;
        }
    }

    // third pass: pin references between blocks of this write to the targets' final versions,
    // targets first; edges inside a reference cycle stay unpinned
    let comp = components(&deps);
    let mut order: Vec<usize> = (0..ops.len()).collect();
    order.sort_by_key(|&i| comp[i]);
    for i in order {
        let Some(mut pins) = pinsets[i].take() else {
            continue;
        };
        for &j in &deps[i] {
            if comp[j] != comp[i]
                && let Op::Create { block: t } | Op::Update { block: t, .. } = &ops[j]
            {
                pins.insert(t.id.clone(), t.version_hash());
            }
        }
        if let Op::Create { block } | Op::Update { block, .. } = &mut ops[i] {
            block.pins = pins;
        }
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
    Ok((ops, created, touched, names))
}

impl Rdy {
    pub fn write(&mut self, session: &Session, req: WriteRequest) -> Result<WriteResult, RdyError> {
        self.write_linked(session, req, ChangeLinks::default())
    }

    /// `write`, with `links` (intent, alternative, client nonce) recorded on the change. MCP and
    /// the CLI don't use it yet: M2d1 adds intents, and M2f the nonce.
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
    ) -> Result<WriteResult, RdyError> {
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
        check_view_access(&st, &req.view, session)?;
        let m = st.materialized(&scope)?;
        let (ops, created, touched, names) =
            build_ops(&self.pack, &self.store, &m, session, req.ops)?;
        let change = Change {
            view: req.view.clone(),
            ops: ops.clone(),
            deps: vec![],
            provenance: session.provenance(),
            justification: req.justification,
            message: req.message,
            links,
        };
        let id = self
            .store
            .commit_to_view_with(&req.view, change, &|id| m.version_of(id))?;
        self.publish(Delta::Commit {
            view: req.view.clone(),
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
        Ok(WriteResult {
            change: id.to_string(),
            created,
            names,
            inherited: BTreeMap::new(),
            warnings,
        })
    }
}

/// May `s` write into `view`? The view must be writable at all (`writable_view`); then an agent
/// session may only commit into a view with no head yet, an unowned head, or a head it owns
/// (same actor and run). A human session may write into any view.
pub(crate) fn check_view_access(st: &ReadState, view: &str, s: &Session) -> Result<(), RdyError> {
    writable_view(view)?;
    if s.kind != SessionKind::Agent {
        return Ok(());
    }
    match st.view_head(view) {
        Some(h) => match &h.owner {
            Some(o) if (&o.actor, &o.run) != (&s.actor, &s.run) => Err(RdyError::new(
                E_VIEW_OWNER,
                format!(
                    "view {view:?} is owned by {}{}",
                    o.actor,
                    o.run.as_ref().map(|r| format!("/{r}")).unwrap_or_default()
                ),
                own_view_fix(view, s),
            )),
            _ => Ok(()),
        },
        None => Ok(()),
    }
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

/// Views that can receive writes: not `canon`, and no `:` (reserved for scope kinds such as
/// `tag:` and `landing:`; legacy M1 views with `:` stay readable and proposable via `view:<name>`).
pub(crate) fn writable_view(name: &str) -> Result<(), RdyError> {
    if name == "canon" {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            "canon cannot be written directly",
            "write to a view and land it with rdy_propose",
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
