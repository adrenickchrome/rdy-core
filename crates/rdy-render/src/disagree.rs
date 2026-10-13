//! The disagreements page's list (FW-SW2, D-QWSTV): each open tension of canon whose every side
//! is canon, with one reader sentence for its kind. The page shows no id, kind name, severity,
//! count, date or action, and a canon render shows only the published documentation, so an
//! in-flight change is never a side here.
use rdy_engine::tensions::{SideRef, TensionStatus};
use rdy_engine::{RdyError, ReadState, Scope};
use rdy_model::BlockId;
use std::collections::BTreeSet;

/// One entry of the page: what the reader is told, and the blocks it is about. For the two kinds
/// whose sentence says "this page" (a retired reference, a restated point that changed), `lead`
/// is the block of that page and its link comes first; for the others it is `None`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Disagreement {
    pub sentence: &'static str,
    pub blocks: Vec<BlockId>,
    pub lead: Option<BlockId>,
}

/// The reader sentence of a tension kind (by its wire name). The table is closed: a kind that
/// does not persist in canon (D-84QSN) has none, and its tensions are skipped.
pub fn sentence(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "fork" => "These pages give two versions of one point.",
        "duplicate_definition" => "These pages define the same thing twice.",
        "deprecated_reference" => "This page still points to something that was retired.",
        "value_disagreement" => "These pages state different values for the same thing.",
        "restatement_drift" => "This page restates a point that has since changed.",
        "semantic" => "A reviewer found that these statements contradict each other.",
        _ => return None,
    })
}

/// The disagreements of `scope`, for canon and for a tag at canon's head only. A tag at a past
/// landing and a view have none: the engine computes tensions for the current state only
/// (founder ruling 2026-10-12).
pub fn list(rdy: &ReadState, scope: &Scope) -> Result<Vec<Disagreement>, RdyError> {
    let current = match scope {
        Scope::Canon => true,
        Scope::At(h) => Some(*h) == rdy.canon_head(),
        _ => false,
    };
    if !current {
        return Ok(vec![]);
    }
    let mut out: Vec<Disagreement> = vec![];
    let mut seen: BTreeSet<Disagreement> = BTreeSet::new();
    // canon's tensions only: a canon render never detects the in-flight part (render_50k)
    for t in rdy.canon_tensions()?.iter() {
        let Some(said) = sentence(t.kind.name()) else {
            continue;
        };
        let all_canon =
            !t.sides.is_empty() && t.sides.iter().all(|s| matches!(s, SideRef::Canon { .. }));
        if !all_canon || rdy.tension_status(t) != TensionStatus::Open {
            continue;
        }
        let blocks = t.blocks();
        if blocks.is_empty() {
            continue;
        }
        let lead = lead_of(t.kind.name(), &t.summary, &blocks);
        let entry = Disagreement {
            sentence: said,
            blocks,
            lead,
        };
        if seen.insert(entry.clone()) {
            out.push(entry);
        }
    }
    Ok(out)
}

/// The block that "this page" names in the sentence of `kind`. The engine keeps a tension's
/// sides sorted, so the roles are read from the summary the detector wrote: a deprecated
/// reference opens with the pointing block ("{y} (who) references deprecated {x}"), and a
/// restatement drift names the restating block last ("{x} (who) changed text that {y} restates").
/// The other kinds have no such page. This reads the detectors' wording: if it changes, the lead
/// is lost and the entry falls back to reading order. An explicit side role on a tension would
/// remove the dependence.
fn lead_of(kind: &str, summary: &str, blocks: &[BlockId]) -> Option<BlockId> {
    match kind {
        "deprecated_reference" => blocks
            .iter()
            .find(|b| summary.starts_with(&format!("{b} ")))
            .cloned(),
        "restatement_drift" => blocks
            .iter()
            .find(|b| summary.contains(&format!(" that {b} restates")))
            .cloned(),
        _ => None,
    }
}

/// `blocks` without repeats, keeping the first of each.
pub fn distinct(blocks: impl IntoIterator<Item = BlockId>) -> Vec<BlockId> {
    let mut seen = BTreeSet::new();
    blocks
        .into_iter()
        .filter(|b| seen.insert(b.clone()))
        .collect()
}
