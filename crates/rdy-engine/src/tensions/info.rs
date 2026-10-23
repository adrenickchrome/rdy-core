//! What a surface shows of a tension: its id and label, kind, severity, status, sides (with each
//! side's views and provenance) and blocks.
use super::semantic::pair_of;
use super::{SideRef, Tension, TensionKind, TensionSeverity, tension_label};
use crate::ReadState;
use crate::coord_graph::intent_label;
use crate::history::role_str;
use crate::session::rfc3339;
use rdy_model::{BlockId, Hash, Provenance};
use serde::Serialize;
use std::cell::Cell;
use std::collections::HashMap;

thread_local! {
    /// This thread's counts: tension rows serialized, and rows built with their sides.
    static COUNTS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    /// This thread's count of tension rows built without their sides (`tension_info_bare`).
    static BARE_ROWS: Cell<u64> = const { Cell::new(0) };
}

/// This thread's count so far of tension rows built without their sides
/// (`ReadState::tension_info_bare`). A test reads the difference over a briefing to check that
/// it builds the rows its page can show, not every row that involves its blocks.
#[doc(hidden)]
pub fn bare_rows_built() -> u64 {
    BARE_ROWS.with(Cell::get)
}

/// This thread's counts so far: how many tension rows were serialized (whole or inside a
/// reply), and how many were built with their sides. A test reads the difference over a call
/// to check that a budget cut costs what the rows it keeps cost, never the square of the rows.
#[doc(hidden)]
pub fn info_counts() -> (u64, u64) {
    COUNTS.with(Cell::get)
}

fn count(serialized: u64, sided: u64) {
    COUNTS.with(|c| {
        let (s, b) = c.get();
        c.set((s + serialized, b + sided));
    });
}

/// The JSON length of `row`, measured once (and counted, see `info_counts`).
pub(crate) fn row_bytes(row: &TensionInfo) -> usize {
    serde_json::to_string(row)
        .expect("a tension row serializes: strings and string-keyed maps only")
        .len()
}

/// The bytes `n` rows of `total` bytes take as a JSON array's elements: a comma after each but
/// the last.
pub(crate) fn rows_bytes(total: usize, n: usize) -> usize {
    total + n.saturating_sub(1)
}

/// Serializes a row's `id`, counting the row (see `info_counts`).
fn counted_id<S: serde::Serializer>(id: &str, s: S) -> Result<S::Ok, S::Error> {
    count(1, 0);
    s.serialize_str(id)
}

/// What a list of tension rows reads once and shares: each change's provenance and each canon
/// item's writers. Changes are content-addressed, so a cache is never stale; it lives for one
/// list (one call), so it never grows past the rows it serves.
#[derive(Default)]
pub struct InfoCache {
    provenance: HashMap<Hash, Option<Provenance>>,
    writers: HashMap<(BlockId, Option<Hash>), Vec<Hash>>,
}

impl InfoCache {
    /// The provenance of change `c`, read from the store on the first ask.
    fn provenance(&mut self, st: &ReadState, c: &Hash) -> Option<Provenance> {
        self.provenance
            .entry(*c)
            .or_insert_with(|| st.reader().change(c).ok().map(|ch| ch.provenance))
            .clone()
    }

    /// The changes that wrote canon item `block` at `version`, read on the first ask.
    fn writers(&mut self, st: &ReadState, block: &BlockId, version: Option<Hash>) -> Vec<Hash> {
        self.writers
            .entry((block.clone(), version))
            .or_insert_with(|| st.canon_item_writers(block, version).unwrap_or_default())
            .clone()
    }
}

/// Whether a tension is open or dismissed (by the review named).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TensionStatus {
    Open,
    Dismissed(Hash),
}

/// A tension as the surfaces list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TensionInfo {
    /// `tn-<12 hex>`.
    #[serde(serialize_with = "counted_id")]
    pub id: String,
    /// The full id, 64 hex.
    pub tension: String,
    pub kind: TensionKind,
    pub severity: TensionSeverity,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub overridden: bool,
    /// `open` or `dismissed`.
    pub status: String,
    pub summary: String,
    pub sides: Vec<SideInfo>,
    pub blocks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissal: Option<DismissalInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refuses: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_time: bool,
    /// A landing-only form (`rdy_tensions`): the open proposal (`P-<stable short>@<n>`) whose
    /// landing it describes. The gate judges this form; the in-flight set does not hold it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    /// A semantic tension's verdict: who judged the pair, their citations and advice, and a
    /// person's acceptance of the advice (D-WMCFW).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judged: Option<JudgedInfo>,
}

