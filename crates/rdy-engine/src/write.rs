use crate::error::*;
use crate::{Materialized, Rdy, Scope, Session};
use rdy_model::{decision, order_between, Block, BlockId, Change, FieldValue, Hash, Justification, Link, Op};
use rdy_schema::{has_errors, skeleton_gaps, validate_block, Issue, Pack};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
        parent: String,
        #[serde(default)]
        after: Option<String>,
    },
    Delete {
        id: String,
        #[serde(default)]
        base: Option<String>,
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
    pub warnings: Vec<Issue>,
}

pub fn json_to_field(v: &serde_json::Value) -> Result<FieldValue, String> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => FieldValue::Null,
        J::Bool(b) => FieldValue::Bool(*b),
        J::Number(n) => FieldValue::Int(n.as_i64().ok_or_else(|| format!("number {n} is not an integer: floats are not supported, write it as a string"))?),
        J::String(s) => FieldValue::Str(s.clone()),
        J::Array(a) => FieldValue::List(a.iter().map(json_to_field).collect::<Result<_, _>>()?),
        J::Object(o) => FieldValue::Map(o.iter().map(|(k, v)| Ok((k.clone(), json_to_field(v)?))).collect::<Result<_, String>>()?),
    })
}

fn bad(msg: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, msg, fix)
}

/// Pending edits layered over a materialized view.
struct Overlay<'a> {
    base: &'a Materialized,
    pending: BTreeMap<BlockId, Option<Block>>,
}

impl<'a> Overlay<'a> {
    fn get(&self, id: &BlockId) -> Option<&Block> {
        match self.pending.get(id) {
            Some(p) => p.as_ref(),
            None => self.base.get(id),
        }
    }

    fn children(&self, parent: Option<&BlockId>) -> Vec<Block> {
        let mut out: Vec<Block> = self.base.children(parent).into_iter().filter(|b| !self.pending.contains_key(&b.id)).cloned().collect();
        out.extend(self.pending.values().flatten().filter(|b| b.parent.as_ref() == parent).cloned());
        out.sort_by(|a, b| (&a.order, &a.id).cmp(&(&b.order, &b.id)));
        out
    }
}

struct Resolver {
    placeholders: BTreeMap<String, BlockId>,
}

impl Resolver {
    fn id(&self, s: &str) -> Result<BlockId, RdyError> {
        if let Some(name) = s.strip_prefix('$') {
            return self.placeholders.get(s).cloned().ok_or_else(|| bad(format!("placeholder ${name} is used before it is created"), "create the block earlier in the same batch, or use a real id"));
        }
        BlockId::parse(s).map_err(|e| bad(e.to_string(), "ids look like b-7KQ2M; find them with rdy_read or rdy_search"))
    }

    fn text(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (name, id) in &self.placeholders {
            out = out.replace(&format!("{{{{{name}}}}}"), &format!("{{{{{id}}}}}"));
        }
        out
    }

    fn value(&self, v: FieldValue) -> FieldValue {
        match v {
            FieldValue::Str(s) => FieldValue::Str(self.text(&s)),
            FieldValue::List(items) => FieldValue::List(items.into_iter().map(|i| self.value(i)).collect()),
            FieldValue::Map(m) => FieldValue::Map(m.into_iter().map(|(k, v)| (k, self.value(v))).collect()),
            other => other,
        }
    }

    fn links(&self, links: BTreeMap<String, Vec<LinkInput>>) -> Result<BTreeMap<String, Vec<Link>>, RdyError> {
        links.into_iter().map(|(k, v)| Ok((k, v.into_iter().map(|l| Ok(Link { target: self.id(&l.target)?, part: l.part })).collect::<Result<Vec<_>, RdyError>>()?))).collect()
    }

