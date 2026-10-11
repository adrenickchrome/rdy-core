//! Confidence (FW-CV2, D-XZXCE, D-EEN7A): every assertion rdy gives an agent carries a level and
//! its reasons, drawn from the coverage issues of the answer's neighbourhood. It never blocks.
use super::model::{CoverageIssue, CoverageIssues, IssueCode, sort_issues};
use crate::lexicon::is_tie;
use crate::{RdyError, ReadState, Scope};
use rdy_model::BlockId;
use serde::Serialize;
use std::collections::BTreeSet;

/// The most reasons a confidence lists; the rest are counted in `reasons_omitted`.
pub const REASONS_CAP: usize = 5;

/// How far rdy vouches for an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    High,
    Medium,
    Low,
}

/// A level and the coverage issues that explain it: first those that set it, then the others,
/// at most [`REASONS_CAP`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Confidence {
    pub confidence: Level,
    pub reasons: Vec<CoverageIssue>,
    #[serde(skip_serializing_if = "is_zero")]
    pub reasons_omitted: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The confidence of an answer drawn from `neighbourhood`. Low when a block of the neighbourhood
/// has an open tension or an open suspect, or when `hit` is given and has an `untied_content`
/// issue (text with no entity behind it). Medium when any other issue is on the neighbourhood.
/// High when none is.
pub fn confidence_of(
    issues: &CoverageIssues,
    neighbourhood: &BTreeSet<BlockId>,
    hit: Option<&BlockId>,
) -> Confidence {
    let sets_low = |i: &CoverageIssue| match i.code {
        IssueCode::OpenTension | IssueCode::OpenSuspect => true,
        IssueCode::UntiedContent => hit.is_some_and(|h| i.block == h.to_string()),
        _ => false,
    };
    let mut all: Vec<CoverageIssue> = Vec::new();
    for b in neighbourhood {
        if let Some(e) = issues.blocks.get(b) {
            all.extend(e.issues.iter().cloned());
        }
    }
    // a hit outside its own neighbourhood still counts for its text
    if let Some(h) = hit.filter(|h| !neighbourhood.contains(*h))
        && let Some(e) = issues.blocks.get(h)
    {
        all.extend(
            e.issues
                .iter()
                .filter(|i| i.code == IssueCode::UntiedContent)
                .cloned(),
        );
    }
    let (mut first, mut rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|i| sets_low(i));
    let level = if !first.is_empty() {
        Level::Low
    } else if !rest.is_empty() {
        Level::Medium
    } else {
        Level::High
    };
    let no_paths = Default::default();
    sort_issues(&mut first, &no_paths);
    sort_issues(&mut rest, &no_paths);
    first.extend(rest);
    let reasons_omitted = first.len().saturating_sub(REASONS_CAP);
    first.truncate(REASONS_CAP);
    Confidence {
        confidence: level,
        reasons: first,
        reasons_omitted,
    }
}

impl ReadState {
    /// The neighbourhood of a fact sheet: the entity, every block with a tie edge to it, and
    /// every block whose text names it, linked or not.
    pub fn fact_sheet_neighbourhood(
        &self,
        scope: &Scope,
        entity: &BlockId,
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        self.block(scope, entity)?;
        let g = self.graph(scope)?;
        let mut out: BTreeSet<BlockId> = BTreeSet::new();
        out.insert(entity.clone());
        for e in g.incoming(entity) {
            if is_tie(&e.kind) {
                out.insert(e.from.clone());
            }
        }
        let issues = self.coverage_issues(scope)?;
        if let Some(named) = issues.mentions.get(entity) {
            out.extend(named.iter().cloned());
        }
        Ok(out)
    }

    /// The neighbourhood of a search hit: the block and every entity block it has a tie edge to.
    pub fn hit_neighbourhood(
        &self,
        scope: &Scope,
        block: &BlockId,
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        let lex = self.lexicon(scope)?;
        let mut out: BTreeSet<BlockId> = BTreeSet::new();
        out.insert(block.clone());
        for e in g.outgoing(block) {
            if is_tie(&e.kind)
                && m.get(&e.to)
                    .is_some_and(|t| lex.is_entity_type(&t.block_type))
            {
                out.insert(e.to.clone());
            }
        }
        Ok(out)
    }
}