/// One side of a tension: its label, what it is (a change, a landing, an intent), the open views
/// that hold it, who wrote it, and the blocks it involves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SideInfo {
    pub side: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub views: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// A canon side's authors: every actor who wrote one of its items (the dismissal rules
    /// read them all), sorted. Empty for a change or an intent side, whose author is `actor`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    pub blocks: Vec<SideBlock>,
}

/// A block a side involves, at its version (`None`: deleted or absent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SideBlock {
    pub block: String,
    pub version: Option<String>,
}

/// The review that dismissed a tension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DismissalInfo {
    pub review: String,
    pub actor: String,
    pub run: Option<String>,
    pub role: String,
    pub at: String,
    pub at_ms: i64,
    pub reason: String,
}

/// The verdict behind a semantic tension, and what became of its advice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JudgedInfo {
    /// The review whose `tension` verdict opened the tension.
    pub review: String,
    /// The reviewer.
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub citations: Vec<CitationInfo>,
    /// The reviewer's reason that the tension should block, when the verdict advised it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advice: Option<String>,
    /// A person's acceptance of the advice: the tension blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<AcceptanceInfo>,
}

/// A reviewer's note on one block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CitationInfo {
    pub block: String,
    pub note: String,
}

/// Who accepted a reviewer's advice: the person, the agent that relayed their words, the words,
/// the question and where they said it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AcceptanceInfo {
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// `actor[/run]` of the agent that relayed the answer at the command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    /// RFC 3339 UTC.
    pub at: String,
}

impl ReadState {
    /// Whether `t` is open or dismissed in this state. A dismissal binds to the vkey, which
    /// covers the content and the severity: a dismissal made while `t` was advisory does not
    /// hold once it is blocking.
    pub fn tension_status(&self, t: &Tension) -> TensionStatus {
        match self.dismissals.get(&t.vkey) {
            Some(review) => TensionStatus::Dismissed(*review),
            None => TensionStatus::Open,
        }
    }

    /// `t` as the surfaces list it. A change side's views are the open views that hold it and
    /// its actor and run the change's; a canon side's `authors` are every actor who wrote one of
    /// its items (as the dismissal rules read them), and its actor and run those of its first
    /// item's writer; an intent side's are the intent's. A dismissed tension carries its
    /// review. What cannot be read is left out. One row: a list of rows shares an
    /// [`InfoCache`] through [`ReadState::tension_info_with`].
    pub fn tension_info(&self, t: &Tension) -> TensionInfo {
        self.tension_info_with(t, &mut InfoCache::default())
    }

    /// `tension_info`, reading each change's provenance and each canon item's writers once
    /// per `cache`: a change is read whole (its ops too), so a list of rows that share a change
    /// reads it once, not once a row.
    pub fn tension_info_with(&self, t: &Tension, cache: &mut InfoCache) -> TensionInfo {
        let mut info = self.tension_info_bare(t);
        info.sides = self.sides_info(t, cache);
        info.judged = self.judged_info(t);
        info
    }

    /// The sides of `t` as `tension_info` lists them.
    pub(crate) fn sides_info(&self, t: &Tension, cache: &mut InfoCache) -> Vec<SideInfo> {
        count(0, 1);
        t.sides
            .iter()
            .map(|s| self.side_info(t, s, cache))
            .collect()
    }

    /// `tension_info` without the sides (`sides` empty): what a budget cut that drops the
    /// detail keeps. It reads no change.
    pub fn tension_info_bare(&self, t: &Tension) -> TensionInfo {
        BARE_ROWS.with(|c| c.set(c.get() + 1));
        let (status, dismissal) = match self.tension_status(t) {
            TensionStatus::Open => ("open", None),
            TensionStatus::Dismissed(review) => ("dismissed", self.dismissal_info(&review)),
        };
        TensionInfo {
            id: tension_label(&t.id),
            tension: t.id.to_string(),
            kind: t.kind,
            severity: t.severity,
            overridden: t.overridden,
            status: status.to_string(),
            summary: t.summary.clone(),
            sides: vec![],
            blocks: t.blocks().iter().map(ToString::to_string).collect(),
            dismissal,
            refuses: None,
            read_time: t.read_time,
            proposal: None,
            judged: None,
        }
    }

