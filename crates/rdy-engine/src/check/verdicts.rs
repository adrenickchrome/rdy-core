//! Verdict facts and version holds (D-JQD5X; O13, O14, O16). A Check review answers one durable
//! request. The approval card of each version whose case it matches shows it as a fact line,
//! after every stored fact, and a person may accept a `concern` or `fail` there as blocking for
//! that version alone. Nothing here writes; the stored assessment never changes.
//!
//! # Matching (O13)
//!
//! [`matching_verdicts`] streams every Check review the log held at the state's captured end.
//! Each request's recipe is built again on the version's own selected net state, and the
//! complete case hash must equal the request's:
//! - a version subject builds the same recipe on this version;
//! - a change subject needs that change among the version's selected changes;
//! - a block subject needs a selected change that writes the block, or, for `prose_drift`, a
//!   written decision that supersedes, revokes or refines it.
//!
//! A case that changed or cannot be read now (code evidence unavailable) shows no line and no
//! acceptance option. Contradiction keeps its tension path and shows no line.
//!
//! # Holds (O14, O16)
//!
//! [`accepted_verdict_issues`] reads every AcceptVerdict answer at the captured end, never the
//! latest-answer ref, so two versions that accept one review keep both holds. An answer counts
//! when its structured option names its own review and this version's full hash. The review's
//! recorded case is classified with [`case_freshness`]: a matching or unavailable case keeps the
//! hold; only a changed case ends it. A new version never inherits a hold.
use super::case::CaseInputs;
use super::catalogue::AgentCheck;
use super::requests::{CaseFreshness, case_freshness, verdict_word};
use crate::gate::ProposalRef;
use crate::{E_LIMIT, E_VALIDATION, Materialized, RdyError, ReadState, Scope};
use rdy_model::check_request::{CheckMode, CheckRequest};
use rdy_model::decision::{DECISION, REFINES, REVOKES, SUPERSEDES};
use rdy_model::{BlockId, Change, Hash, Op, Proposal, Review, ReviewSubject, Verdict};
use rdy_schema::{Issue, Severity};
use rdy_store::{StoreError, kinds};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most verdict rows one approval card holds: the model's `MAX_ROWS`.
pub const MAX_VERDICT_ROWS: usize = rdy_model::assessment::MAX_ROWS;
/// The most UTF-8 text bytes the verdict rows of one approval card hold ([`MatchingVerdict::text_bytes`]).
pub const MAX_VERDICT_TEXT: usize = 16 * 1024 * 1024;
/// The id prefix of an acceptance option: `accept_verdict:<review>:<version>`.
pub const ACCEPT_PREFIX: &str = "accept_verdict:";
/// What choosing an acceptance option does.
pub const ACCEPT_CONSEQUENCE: &str = "This version cannot land while the check's case still holds.";
/// The fix of an accepted verdict's readiness issue.
pub const ACCEPTED_FIX: &str = "change the cited blocks, then propose again: rdy_write / rdy edit";
/// The fix of a card past its verdict limits.
pub const LIMIT_FIX: &str = "propose a smaller version: rdy_propose / rdy propose";
/// The item prefix of an AcceptVerdict answer: `verdict:<review>`.
pub const VERDICT_ITEM: &str = "verdict:";

/// One citation of a verdict, as the card's metadata binds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerdictCitation {
    pub block: String,
    pub note: String,
}

/// A Check review whose request's case matches a version now: the card's fact line and its
/// metadata row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MatchingVerdict {
    /// The request the review answers.
    #[serde(with = "rdy_store::transition::hex::one")]
    pub request: Hash,
    #[serde(with = "rdy_store::transition::hex::one")]
    pub review: Hash,
    /// The check's catalogue name.
    pub check: String,
    /// `author-run` or `independent`.
    pub mode: String,
    pub actor: String,
    pub run: Option<String>,
    /// `pass`, `concern` or `fail`.
    pub verdict: String,
    /// Empty for a pass with no reason.
    pub reason: String,
    pub citations: Vec<VerdictCitation>,
    /// The case hash the review recorded.
    #[serde(with = "rdy_store::transition::hex::one")]
    pub case_hash: Hash,
}

impl MatchingVerdict {
    /// The fact's name: the check's name with underscores as spaces.
    pub fn fact_name(&self) -> String {
        self.check.replace('_', " ")
    }