    fn fields(&self, fields: BTreeMap<String, serde_json::Value>) -> Result<BTreeMap<String, FieldValue>, RdyError> {
        fields.into_iter().map(|(k, v)| Ok((k.clone(), self.value(json_to_field(&v).map_err(|e| bad(format!("field {k}: {e}"), "write the value as a string"))?)))).collect()
    }
}

fn parse_base(s: &Option<String>) -> Result<Option<Hash>, RdyError> {
    s.as_ref().map(|h| h.parse::<Hash>().map_err(|e| bad(e.to_string(), "pass base as the 64-character version hash from rdy_context"))).transpose()
}

fn order_after(ov: &Overlay, parent: Option<&BlockId>, after: Option<&BlockId>) -> Result<String, RdyError> {
    let siblings = ov.children(parent);
    let parent_name = parent.map(|p| p.to_string()).unwrap_or_else(|| "the document root".to_string());
    match after {
        None => Ok(order_between(siblings.last().map(|b| b.order.as_str()), None)),
        Some(a) => {
            let pos = siblings.iter().position(|b| &b.id == a).ok_or_else(|| bad(format!("{a} is not a child of {parent_name}"), "pass an `after` id that is a sibling under `parent`, or omit it to append"))?;
            Ok(order_between(Some(&siblings[pos].order), siblings.get(pos + 1).map(|b| b.order.as_str())))
        }
    }
}

fn current_for_edit<'b>(ov: &'b Overlay, id: &BlockId, base: &Option<String>, touched: &BTreeSet<BlockId>) -> Result<(Hash, &'b Block), RdyError> {
    if touched.contains(id) {
        return Err(bad(format!("{id} is changed more than once in this write"), "combine the edits into one op"));
    }
    let cur = ov.get(id).ok_or_else(|| RdyError::new(E_UNKNOWN_BLOCK, format!("no block {id} in this view"), "check the id with rdy_read"))?;
    let cur_hash = cur.version_hash();
    if let Some(b) = parse_base(base)?
        && b != cur_hash
    {
        return Err(RdyError::new(E_STALE_BASE, format!("{id} changed since you read it: your base is {}, current is {}", b.short(), cur_hash.short()), format!("re-read {id} (rdy_context) and retry with base={cur_hash}")));
    }
    if decision::is_frozen(cur) {
        return Err(RdyError::new(E_FROZEN, format!("{id} is an accepted or rejected decision and cannot change"), format!("write a new decision that supersedes or revokes {id}")));
    }
    Ok((cur_hash, cur))
}

type Built = (Vec<Op>, BTreeMap<String, String>, BTreeSet<BlockId>);

type Prior = (bool, BTreeMap<BlockId, Hash>);

