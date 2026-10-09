//! Accepting suggestions in bulk (FW-EN3).
use crate::error::*;
use crate::lexicon::link_kind_for;
use crate::write::{LinkInput, OpInput, WriteRequest, WriteResult};
use crate::{Rdy, Scope, Session};
use rdy_model::BlockId;
use rdy_schema::Trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The most suggestions one accept takes.
pub const MAX_ACCEPT: usize = 500;

/// One suggestion, as a write or search response gave it (keys it does not use are ignored).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptItem {
    Link {
        block: String,
        link: String,
        target: String,
    },
    Entity {
        literal: String,
        #[serde(default)]
        blocks: Vec<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default, rename = "type")]
        block_type: Option<String>,
        #[serde(default)]
        entity_kind: Option<String>,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        unit: Option<String>,
        #[serde(default)]
        parent: Option<String>,
        /// The new entity's body (a term needs one).
        #[serde(default)]
        body: Option<String>,
    },
}

/// What an accept took.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Accepted {
    pub links: usize,
    pub entities: usize,
}

/// A suggestion (by index) that was left out, and why.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Skipped {
    pub index: usize,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcceptResult {
    #[serde(flatten)]
    pub write: WriteResult,
    pub accepted: Accepted,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Skipped>,
}

fn bad(msg: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, msg, fix)
}