    /// The verdict behind `t` when `t` is a semantic tension whose pair has an open record in
    /// the semantic state: the review's reviewer and citations, the record's advice, and the
    /// acceptance when the record's latest version is one. `None` when the review cannot be read.
    pub(crate) fn judged_info(&self, t: &Tension) -> Option<JudgedInfo> {
        let key = pair_of(t)?;
        let open = self.semantic().tension(&key)?;
        let rec = &open.record;
        let r = self.reader().review(&rec.review).ok()?;
        let accepted = rec.blocking.then(|| {
            let p = &rec.provenance;
            let relay = rec.relay.as_ref();
            AcceptanceInfo {
                actor: p.actor.clone(),
                run: p.run.clone(),
                relayed_by: rec.relayed_by.as_ref().map(|b| match &b.run {
                    Some(run) => format!("{}/{run}", b.actor),
                    None => b.actor.clone(),
                }),
                evidence: relay.map(|e| e.quoted.clone()),
                question: relay.and_then(|e| e.question.clone()),
                session_ref: relay.map(|e| e.session_ref.clone()),
                at: rfc3339(p.at_ms),
            }
        });
        Some(JudgedInfo {
            review: rec.review.to_string(),
            actor: r.provenance.actor,
            run: r.provenance.run,
            citations: r
                .citations
                .into_iter()
                .map(|f| CitationInfo {
                    block: f.block.to_string(),
                    note: f.note,
                })
                .collect(),
            advice: rec.advice.clone(),
            accepted,
        })
    }

    /// The dismissal review `review` as the surfaces show it, if it can be read.
    pub(crate) fn dismissal_info(&self, review: &Hash) -> Option<DismissalInfo> {
        let r = self.reader().review(review).ok()?;
        let p = r.provenance;
        Some(DismissalInfo {
            review: review.to_string(),
            actor: p.actor,
            run: p.run,
            role: role_str(p.role).to_string(),
            at: rfc3339(p.at_ms),
            at_ms: p.at_ms,
            reason: r.reason,
        })
    }

    fn side_info(&self, t: &Tension, side: &SideRef, cache: &mut InfoCache) -> SideInfo {
        let items: Vec<_> = t.involved.iter().filter(|i| i.side == *side).collect();
        let mut info = SideInfo {
            side: side.label(),
            change: None,
            landing: None,
            intent: None,
            views: vec![],
            actor: None,
            run: None,
            authors: vec![],
            blocks: items
                .iter()
                .map(|i| SideBlock {
                    block: i.block.to_string(),
                    version: i.version.map(|v| v.to_string()),
                })
                .collect(),
        };
        let reader = self.reader();
        match side {
            SideRef::Change(c) => {
                info.change = Some(c.to_string());
                if let Ok(set) = self.inflight_set() {
                    info.views = set.views_of.get(c).cloned().unwrap_or_default();
                }
                if let Some(p) = cache.provenance(self, c) {
                    (info.actor, info.run) = (Some(p.actor), p.run);
                }
            }
            SideRef::Canon { landing } => {
                info.landing = landing.map(|l| l.to_string());
                let mut authors = std::collections::BTreeSet::new();
                for i in &items {
                    let writers = cache.writers(self, &i.block, i.version);
                    for p in writers.iter().filter_map(|c| cache.provenance(self, c)) {
                        if info.actor.is_none() {
                            (info.actor, info.run) = (Some(p.actor.clone()), p.run);
                        }
                        authors.insert(p.actor);
                    }
                }
                info.authors = authors.into_iter().collect();
            }
            SideRef::Intent { stable, version } => {
                info.intent = Some(intent_label(stable));
                if let Ok(it) = reader.intent(version) {
                    info.views = vec![it.view];
                    (info.actor, info.run) = (Some(it.actor), it.run);
                }
            }
        }
        info
    }
}
