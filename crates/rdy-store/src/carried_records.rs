//! Carried records (D-AYYD8, QD1): the stored shapes that have no writer yet. ASSESSMENT,
//! DISPUTE, DISPUTE_RESOLUTION and CHECK_REQUEST, a Check REVIEW and its three verdicts, the
//! Dispute, ResolveDispute and AcceptVerdict answers, a landing's `assessment` and `policy`, and a
//! tension plan item. The store, a dump, a sync batch and a take carry them byte for byte; no
//! product path writes one.
//!
//! The one rule set every reader of outside or stored bytes applies (the project's and the
//! holder's fsck, a checked stream and a take, a load and a reindex):
//!
//! - [`validate_local`]: a record's structure, from its own bytes;
//! - [`validate_refs`]: the kinds and bindings of the objects it names, through a
//!   [`CarriedSource`].
//!
//! A refusal is a closed reason; [`refusal`] gives its store error,
//! `record {hash} (kind {kind}): {reason}` (`StoreError::Decode`). Neither function computes a
//! readiness, an authorization or a dispute state.
use crate::codec::{self, ObjectBytes};
use crate::error::StoreError;
use crate::kinds;
use crate::object_refs::ObjectKey;
use rdy_model::assessment::Assessment;
use rdy_model::check_request::CheckRequest;
use rdy_model::dispute::{Dispute, DisputeResolution};
use rdy_model::plan::{Plan, PlanAct, PlanKind, outcome_allowed};
use rdy_model::{
    Answer, AnswerAct, Assurance, Block, Hash, LandingRecord, Review, ReviewSubject, Verdict,
};
use std::collections::HashSet;

/// A kind-None reference names an object of another kind than its field needs.
pub const WRONG_KIND: &str = "wrong reference kind";
/// A policy reference's block version is a version of another block.
pub const POLICY_BLOCK: &str = "policy block id differs";
/// A landing's assessment is of another proposal version than the one approved.
pub const ASSESSMENT_PROPOSAL: &str = "assessment proposal differs";
/// A landing's assessment base is no ancestor of the landing's predecessor.
pub const ASSESSMENT_BASE: &str = "assessment base is not an ancestor";
/// A Dispute or ResolveDispute answer's item is not its act's `<prefix>:<64 hex>`.
pub const DISPUTE_ITEM: &str = "invalid dispute answer item";
/// A Dispute or ResolveDispute answer's one target is not the record its item names.
pub const DISPUTE_TARGET: &str = "invalid dispute answer target";
/// An assessment's tension names a plan that holds no answer to the item.
pub const PLAN_ANSWER: &str = "assessment plan answer is missing";
/// A Dismiss plan answer answers a sealed item that is not a tension.
pub const DISMISS: &str = "dismiss is not a tension answer";
/// A sealed plan item has an outcome its kind does not allow.
pub const OUTCOME: &str = "outcome is not allowed for this plan kind";
/// A `check/` ref does not name the CHECK_REQUEST of its key.
pub const CHECK_REF: &str = "invalid check ref";
/// A review's verdict does not fit its subject.
pub const CHECK_VERDICT: &str = "invalid check review verdict";
/// A Check review's case is not its request's case.
pub const CHECK_CASE: &str = "check review case differs";
/// A Check review's reason is outside its bounds.
pub const CHECK_REASON: &str = "invalid check review reason";
/// A Check review's citations are outside their bounds.
pub const CHECK_CITATIONS: &str = "invalid check review citations";
/// A landing with a policy is not a policy landing with no relay.
pub const POLICY_FIELDS: &str = "policy landing fields differ";
/// An AcceptVerdict answer's item is not `verdict:<64 hex>`.
pub const VERDICT_ITEM: &str = "invalid verdict answer item";
/// An AcceptVerdict answer has targets.
pub const VERDICT_TARGETS: &str = "invalid verdict answer targets";
/// An AcceptVerdict answer names a review that is not a Concern or Fail check review.
pub const VERDICT_REVIEW: &str = "accepted verdict is not a concern or fail check review";