impl Rdy {
    /// Accept `items` in one write to `view`: link suggestions become links (added, never
    /// replacing), entity suggestions create the entity under `parent` (or their own) and link
    /// the blocks that can. A link already there, or added by an earlier item, is skipped.
    pub fn accept(
        &mut self,
        s: &Session,
        view: &str,
        items: Vec<AcceptItem>,
        parent: Option<&str>,
        brief: Option<String>,
        message: Option<String>,
    ) -> Result<AcceptResult, RdyError> {
        if items.is_empty() {
            return Err(bad(
                "no suggestions to accept",
                "pass suggestions from a write or search response",
            ));
        }
        if items.len() > MAX_ACCEPT {
            return Err(RdyError::new(
                E_LIMIT,
                format!(
                    "{} suggestions in one call; the limit is {MAX_ACCEPT}",
                    items.len()
                ),
                "accept them in batches",
            ));
        }
        let total = items.len();
        let st = self.read();
        let scope = Scope::View(view.to_string());
        let m = st.materialized(&scope)?;
        let is_entity_type = |t: &str| {
            self.pack
                .block_types
                .get(t)
                .is_some_and(|d| d.traits.contains(&Trait::Defines))
        };
        let mut creates: Vec<OpInput> = vec![];
        // the targets to add, by (block, kind), in the order they were accepted
        let mut pending: BTreeMap<(BlockId, &'static str), Vec<String>> = BTreeMap::new();
        let mut skipped: Vec<Skipped> = vec![];
        let mut new_links = 0usize;
        let mut add = |pending: &mut BTreeMap<(BlockId, &'static str), Vec<String>>,
                       skipped: &mut Vec<Skipped>,
                       index: usize,
                       block: BlockId,
                       kind: &'static str,
                       target: String|
         -> Result<(), RdyError> {
            let b = m.get(&block).ok_or_else(|| {
                RdyError::new(
                    E_UNKNOWN_BLOCK,
                    format!("no block {block} in view {view}"),
                    "check the id with rdy_read",
                )
            })?;
            let has = b
                .links
                .get(kind)
                .is_some_and(|ls| ls.iter().any(|l| l.target.to_string() == target));
            let list = pending.entry((block.clone(), kind)).or_default();
            if has || list.contains(&target) {
                skipped.push(Skipped {
                    index,
                    reason: format!("{block} already links to {target} by {kind}"),
                });
            } else {
                list.push(target);
                new_links += 1;
            }
            Ok(())
        };
        for (i, item) in items.into_iter().enumerate() {
            match item {
                AcceptItem::Link {
                    block,
                    link,
                    target,
                } => {
                    let kind: &'static str = match link.as_str() {
                        "describes" => "describes",
                        "affects" => "affects",
                        _ => {
                            return Err(bad(
                                format!("suggestion {i}: link {link} is not describes or affects"),
                                "accept link suggestions as rdy gave them",
                            ));
                        }
                    };
                    let block = st.resolve_id(&scope, &block)?;
                    let target = st.resolve_id(&scope, &target)?;
                    let t = m.get(&target).ok_or_else(|| {
                        RdyError::new(
                            E_UNKNOWN_BLOCK,
                            format!("no block {target} in view {view}"),
                            "check the id with rdy_read",
                        )
                    })?;
                    if !is_entity_type(&t.block_type) {
                        return Err(bad(
                            format!(
                                "suggestion {i}: {target} is a {}, not an entity",
                                t.block_type
                            ),
                            "link to an entity: a block of a type that defines one (rdy_schema entities.types)",
                        ));
                    }
                    // a stale item (the block is gone, or can no longer link this way) is
                    // skipped; the rest of the batch still lands
                    let Some(b) = m.get(&block) else {
                        skipped.push(Skipped {
                            index: i,
                            reason: format!("no block {block} in view {view}"),
                        });
                        continue;
                    };
                    match link_kind_for(&self.pack, b) {
                        None => skipped.push(Skipped {
                            index: i,
                            reason: format!("{block} cannot link to an entity"),
                        }),
                        Some(k) if k != kind => skipped.push(Skipped {
                            index: i,
                            reason: format!("{block} links to an entity by {k}, not {link}"),
                        }),
                        Some(_) => add(
                            &mut pending,
                            &mut skipped,
                            i,
                            block,
                            kind,
                            target.to_string(),
                        )?,
                    }
                }
                AcceptItem::Entity {
                    literal,
                    blocks,
                    name,
                    block_type,
                    entity_kind,
                    value,
                    unit,
                    parent: own_parent,
                    body,
                } => {
                    let t = block_type.unwrap_or_else(|| "entity".to_string());
                    if !is_entity_type(&t) {
                        let types: Vec<&str> = self
                            .pack
                            .block_types
                            .iter()
                            .filter(|(_, d)| d.traits.contains(&Trait::Defines))
                            .map(|(n, _)| n.as_str())
                            .collect();
                        return Err(bad(
                            format!("suggestion {i}: type {t} does not define an entity"),
                            format!("use one of: {}", types.join(", ")),
                        ));
                    }
                    let Some(parent_text) = own_parent.as_deref().or(parent) else {
                        return Err(bad(
                            format!("suggestion {i}: a new entity needs a parent"),
                            "pass parent (a doc or section for new entities) on the call or on the suggestion",
                        ));
                    };
                    let parent_id = st.resolve_id(&scope, parent_text)?;
                    let temp = format!("$en{i}");
                    let mut fields = BTreeMap::new();
                    fields.insert(
                        "name".to_string(),
                        serde_json::Value::String(
                            name.as_deref().unwrap_or(&literal).trim().to_string(),
                        ),
                    );
                    for (k, v) in [("kind", entity_kind), ("value", value), ("unit", unit)] {
                        if let Some(v) = v {
                            fields.insert(k.to_string(), serde_json::Value::String(v));
                        }
                    }
                    creates.push(OpInput::Create {
                        block_type: t,
                        id: Some(temp.clone()),
                        parent: Some(parent_id.to_string()),
                        after: None,
                        before: None,
                        fields,
                        body: body.unwrap_or_default(),
                        links: BTreeMap::new(),
                    });
                    for b in blocks {
                        let id = st.resolve_id(&scope, &b)?;
                        let Some(blk) = m.get(&id) else {
                            skipped.push(Skipped {
                                index: i,
                                reason: format!("no block {id} in view {view}"),
                            });
                            continue;
                        };
                        match link_kind_for(&self.pack, blk) {
                            Some(k) => add(&mut pending, &mut skipped, i, id, k, temp.clone())?,
                            None => skipped.push(Skipped {
                                index: i,
                                reason: format!("{b} cannot link to an entity"),
                            }),
                        }
                    }
                }
            }
        }
        // one update per block, holding each touched kind as its current links plus the new ones
        let mut updates: BTreeMap<BlockId, BTreeMap<String, Vec<LinkInput>>> = BTreeMap::new();
        for ((block, kind), targets) in pending {
            if targets.is_empty() {
                continue;
            }
            let cur = m.get(&block).expect("checked when added");
            let mut list: Vec<LinkInput> = cur
                .links
                .get(kind)
                .map(|ls| {
                    ls.iter()
                        .map(|l| LinkInput {
                            target: l.target.to_string(),
                            part: l.part.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            list.extend(
                targets
                    .into_iter()
                    .map(|target| LinkInput { target, part: None }),
            );
            updates
                .entry(block)
                .or_default()
                .insert(kind.to_string(), list);
        }
        if creates.is_empty() && updates.is_empty() {
            return Err(bad(
                "nothing to accept: every link is already there",
                "nothing to do",
            ));
        }
        let entities = creates.len();
        let mut ops = creates;
        for (id, links) in updates {
            ops.push(OpInput::Update {
                id: id.to_string(),
                base: None,
                fields: BTreeMap::new(),
                unset: vec![],
                body: None,
                links: Some(links),
                block_type: None,
                move_to: None,
            });
        }
        drop(st);
        let write = self.write_briefed(
            s,
            WriteRequest {
                view: view.to_string(),
                ops,
                justification: None,
                message: message.unwrap_or_else(|| format!("accept {total} suggestions")),
            },
            brief,
        )?;
        Ok(AcceptResult {
            write,
            accepted: Accepted {
                links: new_links,
                entities,
            },
            skipped,
        })
    }
}