    /// The fact's value: `<verdict> (<actor>, <mode>): <reason>`, with no `: <reason>` for an
    /// empty reason.
    pub fn fact_value(&self) -> String {
        let head = format!("{} ({}, {})", self.verdict, self.actor, self.mode);
        if self.reason.is_empty() {
            head
        } else {
            format!("{head}: {}", self.reason)
        }
    }

    /// Whether a person may accept it as blocking: a `concern` or a `fail`.
    pub fn acceptable(&self) -> bool {
        self.verdict == "concern" || self.verdict == "fail"
    }

    /// The id of its acceptance option on version `version`'s card.
    pub fn accept_option(&self, version: &Hash) -> String {
        format!("{ACCEPT_PREFIX}{}:{version}", self.review)
    }

    /// The label of its acceptance option.
    pub fn accept_label(&self) -> String {
        format!("Accept {} {} as blocking", self.fact_name(), self.verdict)
    }

    /// The UTF-8 text bytes the row holds: every string field, citations included.
    pub fn text_bytes(&self) -> usize {
        let cited: usize = self
            .citations
            .iter()
            .map(|c| c.block.len() + c.note.len())
            .sum();
        self.check.len()
            + self.mode.len()
            + self.actor.len()
            + self.run.as_ref().map_or(0, String::len)
            + self.verdict.len()
            + self.reason.len()
            + cited
    }
}

/// The review and the version a structured acceptance option names, when `option` is exactly
/// `accept_verdict:<64 hex>:<64 hex>`.
pub fn parse_accept_option(option: &str) -> Option<(Hash, Hash)> {
    let rest = option.strip_prefix(ACCEPT_PREFIX)?;
    let (review, version) = rest.split_once(':')?;
    let full = |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !full(review) || !full(version) {
        return None;
    }
    Some((review.parse().ok()?, version.parse().ok()?))
}

/// The state one version's cases read: its base canon, the state it judges and its selected
/// changes, in the version's order. The judged state is the version's view while it exists,
/// else the version itself, as a version request's case reads it.
struct VersionState {
    before: Arc<Materialized>,
    after: Arc<Materialized>,
    changes: Vec<Change>,
    /// The selected changes' hashes, in the version's order.
    hashes: Vec<Hash>,
    /// The blocks the selected changes write.
    written: BTreeSet<BlockId>,
}

impl VersionState {
    fn read(st: &ReadState, pref: &ProposalRef, p: &Proposal) -> Result<VersionState, RdyError> {
        let after = if st.has_view(&p.view) {
            st.materialized(&Scope::View(p.view.clone()))?
        } else {
            st.materialized(&Scope::Proposal(pref.version))?
        };
        let before = match p.canon_at {
            Some(h) => st.materialized(&Scope::At(h))?,
            None => Arc::new(Materialized::default()),
        };
        let reader = st.reader();
        let mut changes = Vec::with_capacity(p.changes.len());
        for c in &p.changes {
            changes.push(reader.change(c)?);
        }
        let written = changes
            .iter()
            .flat_map(|c| c.ops.iter().map(|op| op.block_id().clone()))
            .collect();
        Ok(VersionState {
            before,
            after,
            changes,
            hashes: p.changes.clone(),
            written,
        })
    }

    /// Whether a selected change writes a decision that supersedes, revokes or refines `d`.
    fn relates(&self, d: &BlockId) -> bool {
        self.changes.iter().flat_map(|c| &c.ops).any(|op| match op {
            Op::Create { block } | Op::Update { block, .. } => {
                block.block_type == DECISION
                    && [SUPERSEDES, REVOKES, REFINES].iter().any(|name| {
                        block
                            .links
                            .get(*name)
                            .is_some_and(|ls| ls.iter().any(|l| &l.target == d))
                    })
            }
            Op::Delete { .. } => false,
        })
    }