/// The item prefix of a Dispute answer: `dispute:<landing>`.
pub const DISPUTE_PREFIX: &str = "dispute:";
/// The item prefix of a ResolveDispute answer: `resolve:<dispute>`.
pub const RESOLVE_PREFIX: &str = "resolve:";
/// The item prefix of an AcceptVerdict answer: `verdict:<review>`.
pub const VERDICT_PREFIX: &str = "verdict:";
/// The ref prefix of a check request: `check/<request>`.
pub const CHECK_REF_PREFIX: &str = "check/";

/// The most citations of a Check review.
pub const MAX_CHECK_CITATIONS: usize = 20;
/// The most Unicode scalar values of a Check review's reason, after trimming.
pub const MAX_CHECK_REASON_CHARS: usize = 2_000;
/// The most bytes of a Check review's reason.
pub const MAX_CHECK_REASON_BYTES: usize = 8_000;

/// The hash `text` names after `prefix`: exactly 64 lower-case hex digits.
fn suffix(text: &str, prefix: &str) -> Option<Hash> {
    let hex = text.strip_prefix(prefix)?;
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex.parse().ok()
}

/// The REVIEW an AcceptVerdict answer's item `verdict:<hex>` names.
pub fn verdict_item(item: &str) -> Option<Hash> {
    suffix(item, VERDICT_PREFIX)
}

/// The LANDING a Dispute answer's item `dispute:<hex>` names.
pub fn dispute_item(item: &str) -> Option<Hash> {
    suffix(item, DISPUTE_PREFIX)
}

/// The DISPUTE a ResolveDispute answer's item `resolve:<hex>` names.
pub fn resolve_item(item: &str) -> Option<Hash> {
    suffix(item, RESOLVE_PREFIX)
}

/// The CHECK_REQUEST a `check/<hex>` ref names by its key; None for another ref.
pub fn check_ref_key(name: &str) -> Option<Hash> {
    suffix(name, CHECK_REF_PREFIX)
}

/// The store error of record `hash` of logical kind `kind` refused for `reason`.
pub fn refusal(hash: &Hash, kind: u8, reason: &str) -> StoreError {
    StoreError::Decode(format!("record {hash} (kind {kind}): {reason}"))
}

/// Whether the kind's records may hold a carried shape: the four carried kinds, and the kinds
/// whose records gained a carried field, act, subject or verdict.
pub fn may_carry(kind: u8) -> bool {
    matches!(
        kind,
        kinds::ASSESSMENT
            | kinds::DISPUTE
            | kinds::DISPUTE_RESOLUTION
            | kinds::CHECK_REQUEST
            | kinds::LANDING
            | kinds::ANSWER
            | kinds::REVIEW
            | kinds::PLAN
    )
}

fn decoded<T: serde::de::DeserializeOwned + 'static>(o: &ObjectBytes<'_>) -> Option<T> {
    codec::decode_bounded_split(o).ok()
}

/// The structure of the record of raw kind `raw` with stored payload `payload`, from its own
/// bytes: Task 3's record rules, a Check review's verdict, case, reason and citations, a
/// carried answer's item and targets, a policy landing's fields (also a landing field rule,
/// `crate::check::landing_field_rules`, through the same function) and a plan's tension rules. A
/// record that does not split or decode passes here: the decoders refuse it with their own
/// errors. Err is the closed reason.
pub fn validate_local(raw: u8, payload: &[u8]) -> Result<(), &'static str> {
    let Ok(o) = codec::split(raw, payload) else {
        return Ok(());
    };
    match o.kind {
        kinds::ASSESSMENT => match decoded::<Assessment>(&o) {
            Some(a) => a.validate(),
            None => Ok(()),
        },
        kinds::DISPUTE => match decoded::<Dispute>(&o) {
            Some(d) => d.validate(),
            None => Ok(()),
        },
        kinds::DISPUTE_RESOLUTION => match decoded::<DisputeResolution>(&o) {
            Some(r) => r.validate(),
            None => Ok(()),
        },
        kinds::CHECK_REQUEST => match decoded::<CheckRequest>(&o) {
            Some(r) => r.validate(),
            None => Ok(()),
        },
        kinds::REVIEW => match decoded::<Review>(&o) {
            Some(r) => review_local(&r),
            None => Ok(()),
        },
        kinds::ANSWER => match decoded::<Answer>(&o) {
            Some(a) => answer_local(&a),
            None => Ok(()),
        },
        kinds::LANDING => match decoded::<LandingRecord>(&o) {
            Some(l) => landing_local(&l),
            None => Ok(()),
        },
        kinds::PLAN => match crate::plan::decode_plan(o.body) {
            Ok(p) => plan_local(&p),
            Err(_) => Ok(()),
        },
        _ => Ok(()),
    }
}

