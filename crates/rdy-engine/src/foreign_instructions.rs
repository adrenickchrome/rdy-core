//! The instruction layers a repo link adds (rpl22, D-DAPR8, D-MEMMQ, D-8X04F).
//!
//! [`collect`] reads the project's canon and its delivered foreign content once and gives one
//! [`Contribution`] per layer that holds a rule in force. A rule is in force when its link block
//! pins it (D-MEMMQ): an adopt answer settles a delivery but puts nothing in force by itself, so a
//! rule that a delivery only offers is not delivered here, and a rule shows at its pinned version
//! (its text and its lock). A link delivers into the layer of the set its last adopt answer
//! served: `organization`, `team:<name>`, or, with no set, `cited:<alias>`. A no-set cited rule
//! shows its stored lock and has no force ([`Layer::locks_have_force`]). A revoked link delivers
//! nothing.
//!
//! The ruleset of a cited rule is the doc above it in the delivered content. Its handle names the
//! link, so two links of one layer (one repo under two profiles included) never share one:
//! `organization:<alias>:<doc>`, `team:<name>:<alias>:<doc>` or `cited:<alias>:<doc>`. The delivery
//! rules of the project's own sources stay as they are: a ruleset is on the menu with its
//! `required` mark, a required ruleset and every rule selecting `all` are delivered whole, a
//! loaded ruleset is delivered whole, and a selector is a hint, never a filter. One rule is
//! delivered once per reply, in the first layer, then the first link, that shows it.
use crate::error::RdyError;
use crate::foreign_refs::RepoLinks;
use crate::instructions::{
    Bounds, Contribution, InstructionRow, Layer, LoadRequest, MenuEntry, Reason, estimate_tokens,
    exceeded, link_handle, selectors, text_of,
};
use crate::pending::adopt::adopted_set;
use crate::query::source_body;
use crate::{ReadState, Scope};
use rdy_model::foreign::{ForeignId, ForeignSet};
use rdy_model::{Block, BlockId, FieldValue, RepoId, decision, tree};
use rdy_schema::instruction::{ALL, APPLIES_TO, INSTRUCTION_PREFIX, LOCKED, REQUIRED_FIELD};
use rdy_store::foreign::SourceView;
use std::collections::{BTreeMap, BTreeSet};

/// The most parents one rule's doc search climbs.
const CLIMB: usize = 64;

/// One adopted rule: the block at its pinned version.
struct Rule {
    block: Block,
}

/// One link's adopted rulesets, in doc order.
struct Part<'a> {
    layer: Layer,
    alias: String,
    repo: RepoId,
    view: &'a SourceView,
    sets: Vec<Set>,
}

/// One ruleset of a link: its doc and its adopted rules.
struct Set {
    doc: BlockId,
    title: String,
    required: bool,
    rules: Vec<Rule>,
}

fn is_rule(id: &BlockId) -> bool {
    id.prefix() == INSTRUCTION_PREFIX
}