    /// The request `r` as its recipe builds on this version, and the inputs it reads; `None`
    /// when `related` asks for a subject this version names and it names none (module docs).
    fn on_version<'a>(
        &'a self,
        st: &'a ReadState,
        pref: &ProposalRef,
        r: &CheckRequest,
        related: bool,
    ) -> Option<(CheckRequest, CaseInputs<'a>)> {
        let mut built = r.clone();
        if r.subject.version.is_some() {
            built.subject.version = Some(pref.version);
            built.subject.blocks.clear();
            return Some((
                built,
                CaseInputs {
                    st,
                    before: &self.before,
                    after: &self.after,
                    changes: &self.changes,
                },
            ));
        }
        if let Some(c) = &r.subject.change {
            let at = self.hashes.iter().position(|x| x == c)?;
            return Some((
                built,
                CaseInputs {
                    st,
                    before: &self.before,
                    after: &self.after,
                    changes: &self.changes[at..=at],
                },
            ));
        }
        if related {
            let prose = r.check == AgentCheck::ProseDrift.spec().name;
            let names = r
                .subject
                .blocks
                .iter()
                .any(|b| self.written.contains(b) || (prose && self.relates(b)));
            if !names {
                return None;
            }
        }
        Some((
            built,
            CaseInputs {
                st,
                before: &self.after,
                after: &self.after,
                changes: &[],
            },
        ))
    }
}

/// Whether the log held any check request at `st`'s captured end: with none it holds no Check
/// review and no AcceptVerdict answer either.
fn any_request(st: &ReadState) -> Result<bool, RdyError> {
    let reader = st.reader();
    let end = st.seq();
    let mut found = false;
    let walked = reader.for_each_object_of(kinds::CHECK_REQUEST, |h| {
        if reader.has_kind_before(kinds::CHECK_REQUEST, &h, end)? {
            found = true;
            return Err(StoreError::Refused(String::new()));
        }
        Ok(())
    });
    if found {
        return Ok(true);
    }
    walked.map_err(RdyError::from)?;
    Ok(false)
}

/// Runs `walk`, whose callback may fail with an engine error, as one engine result.
fn engine_walk<T>(
    walk: impl FnOnce(&mut dyn FnMut(Hash, T) -> Result<(), StoreError>) -> Result<(), StoreError>,
    mut f: impl FnMut(Hash, T) -> Result<(), RdyError>,
) -> Result<(), RdyError> {
    let mut failed = None;
    let walked = walk(&mut |h, v| match f(h, v) {
        Ok(()) => Ok(()),
        Err(e) => {
            failed = Some(e);
            Err(StoreError::Refused(String::new()))
        }
    });
    if let Some(e) = failed {
        return Err(e);
    }
    walked.map_err(RdyError::from)
}

/// The refusal of a card past its row limit.
pub(crate) fn too_many(pref: &ProposalRef) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("the approval card of {pref} matches more than {MAX_VERDICT_ROWS} check verdicts"),
        LIMIT_FIX,
    )
}

/// The refusal of a card past its text limit.
fn too_much_text(pref: &ProposalRef) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!(
            "the approval card of {pref} matches more than {MAX_VERDICT_TEXT} bytes of check verdict text"
        ),
        LIMIT_FIX,
    )
}

/// The catalogue position of check `name`; an unknown name sorts last.
fn rank(name: &str) -> Option<AgentCheck> {
    AgentCheck::parse(name)
}