/// O16: a Check review has verdict Pass, Concern or Fail and a case; Concern and Fail need 1 to
/// 20 citations and a reason of 1 to 2,000 trimmed scalar values and at most 8,000 bytes; Pass
/// takes 0 to 20 citations and an empty reason within the same upper bounds. The three verdicts
/// fit no other subject.
fn review_local(r: &Review) -> Result<(), &'static str> {
    let check_verdict = matches!(r.verdict, Verdict::Pass | Verdict::Concern | Verdict::Fail);
    let ReviewSubject::Check { .. } = r.subject else {
        return if check_verdict {
            Err(CHECK_VERDICT)
        } else {
            Ok(())
        };
    };
    if !check_verdict {
        return Err(CHECK_VERDICT);
    }
    if r.case_hash.is_none() {
        return Err(CHECK_CASE);
    }
    let chars = r.reason.trim().chars().count();
    let needs = r.verdict != Verdict::Pass;
    if chars > MAX_CHECK_REASON_CHARS
        || r.reason.len() > MAX_CHECK_REASON_BYTES
        || (needs && chars == 0)
    {
        return Err(CHECK_REASON);
    }
    if r.citations.len() > MAX_CHECK_CITATIONS || (needs && r.citations.is_empty()) {
        return Err(CHECK_CITATIONS);
    }
    Ok(())
}