/// The contributions of the project's repo links for `load`: one per layer with a rule in force,
/// in layer order. Every count is checked before anything is copied (`E_LIMIT`).
pub(crate) fn collect(
    state: &ReadState,
    load: &LoadRequest,
    b: &Bounds,
) -> Result<Vec<Contribution>, RdyError> {
    let foreign = state.foreign()?;
    if foreign.is_empty() {
        return Ok(vec![]);
    }
    let m = state.materialized(&Scope::Canon)?;
    let links = RepoLinks::of_view(&m);
    let mut parts: Vec<Part<'_>> = vec![];
    let mut rulesets = 0usize;
    for (alias, row) in links.rows() {
        let Some(view) = foreign.link_source(&row.link) else {
            continue;
        };
        if view.revoked {
            continue;
        }
        let Some(link) = m.get(&row.link) else {
            continue;
        };
        let mut adopted: Vec<Block> = vec![];
        for (target, pin) in &link.foreign.pins {
            if target.repo != row.repo || !is_rule(&target.id) {
                continue;
            }
            if adopted.len() >= b.visits {
                return Err(exceeded(
                    b.visits as u64,
                    "blocks in one source",
                    b.visits as u64 + 1,
                ));
            }
            if let Some(v) = state.foreign_held_pin(target, pin)?
                && let Some(blk) = state.foreign_held(target, &v)?
            {
                adopted.push(blk);
            }
        }
        if adopted.is_empty() {
            continue;
        }
        let layer = match adopted_set(state, &row.repo, view.head)? {
            Some(ForeignSet::Organization) => Layer::Organization,
            Some(ForeignSet::Team(n)) => Layer::Team(n),
            None => Layer::Cited(alias.to_string()),
        };
        let mut by_doc: BTreeMap<BlockId, Vec<Rule>> = BTreeMap::new();
        for blk in adopted {
            let doc = doc_of(state, view, row.repo, &blk)?;
            by_doc.entry(doc).or_default().push(Rule { block: blk });
        }
        rulesets += by_doc.len();
        if rulesets > b.rulesets {
            return Err(exceeded(b.rulesets as u64, "rulesets", rulesets as u64));
        }
        let mut sets = vec![];
        for (doc, mut rules) in by_doc {
            rules.sort_by(|x, y| x.block.id.cmp(&y.block.id));
            for r in &rules {
                if let Some(FieldValue::List(items)) = r.block.fields.get(APPLIES_TO) {
                    if items.len() > b.selectors {
                        return Err(exceeded(
                            b.selectors as u64,
                            "selectors in one instruction",
                            items.len() as u64,
                        ));
                    }
                    if let Some(n) = items
                        .iter()
                        .filter_map(FieldValue::as_str)
                        .map(str::len)
                        .find(|n| *n > b.selector_bytes)
                    {
                        return Err(exceeded(
                            b.selector_bytes as u64,
                            "bytes in one selector",
                            n as u64,
                        ));
                    }
                }
            }
            let head = doc_block(state, view, row.repo, &doc)?;
            sets.push(Set {
                title: head
                    .as_ref()
                    .and_then(|d| d.field_str("title"))
                    .unwrap_or("")
                    .to_string(),
                required: head
                    .as_ref()
                    .is_some_and(|d| d.fields.get(REQUIRED_FIELD) == Some(&FieldValue::Bool(true))),
                doc,
                rules,
            });
        }
        parts.push(Part {
            layer,
            alias: alias.to_string(),
            repo: row.repo,
            view,
            sets,
        });
    }
    parts.sort_by(|x, y| {
        x.layer
            .precedence()
            .cmp(&y.layer.precedence())
            .then(x.layer.cmp(&y.layer))
            .then(x.alias.cmp(&y.alias))
    });
    let mut seen: BTreeSet<(RepoId, BlockId)> = BTreeSet::new();
    let mut out: BTreeMap<Layer, (Vec<MenuEntry>, Vec<InstructionRow>)> = BTreeMap::new();
    for part in &parts {
        let (menu, rows) = out.entry(part.layer.clone()).or_default();
        for set in &part.sets {
            let handle = link_handle(&part.layer, &part.alias, &set.doc);
            let whole = set.required || load.names(&part.layer, Some(&part.alias), &set.doc);
            let mut bytes = 0u64;
            let mut hints: Vec<String> = vec![];
            let mut hinted = BTreeSet::new();
            for r in &set.rules {
                let text = text_of(&r.block);
                bytes = bytes.saturating_add(text.len() as u64);
                let sels = selectors(&r.block);
                for s in &sels {
                    if hinted.insert(s.to_string()) {
                        hints.push(s.to_string());
                    }
                }
                if (whole || sels.contains(&ALL)) && seen.insert((part.repo, r.block.id.clone())) {
                    rows.push(InstructionRow {
                        id: r.block.id.to_string(),
                        ruleset: handle.clone(),
                        text: text.into_owned(),
                        layer: part.layer.clone(),
                        locked: r.block.fields.get(LOCKED) == Some(&FieldValue::Bool(true)),
                        reason: reasons(state, part.view, part.repo, &r.block)?,
                    });
                }
            }
            menu.push(MenuEntry {
                id: handle,
                name: set.title.clone(),
                when: String::new(),
                layer: part.layer.clone(),
                required: set.required,
                tokens: estimate_tokens(bytes),
                hints,
            });
        }
    }
    out.into_iter()
        .map(|(layer, (menu, rows))| Contribution::assemble(layer, menu, rows, b))
        .collect()
}

/// The block that the source holds as member `id`, at its delivered version.
fn member(
    state: &ReadState,
    view: &SourceView,
    repo: RepoId,
    id: &BlockId,
) -> Result<Option<Block>, RdyError> {
    match view.members.get(id) {
        Some(held) => state.foreign_held(&ForeignId::new(repo, id.clone()), &held.version.version),
        None => Ok(None),
    }
}

/// The doc above rule `blk` in the delivered content: its nearest doc ancestor, else the
/// topmost ancestor the delivery names, else the rule itself.
fn doc_of(
    state: &ReadState,
    view: &SourceView,
    repo: RepoId,
    blk: &Block,
) -> Result<BlockId, RdyError> {
    let mut last = blk.id.clone();
    let mut cur = blk.parent.clone();
    for _ in 0..CLIMB {
        let Some(p) = cur else {
            break;
        };
        last = p.clone();
        match member(state, view, repo, &p)? {
            Some(b) if b.block_type == tree::DOC => return Ok(p),
            Some(b) => cur = b.parent,
            None => break,
        }
    }
    Ok(last)
}

fn doc_block(
    state: &ReadState,
    view: &SourceView,
    repo: RepoId,
    doc: &BlockId,
) -> Result<Option<Block>, RdyError> {
    Ok(member(state, view, repo, doc)?.filter(|b| b.block_type == tree::DOC))
}

/// The reasons of a cited rule: each decision it names that the source delivers, with its
/// statement (the removal notice when its content was removed).
fn reasons(
    state: &ReadState,
    view: &SourceView,
    repo: RepoId,
    rule: &Block,
) -> Result<Vec<Reason>, RdyError> {
    let mut out = vec![];
    for l in rule.links.get(decision::DECIDED_BY).into_iter().flatten() {
        let Some(t) = member(state, view, repo, &l.target)? else {
            continue;
        };
        if t.block_type != decision::DECISION {
            continue;
        }
        out.push(Reason {
            id: t.id.to_string(),
            statement: if t.is_redacted() {
                source_body(&t)
            } else {
                t.field_str("statement").unwrap_or("").to_string()
            },
        });
    }
    Ok(out)
}
