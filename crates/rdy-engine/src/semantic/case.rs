//! Case files (spec §9.3 step 2): everything a reviewer reads to judge one pair, hashed so a verdict binds to exactly what it read.
use crate::lexicon::{Lexicon, shown_text_in};
use crate::semantic::candidates::{Item, Rule, request_label};
use crate::tensions::index::{DetectCx, canon_index_of};
use crate::tensions::{DetectScope, SideRef};
use crate::{E_INTERNAL, E_REVIEW_STALE, Materialized, RdyError, ReadState};
use rdy_model::{Block, BlockId, Hash, decision};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

/// The question every tension case asks.
pub const CASE_QUESTION: &str = "Do these two blocks disagree? Answer tension when they cannot both hold, else no_tension. A tension names, for each block that takes part, what in it disagrees.";

/// The most governing decisions one case block lists.
const MAX_GOVERNING: usize = 10;
/// The most entities one case lists.
const MAX_ENTITIES: usize = 10;
/// The domain of a case hash.
const CASE_DOMAIN: &[u8] = b"rdy-case-1\0";

/// Everything a reviewer reads to judge one pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseFile {
    /// The request's label (`rq-<12 hex>`).
    pub request: String,
    /// The review kind: `tension`.
    pub kind: String,
    /// The best rule that holds between the two blocks now, if any.
    pub rule: Option<Rule>,
    pub question: String,
    /// The two blocks, in item order.
    pub blocks: Vec<CaseBlock>,
    /// The entities both blocks name or both describe, by id, at most 10.
    pub entities: Vec<CaseEntity>,
    /// The verdict schema ([`verdict_schema`]).
    pub schema: Value,
}

/// One block of a case, at the version the pair names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseBlock {
    pub id: String,
    #[serde(rename = "type")]
    pub block_type: String,
    /// The full label of the side that holds the block.
    pub side: String,
    /// The block's version (64 hex).
    pub version: String,
    /// The path of the block's doc in the in-flight scope.
    pub doc: String,
    /// The doc's title, then each section title from the top down to the block's parent.
    pub headings: Vec<String>,
    /// The text a mention scan reads in the block, in field order.
    pub text: Vec<CaseText>,
    /// The decisions that govern the block in the in-flight scope, by id, at most 10.
    pub governing: Vec<CaseDecision>,
}

/// One text field of a block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseText {
    pub field: String,
    pub text: String,
}

/// A decision that governs a case block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseDecision {
    pub id: String,
    /// The decision's `statement` field, or `""`.
    pub statement: String,
}

/// An entity both blocks of a case name or describe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseEntity {
    pub id: String,
    /// The entity's `name` field, or `""`.
    pub name: String,
    /// The entity's body.
    pub text: String,
}

/// The shape a verdict on a tension case takes, as the case shows it to the reviewer.
pub fn verdict_schema() -> Value {
    json!({
        "verdict": ["tension", "no_tension"],
        "citations": "a list of {block, note}: block is one of the two block ids; note is 1 to 2000 characters; a tension verdict needs 1 to 20 citations, a no_tension verdict 0 to 20",
        "severity": "advisory (default) or blocking, on a tension verdict only: blocking advises a person to make the tension block; it gates nothing until a person accepts it",
        "reason": "with severity blocking only: why the tension should block, 1 to 2000 characters",
    })
}

/// The hash a verdict binds to: `rdy-case-1\0` then the case as JSON, keys sorted.
pub fn case_hash(case: &CaseFile) -> Hash {
    let value = serde_json::to_value(case).expect("a case file serializes");
    let mut bytes = CASE_DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(&value).expect("a JSON value serializes"));
    Hash::of(&bytes)
}

impl ReadState {
    /// The case file of the pair of key `key` whose items are `items` (two, sorted by block), in
    /// the in-flight scope of this state. `E_REVIEW_STALE` when an item's version is not on its
    /// side in this state.
    pub fn case_of(&self, key: &Hash, items: &[Item]) -> Result<CaseFile, RdyError> {
        let label = request_label(key);
        let [ia, ib] = items else {
            return Err(RdyError::new(
                E_INTERNAL,
                format!("request {label} has {} items, not two", items.len()),
                "report this as a bug in rdy",
            ));
        };
        let set = self.tensions()?;
        let cx = DetectCx::new(
            self,
            set.writes(),
            canon_index_of(self)?,
            DetectScope::Inflight,
        );
        let m = cx.inflight_m()?;
        let g = cx.inflight_graph()?;
        let lex = cx.lexicon()?;
        let (a, b) = (
            block_of_item(&cx, ia, &label)?,
            block_of_item(&cx, ib, &label)?,
        );
        let mut blocks = vec![];
        // rf6: live decisions by the four routes, one cache for both blocks
        let mut cache = rdy_graph::GoverningCache::default();
        for (item, block) in [(ia, &a), (ib, &b)] {
            let (doc, headings) = headings_of(&m, block);
            let governing = crate::query::governing_ids(
                self.pack(),
                &m,
                &g,
                &block.id,
                rdy_graph::GoverningStatuses::Live,
                &mut cache,
            )
            .into_iter()
            .take(MAX_GOVERNING)
            .map(|d| CaseDecision {
                statement: m
                    .get(&d)
                    .and_then(|x| x.field_str("statement"))
                    .unwrap_or_default()
                    .to_string(),
                id: d.to_string(),
            })
            .collect();
            blocks.push(CaseBlock {
                id: block.id.to_string(),
                block_type: block.block_type.clone(),
                side: item.side.label(),
                version: item.version.to_string(),
                doc,
                headings,
                text: shown_text_in(self.pack(), block)
                    .into_iter()
                    .map(|(field, text)| CaseText { field, text })
                    .collect(),
                governing,
            });
        }
        Ok(CaseFile {
            request: label,
            kind: "tension".to_string(),
            rule: self.pair_rule(&ia.block, &ib.block)?,
            question: CASE_QUESTION.to_string(),
            blocks,
            entities: shared_entities(&m, &lex, &a, &b),
            schema: verdict_schema(),
        })
    }
}