/// A carried answer's item names its record by full hex, and its targets fit its act: one for
/// Dispute and ResolveDispute, none for AcceptVerdict (O17).
fn answer_local(a: &Answer) -> Result<(), &'static str> {
    match a.act {
        AnswerAct::Dispute | AnswerAct::ResolveDispute => {
            let item = if a.act == AnswerAct::Dispute {
                dispute_item(&a.item)
            } else {
                resolve_item(&a.item)
            };
            if item.is_none() {
                return Err(DISPUTE_ITEM);
            }
            if a.targets.len() != 1 {
                return Err(DISPUTE_TARGET);
            }
            Ok(())
        }
        AnswerAct::AcceptVerdict => {
            if verdict_item(&a.item).is_none() {
                return Err(VERDICT_ITEM);
            }
            if !a.targets.is_empty() {
                return Err(VERDICT_TARGETS);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A landing that names a policy is a policy landing with no relay. The reverse is not required
/// yet: an older policy-assurance landing has no policy field. One of the landing field rules
/// (`crate::check::landing_field_rules`), which every landing check applies.
pub(crate) fn landing_local(l: &LandingRecord) -> Result<(), &'static str> {
    if l.policy.is_some()
        && (l.assurance != Assurance::Policy
            || l.relayed_by.is_some()
            || l.relay_evidence.is_some())
    {
        return Err(POLICY_FIELDS);
    }
    Ok(())
}

/// A sealed item's outcome is one its kind allows (a tension item seals as updated, confirmed or
/// dismissed, and no other kind as dismissed), and a Dismiss answer of the plan answers only a
/// tension item it seals.
fn plan_local(p: &Plan) -> Result<(), &'static str> {
    let Some(seal) = &p.seal else {
        return Ok(());
    };
    if seal
        .items
        .iter()
        .any(|i| !outcome_allowed(i.kind, i.outcome))
    {
        return Err(OUTCOME);
    }
    let not_tension: HashSet<&Hash> = seal
        .items
        .iter()
        .filter(|i| i.kind != PlanKind::Tension)
        .map(|i| &i.id)
        .collect();
    if p.answers
        .iter()
        .any(|a| a.act == PlanAct::Dismiss && not_tension.contains(&a.item))
    {
        return Err(DISMISS);
    }
    Ok(())
}

/// Where [`validate_refs`] reads the objects a record names: the store, a stream with the
/// holder's records, or an upload being taken.
pub trait CarriedSource {
    /// The encoding and body of the present object `key`; None when it is not held, or held
    /// only as a removed placeholder.
    fn object(&mut self, key: ObjectKey) -> Result<Option<(u16, Vec<u8>)>, StoreError>;
}

fn read<T: serde::de::DeserializeOwned + 'static>(
    source: &mut dyn CarriedSource,
    key: ObjectKey,
) -> Result<Option<T>, StoreError> {
    Ok(source.object(key)?.and_then(|(version, body)| {
        codec::decode_bounded_split(&ObjectBytes {
            kind: key.kind,
            version,
            body: &body,
        })
        .ok()
    }))
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

/// Whether the BLOCK version `version` that `source` holds is a version of block `block`. An
/// object `source` does not hold is the reference checks' to report.
fn policy_block(
    source: &mut dyn CarriedSource,
    block: &rdy_model::BlockId,
    version: Hash,
) -> Result<bool, StoreError> {
    Ok(read::<Block>(source, key(kinds::BLOCK, version))?.is_none_or(|b| b.id == *block))
}

/// The kinds and bindings of the objects record `k` names, through `source`: a landing's policy
/// block and assessment (its proposal and base), an assessment's policy block and plan answers,
/// a carried answer's target and reviewed verdict, and a Check review's case. An object the
/// source does not hold passes here (the reference checks report a missing object). Ok(None)
/// when every binding holds; Ok(Some(reason)) names the first broken one.
pub fn validate_refs(
    source: &mut dyn CarriedSource,
    k: ObjectKey,
) -> Result<Option<&'static str>, StoreError> {
    match k.kind {
        kinds::LANDING => {
            let Some(l) = read::<LandingRecord>(source, k)? else {
                return Ok(None);
            };
            if let Some(p) = &l.policy
                && !policy_block(source, &p.block, p.version)?
            {
                return Ok(Some(POLICY_BLOCK));
            }
            if let Some(h) = l.assessment
                && let Some(a) = read::<Assessment>(source, key(kinds::ASSESSMENT, h))?
            {
                if a.proposal != l.approved_version.unwrap_or(l.proposal) {
                    return Ok(Some(ASSESSMENT_PROPOSAL));
                }
                if let Some(base) = a.base
                    && !ancestor(source, base, l.prev)?
                {
                    return Ok(Some(ASSESSMENT_BASE));
                }
            }
            Ok(None)
        }
        kinds::ASSESSMENT => {
            let Some(a) = read::<Assessment>(source, k)? else {
                return Ok(None);
            };
            if let Some(p) = a.policy.as_ref().and_then(|p| p.policy.as_ref())
                && !policy_block(source, &p.block, p.version)?
            {
                return Ok(Some(POLICY_BLOCK));
            }
            for t in &a.tensions {
                let Some(r) = &t.answer else {
                    continue;
                };
                if let Some(plan) = read::<Plan>(source, key(kinds::PLAN, r.plan))?
                    && !plan.answers.iter().any(|x| x.item == r.item)
                {
                    return Ok(Some(PLAN_ANSWER));
                }
            }
            Ok(None)
        }
        kinds::ANSWER => {
            let Some(a) = read::<Answer>(source, k)? else {
                return Ok(None);
            };
            answer_refs(source, &a)
        }
        kinds::REVIEW => {
            let Some(r) = read::<Review>(source, k)? else {
                return Ok(None);
            };
            let ReviewSubject::Check { request } = r.subject else {
                return Ok(None);
            };
            match read::<CheckRequest>(source, key(kinds::CHECK_REQUEST, request))? {
                Some(q) if r.case_hash != Some(q.case_hash) => Ok(Some(CHECK_CASE)),
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// The bindings of a carried answer: a Dispute's one target is the DISPUTE of its item's
/// landing, a ResolveDispute's the DISPUTE_RESOLUTION of its item's dispute, and an
/// AcceptVerdict's item names a Check review with verdict Concern or Fail.
fn answer_refs(
    source: &mut dyn CarriedSource,
    a: &Answer,
) -> Result<Option<&'static str>, StoreError> {
    match a.act {
        AnswerAct::Dispute => {
            let (Some(landing), [t]) = (dispute_item(&a.item), &a.targets[..]) else {
                return Ok(Some(DISPUTE_TARGET));
            };
            match read::<Dispute>(source, key(kinds::DISPUTE, *t))? {
                None => Ok(Some(WRONG_KIND)),
                Some(d) if d.landing != landing => Ok(Some(DISPUTE_TARGET)),
                Some(_) => Ok(None),
            }
        }
        AnswerAct::ResolveDispute => {
            let (Some(dispute), [t]) = (resolve_item(&a.item), &a.targets[..]) else {
                return Ok(Some(DISPUTE_TARGET));
            };
            match read::<DisputeResolution>(source, key(kinds::DISPUTE_RESOLUTION, *t))? {
                None => Ok(Some(WRONG_KIND)),
                Some(r) if r.dispute != dispute => Ok(Some(DISPUTE_TARGET)),
                Some(_) => Ok(None),
            }
        }
        AnswerAct::AcceptVerdict => {
            let Some(review) = verdict_item(&a.item) else {
                return Ok(Some(VERDICT_ITEM));
            };
            match read::<Review>(source, key(kinds::REVIEW, review))? {
                Some(r)
                    if matches!(r.subject, ReviewSubject::Check { .. })
                        && matches!(r.verdict, Verdict::Concern | Verdict::Fail) =>
                {
                    Ok(None)
                }
                Some(_) => Ok(Some(VERDICT_REVIEW)),
                None => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// Whether landing `base` is `from` or an ancestor of it through each landing's `prev`. A walk
/// that leaves what `source` holds ends there without a match.
fn ancestor(
    source: &mut dyn CarriedSource,
    base: Hash,
    from: Option<Hash>,
) -> Result<bool, StoreError> {
    let mut at = from;
    let mut seen: HashSet<Hash> = HashSet::new();
    while let Some(h) = at {
        if h == base {
            return Ok(true);
        }
        if !seen.insert(h) || seen.len() > crate::sync_stream::MAX_HISTORY_DEPTH as usize {
            return Ok(false);
        }
        at = match read::<LandingRecord>(source, key(kinds::LANDING, h))? {
            Some(l) => l.prev,
            None => None,
        };
    }
    Ok(false)
}

/// Both checks of record `k` from its own stored bytes and `source`: `validate_local`, then
/// `validate_refs`. The store error is [`refusal`]'s.
pub fn validate(
    source: &mut dyn CarriedSource,
    k: ObjectKey,
    raw: u8,
    payload: &[u8],
) -> Result<(), StoreError> {
    if !may_carry(k.kind) {
        return Ok(());
    }
    if let Err(reason) = validate_local(raw, payload) {
        return Err(refusal(&k.hash, k.kind, reason));
    }
    if let Some(reason) = validate_refs(source, k)? {
        return Err(refusal(&k.hash, k.kind, reason));
    }
    Ok(())
}

/// A [`CarriedSource`] over a store reader: its held, present objects.
pub struct StoreObjects<'a>(pub &'a crate::store::StoreReader);

impl CarriedSource for StoreObjects<'_> {
    fn object(&mut self, key: ObjectKey) -> Result<Option<(u16, Vec<u8>)>, StoreError> {
        match self.0.get_versioned_body(key.kind, &key.hash) {
            Ok(b) => Ok(Some(b)),
            Err(StoreError::NotFound(_) | StoreError::Redacted { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