/// The Check reviews whose request's case matches version `pref` (record `p`) now, in catalogue
/// order, then full review hash (module docs). At most [`MAX_VERDICT_ROWS`] rows and
/// [`MAX_VERDICT_TEXT`] bytes of text: one past is `E_LIMIT`, never a shorter list. A malformed
/// record is a read error.
pub fn matching_verdicts(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
) -> Result<Vec<MatchingVerdict>, RdyError> {
    if !any_request(st)? {
        return Ok(vec![]);
    }
    let reader = st.reader();
    let state = VersionState::read(st, pref, p)?;
    // one classification a request: copies of a review share their request's case
    let mut fits: BTreeMap<Hash, Option<CheckRequest>> = BTreeMap::new();
    let mut out: Vec<MatchingVerdict> = vec![];
    let mut text = 0usize;
    engine_walk(
        |f| reader.for_each_check_review(st.seq(), f),
        |h, r: Review| {
            let ReviewSubject::Check { request } = r.subject else {
                return Ok(());
            };
            if !matches!(r.verdict, Verdict::Pass | Verdict::Concern | Verdict::Fail) {
                return Ok(());
            }
            let fit = match fits.get(&request) {
                Some(fit) => fit.clone(),
                None => {
                    let q = reader.check_request(&request)?;
                    let fit = matching_request(st, &state, pref, q)?;
                    fits.insert(request, fit.clone());
                    fit
                }
            };
            let Some(q) = fit else {
                return Ok(());
            };
            let row = MatchingVerdict {
                request,
                review: h,
                check: q.check.clone(),
                mode: match q.mode {
                    CheckMode::Author => "author-run",
                    CheckMode::Independent => "independent",
                }
                .to_string(),
                actor: r.provenance.actor,
                run: r.provenance.run,
                verdict: verdict_word(r.verdict).to_string(),
                reason: r.reason,
                citations: r
                    .citations
                    .into_iter()
                    .map(|c| VerdictCitation {
                        block: c.block.to_string(),
                        note: c.note,
                    })
                    .collect(),
                case_hash: r.case_hash.unwrap_or(q.case_hash),
            };
            if out.len() >= MAX_VERDICT_ROWS {
                return Err(too_many(pref));
            }
            text = text.saturating_add(row.text_bytes());
            if text > MAX_VERDICT_TEXT {
                return Err(too_much_text(pref));
            }
            out.try_reserve(1).map_err(|_| too_many(pref))?;
            out.push(row);
            Ok(())
        },
    )?;
    out.sort_by(|a, b| {
        (rank(&a.check).is_none(), rank(&a.check), &a.review).cmp(&(
            rank(&b.check).is_none(),
            rank(&b.check),
            &b.review,
        ))
    });
    Ok(out)
}

/// Request `q` when its case matches this version now: a check other than contradiction whose
/// subject this version names and whose recipe rebuilds to the recorded hash on it.
fn matching_request(
    st: &ReadState,
    state: &VersionState,
    pref: &ProposalRef,
    q: CheckRequest,
) -> Result<Option<CheckRequest>, RdyError> {
    if q.check == AgentCheck::Contradiction.spec().name {
        return Ok(None);
    }
    let Some((built, inputs)) = state.on_version(st, pref, &q, true) else {
        return Ok(None);
    };
    Ok(match case_freshness(&built, inputs)? {
        CaseFreshness::Matching => Some(q),
        CaseFreshness::Changed | CaseFreshness::Unavailable => None,
    })
}

/// The readiness issues of the verdicts a person accepted as blocking for version `pref`
/// (record `p`): one `E_VALIDATION` issue for each accepted review whose case still holds or
/// cannot be read now, in catalogue order, then full review hash (module docs).
pub fn accepted_verdict_issues(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
) -> Result<Vec<Issue>, RdyError> {
    if !any_request(st)? {
        return Ok(vec![]);
    }
    let reader = st.reader();
    let mut accepted: BTreeSet<Hash> = BTreeSet::new();
    engine_walk(
        |f| reader.for_each_verdict_answer(st.seq(), f),
        |_, a: rdy_model::Answer| {
            let Some((review, version)) = a.option.as_deref().and_then(parse_accept_option) else {
                return Ok(());
            };
            if version == pref.version && a.item == format!("{VERDICT_ITEM}{review}") {
                accepted.insert(review);
            }
            Ok(())
        },
    )?;
    if accepted.is_empty() {
        return Ok(vec![]);
    }
    let state = VersionState::read(st, pref, p)?;
    let mut held: Vec<(Option<AgentCheck>, Hash, String)> = vec![];
    for review in accepted {
        let r = reader.review(&review)?;
        let ReviewSubject::Check { request } = r.subject else {
            continue;
        };
        let mut q = reader.check_request(&request)?;
        if let Some(case) = r.case_hash {
            q.case_hash = case;
        }
        let Some((built, inputs)) = state.on_version(st, pref, &q, false) else {
            continue;
        };
        match case_freshness(&built, inputs)? {
            CaseFreshness::Changed => {}
            CaseFreshness::Matching | CaseFreshness::Unavailable => {
                held.push((rank(&q.check), review, q.check));
            }
        }
    }
    held.sort_by_key(|a| (a.0.is_none(), a.0, a.1));
    Ok(held
        .into_iter()
        .map(|(_, review, check)| Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: None,
            message: format!("{check} verdict {review} is accepted as blocking for {pref}"),
            fix: ACCEPTED_FIX.into(),
        })
        .collect())
}