/// The block `item` names: its change's write at the item's version, or canon's block at it.
fn block_of_item(cx: &DetectCx<'_>, item: &Item, label: &str) -> Result<Arc<Block>, RdyError> {
    let found = match &item.side {
        SideRef::Change(c) => cx
            .writes
            .on(&item.block)
            .iter()
            .find(|w| w.change == *c && w.version == Some(item.version))
            .and_then(|w| w.new.clone()),
        SideRef::Canon { .. } => {
            let canon = cx.canon_m()?;
            (canon.version_of(&item.block) == Some(item.version))
                .then(|| canon.get(&item.block).cloned().map(Arc::new))
                .flatten()
        }
        SideRef::Intent { .. } => None,
    };
    found.ok_or_else(|| {
        RdyError::new(
            E_REVIEW_STALE,
            format!(
                "request {label} is no longer current: block {} is not at version {} on side {}",
                item.block,
                item.version.short(),
                item.side.short()
            ),
            "call rdy_review_next again",
        )
    })
}

/// The path of `b`'s doc in `m` (`""` when it has none) and its headings: the doc's title, then
/// the title of each ancestor below the doc that has one, from the top down to `b`'s parent.
fn headings_of(m: &Materialized, b: &Block) -> (String, Vec<String>) {
    let mut titles = vec![];
    let mut doc = String::new();
    let mut cur = b.parent.as_ref().and_then(|p| m.get(p));
    let mut steps = 0;
    while let Some(x) = cur {
        steps += 1;
        if steps > m.len() + 1 {
            break;
        }
        let title = x.field_str("title").unwrap_or_default();
        if x.block_type == "doc" {
            titles.push(title.to_string());
            doc = m.path_of(&x.id).unwrap_or_default();
            break;
        }
        if !title.is_empty() {
            titles.push(title.to_string());
        }
        cur = x.parent.as_ref().and_then(|p| m.get(p));
    }
    titles.reverse();
    (doc, titles)
}

/// The entities both `a` and `b` name in their text, or both describe, by id, at most 10.
fn shared_entities(m: &Materialized, lex: &Lexicon, a: &Block, b: &Block) -> Vec<CaseEntity> {
    let named =
        |x: &Block| -> BTreeSet<String> { lex.mentions(x).into_iter().map(|n| n.entity).collect() };
    let described = |x: &Block| -> BTreeSet<String> {
        x.links
            .get(decision::DESCRIBES)
            .into_iter()
            .flatten()
            .filter(|l| {
                m.get(&l.target)
                    .is_some_and(|t| lex.is_entity_type(&t.block_type))
            })
            .map(|l| l.target.to_string())
            .collect()
    };
    let mut ids: BTreeSet<String> = named(a).intersection(&named(b)).cloned().collect();
    ids.extend(described(a).intersection(&described(b)).cloned());
    ids.into_iter()
        .filter_map(|id| {
            let e = m.get(&BlockId::parse(&id).ok()?)?;
            Some(CaseEntity {
                name: e.field_str("name").unwrap_or_default().to_string(),
                text: e.body.clone(),
                id,
            })
        })
        .take(MAX_ENTITIES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// guard: the schema names the verdict, the citations and the advice.
    #[test]
    fn se6_the_verdict_schema_names_the_advice() {
        let s = verdict_schema();
        for k in ["verdict", "citations", "severity", "reason"] {
            assert!(!s[k].is_null(), "{k}");
        }
        assert_eq!(s["verdict"], json!(["tension", "no_tension"]));
    }

    /// guard: the hash covers every field and is domain-separated.
    #[test]
    fn se6_the_case_hash_covers_the_case() {
        let case = CaseFile {
            request: "rq-000000000000".into(),
            kind: "tension".into(),
            rule: Some(Rule::Section),
            question: CASE_QUESTION.into(),
            blocks: vec![],
            entities: vec![],
            schema: verdict_schema(),
        };
        let h = case_hash(&case);
        assert_eq!(h, case_hash(&case.clone()));
        let other = CaseFile {
            rule: Some(Rule::Area),
            ..case.clone()
        };
        assert_ne!(h, case_hash(&other));
        let plain = Hash::of(&serde_json::to_vec(&serde_json::to_value(&case).unwrap()).unwrap());
        assert_ne!(h, plain);
    }
}