fn build_ops(pack: &Pack, base: &Materialized, session: &Session, inputs: Vec<OpInput>) -> Result<Built, RdyError> {
    let mut ov = Overlay { base, pending: BTreeMap::new() };
    let mut res = Resolver { placeholders: BTreeMap::new() };
    let mut ops = vec![];
    let mut created = BTreeMap::new();
    let mut touched = BTreeSet::new();
    let mut prior: Vec<Prior> = vec![];

    for (i, input) in inputs.into_iter().enumerate() {
        match input {
            OpInput::Create { block_type, id, parent, after, fields, body, links } => {
                let def = pack.block_types.get(&block_type).ok_or_else(|| {
                    RdyError::new(E_VALIDATION, format!("unknown block type {block_type:?}"), format!("use one of: {}", pack.block_types.keys().cloned().collect::<Vec<_>>().join(", ")))
                })?;
                let new_id = match id.as_deref() {
                    Some(p) if p.starts_with('$') => {
                        if res.placeholders.contains_key(p) {
                            return Err(bad(format!("placeholder {p} is defined twice in this write"), "use a different $name for each created block"));
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
                    return Err(bad(format!("{new_id} is changed more than once in this write"), "combine the edits into one op"));
                }
                if ov.get(&new_id).is_some() {
                    return Err(RdyError::new(E_ALREADY_EXISTS, format!("block {new_id} already exists"), "omit id to let rdy generate one"));
                }
                let parent = parent.as_deref().map(|p| res.id(p)).transpose()?;
                if let Some(p) = &parent
                    && ov.get(p).is_none()
                {
                    return Err(RdyError::new(E_UNKNOWN_BLOCK, format!("parent {p} does not exist"), "create the parent first or check its id"));
                }
                let after = after.as_deref().map(|a| res.id(a)).transpose()?;
                let order = order_after(&ov, parent.as_ref(), after.as_ref())?;
                let block = Block { id: new_id.clone(), block_type, parent, order, fields: res.fields(fields)?, body: res.text(&body), links: res.links(links)?, pins: BTreeMap::new() };
                touched.insert(new_id.clone());
                ov.pending.insert(new_id, Some(block.clone()));
                ops.push(Op::Create { block });
                prior.push((false, BTreeMap::new()));
            }
            OpInput::Update { id, base: b, fields, unset, body, links } => {
                let id = res.id(&id)?;
                let (base_hash, cur) = current_for_edit(&ov, &id, &b, &touched)?;
                let mut block = cur.clone();
                block.fields.extend(res.fields(fields)?);
                for k in unset {
                    block.fields.remove(&k);
                }
                if let Some(body) = body {
                    block.body = res.text(&body);
                }
                if let Some(links) = links {
                    block.links = res.links(links)?;
                }
                prior.push((false, cur.pins.clone()));
                touched.insert(id.clone());
                ov.pending.insert(id, Some(block.clone()));
                ops.push(Op::Update { base: base_hash, block });
            }
            OpInput::Move { id, base: b, parent, after } => {
                let id = res.id(&id)?;
                let parent = res.id(&parent)?;
                let after = after.as_deref().map(|a| res.id(a)).transpose()?;
                let (base_hash, cur) = current_for_edit(&ov, &id, &b, &touched)?;
                let mut block = cur.clone();
                if ov.get(&parent).is_none() {
                    return Err(RdyError::new(E_UNKNOWN_BLOCK, format!("parent {parent} does not exist"), "check the parent id"));
                }
                let mut walk = Some(parent.clone());
                while let Some(w) = walk {
                    if w == id {
                        return Err(bad(format!("cannot move {id} under itself or one of its descendants"), "choose a parent outside the subtree being moved"));
                    }
                    walk = ov.get(&w).and_then(|b| b.parent.clone());
                }
                block.parent = Some(parent.clone());
                block.order = order_after(&ov, Some(&parent), after.as_ref())?;
                prior.push((true, block.pins.clone()));
                touched.insert(id.clone());
                ov.pending.insert(id, Some(block.clone()));
                ops.push(Op::Update { base: base_hash, block });
            }
            OpInput::Delete { id, base: b } => {
                let id = res.id(&id)?;
                let (base_hash, _) = current_for_edit(&ov, &id, &b, &touched)?;
                if !ov.children(Some(&id)).is_empty() {
                    return Err(RdyError::new(E_HAS_CHILDREN, format!("{id} still has children"), "delete or move its children first"));
                }
                touched.insert(id.clone());
                ov.pending.insert(id.clone(), None);
                prior.push((false, BTreeMap::new()));
                ops.push(Op::Delete { base: base_hash, id });
            }
        }
    }

    // second pass: references, pins, decision permissions, schema validation
    let mut issues = vec![];
    for (op, (is_move, old_pins)) in ops.iter_mut().zip(prior) {
        let block = match op {
            Op::Create { block } | Op::Update { block, .. } => block,
            Op::Delete { .. } => continue,
        };
        let mut pins = BTreeMap::new();
        for tok in block.ref_tokens() {
            if let Some(name) = tok.strip_prefix('$') {
                return Err(bad(format!("placeholder ${name} is used before it is created"), "create the block earlier in the same batch, or use a real id"));
            }
            let target = BlockId::parse(&tok).map_err(|_| RdyError::new(E_UNKNOWN_REF, format!("{{{{{tok}}}}} in {} is not a block id", block.id), "reference blocks as {{id}}, e.g. {{t-MEDGR}}; find ids with rdy_search"))?;
            let t = ov.get(&target).ok_or_else(|| RdyError::new(E_UNKNOWN_REF, format!("{} references {target}, which does not exist in this view", block.id), "fix the id, or create the target in the same write"))?;
            if let Some(h) = old_pins.get(&target) {
                pins.insert(target, *h);
            } else if !touched.contains(&target) {
                pins.insert(target, t.version_hash());
            }
        }
        for link in block.links.values().flatten() {
            let t = ov.get(&link.target).ok_or_else(|| RdyError::new(E_UNKNOWN_REF, format!("{} links to {}, which does not exist in this view", block.id, link.target), "fix the link target id"))?;
            if let Some(h) = old_pins.get(&link.target) {
                pins.insert(link.target.clone(), *h);
            } else if !touched.contains(&link.target) {
                pins.insert(link.target.clone(), t.version_hash());
            }
        }
        if !is_move {
            block.pins = pins;
        }
        if block.block_type == decision::DECISION && block.field_str("status") != Some("proposed") && !session.role.can_land() {
            return Err(RdyError::new(E_PERMISSION, format!("{} sets decision status {:?}", block.id, block.field_str("status").unwrap_or("")), "write status: proposed; the decision is accepted when its proposal lands"));
        }
        let parent = block.parent.as_ref().and_then(|p| ov.get(p));
        issues.extend(validate_block(pack, block, parent));
    }
    if has_errors(&issues) {
        let first = issues.iter().find(|i| i.severity == rdy_schema::Severity::Error).expect("an error");
        let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
        e.issues = issues;
        return Err(e);
    }
    Ok((ops, created, touched))
}

impl Rdy {
    pub fn block(&mut self, scope: &Scope, id: &BlockId) -> Result<Block, RdyError> {
        self.materialized(scope)?.get(id).cloned().ok_or_else(|| RdyError::new(E_UNKNOWN_BLOCK, format!("no block {id}"), "check the id with rdy_read"))
    }

    pub fn write(&mut self, session: &Session, req: WriteRequest) -> Result<WriteResult, RdyError> {
        if !session.role.can_write() {
            return Err(RdyError::new(E_PERMISSION, format!("role {:?} cannot write", session.role), "start the session as a writer"));
        }
        if req.view == "canon" {
            return Err(RdyError::new(E_RESERVED_VIEW, "canon cannot be written directly", "write to a view and land it with rdy_propose"));
        }
        if req.ops.is_empty() {
            return Err(bad("no ops given", "pass at least one op"));
        }
        let scope = Scope::View(req.view.clone());
        self.materialized(&scope)?;
        let (ops, created, touched) = {
            let base = self.cache.get(&scope).expect("materialized above");
            build_ops(&self.pack, base, session, req.ops)?
        };
        let change = Change { view: req.view.clone(), ops: ops.clone(), deps: vec![], provenance: session.provenance(), justification: req.justification, message: req.message };
        let id = self.store.commit_to_view_at(&req.view, change, &self.cache[&scope].snapshot)?;
        self.update_cache(&req.view, &ops);

        let m = self.cache.get(&scope).expect("cached");
        let mut docs = BTreeSet::new();
        for t in &touched {
            if let Some(d) = m.doc_of(t) {
                docs.insert(d.id.clone());
            }
        }
        let mut warnings = vec![];
        for d in docs {
            let doc = m.get(&d).expect("doc exists");
            let titles: Vec<&str> = m.children(Some(&d)).iter().filter(|b| b.block_type == "section").filter_map(|b| b.field_str("title")).collect();
            warnings.extend(skeleton_gaps(&self.pack, doc, &titles));
        }
        Ok(WriteResult { change: id.to_string(), created, warnings })
    }
}
