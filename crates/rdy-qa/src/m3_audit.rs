//! The M3 invariants (FW-G7 D-2GT0Z; D-ME2HD, D-HAG73, D-ZWP7Q, D-QBNAQ, D-4JMPH, D-45RG6): what
//! the stored M3 records of each landing must hold, and what a landing observed between two held
//! read snapshots must have met.
//!
//! The audit is independent of readiness and of the gate. It reads stored records into an
//! immutable fact projection ([`M3Facts`], [`TemporalFacts`]) and checks the projection with its
//! own rules. It never calls readiness, the gate or the approval queue. A test builds a negative
//! witness by changing a copy of a projection, never the log.
//!
//! [`check_m3`] is the durable checker. It reads canon's landings and checks the records and
//! links that history holds. It does not require an assessment of a landing that records none
//! (a pre-M3 landing, a released fixture): only a landing that records an assessment, a policy or
//! `assurance: policy`, or the landing of a re-affirmation card, is an M3 landing. `check_all`
//! runs it.
//!
//! [`audit_m3`] is the temporal audit of one landing, run explicitly at each landing. The
//! `before` snapshot supplies canon, the pack, the gate config, the live policy and the person
//! acts recorded before the landing. The `after` snapshot supplies the acknowledged landing and
//! its batch. The caller's journal supplies what the person saw ([`Shown`]). It also runs the
//! durable rules on that landing and the existing landing audit (`landing_audit::audit`).
//!
//! The rules (closed): `m3_briefing`, `m3_version`, `m3_assessment`, `m3_plan`, `m3_route`
//! (durable); `m3_snapshot`, `m3_fresh`, `m3_hold`, `m3_card` (temporal); `m3_read` (a record the
//! audit needs cannot be read).
//!
//! Limits of the projection (it under-reads rather than over-reads, so it never flags a landing
//! the built rules allow):
//! - The stable-content test of `m3_briefing` reads updates and deletes of an existing block and
//!   creates under an existing parent, each judged on the canon the change was written on plus
//!   the version's earlier changes, in list order; presentational blocks are skipped.
//! - `m3_plan` reads answer lapse on the subject only, for subjects that are not decisions.
//! - `m3_hold` counts an accepted check verdict for the version as a hold without judging whether
//!   its case still holds; a stored briefing link proves only that a briefing was recorded, never
//!   that it was current (the write-time tests and the swarm journal prove that).
use crate::invariants::Violation;
use crate::landing_audit::{Landed, audit};
use rdy_engine::check::verdicts::{VERDICT_ITEM, parse_accept_option};
use rdy_engine::readiness::assessment::{config_digest, pack_digest};
use rdy_engine::readiness::check::MAX_REPLAY_STEPS;
use rdy_engine::{Materialized, ReadState, Scope};
use rdy_model::assessment::{
    Assessment, AssessmentForeign, AssessmentSeverity, AssessmentTensionStatus, AssessmentV2,
    PolicyRef, PolicyVerdict,
};
use rdy_model::plan::{Plan, PlanAct, PlanKind, PlanOutcome};
use rdy_model::{
    AnswerAct, Assurance, Block, BlockId, Change, EvidenceKind, FieldValue, Hash, LandingRecord,
    Op, Proposal, decision,
};
use rdy_store::segment::Location;
use rdy_store::{REAFFIRM_VIEW_PREFIX, StoreError, StoreReader, kinds};
use std::collections::{BTreeMap, BTreeSet};

/// The rule a record that cannot be read breaks.
pub const READ: &str = "m3_read";

fn v(rule: &'static str, block: Option<&BlockId>, detail: impl Into<String>) -> Violation {
    Violation {
        rule,
        block: block.cloned(),
        detail: detail.into(),
    }
}

fn read_err(what: impl std::fmt::Display, e: impl std::fmt::Display) -> Violation {
    v(READ, None, format!("cannot read {what}: {e}"))
}

/// One source on a replay chain: the change, whether `rdy import` wrote it, and its place in
/// the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFact {
    pub change: Hash,
    pub imported: bool,
    pub at: Location,
}

/// One change of an approved version, as the briefing rule reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeFact {
    pub id: Hash,
    pub at: Location,
    /// The briefing the change records (`links.briefing`).
    pub briefing: Option<Hash>,
    /// `rdy import` wrote it (`links.imported_by`).
    pub imported: bool,
    /// It is a replay (`links.replayed_from`).
    pub replay: bool,
    /// Its replay chain's sources, nearest first: at most [`MAX_REPLAY_STEPS`] + 1, ended early
    /// by a source the store does not hold, a source with no `replayed_from`, or a repeat.
    pub sources: Vec<SourceFact>,
    /// The stable blocks it touches, on the state it was written on.
    pub stable: Vec<BlockId>,
}

/// A plan item's subject in the approved version's prospective canon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubjectFact {
    /// Its effective lifecycle is `stable`.
    pub stable: bool,
    /// Its content hash with the store's tracking fields (what a plan answer binds); None when
    /// the version leaves no such block.
    pub content: Option<Hash>,
    /// It is a decision (the acceptance changes it, so lapse is not read from it).
    pub decision: bool,
}

/// The seal a landing wrote, with what its rules read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealFact {
    pub plan: Hash,
    pub record: Plan,
    /// The act of each ANSWER a sealed item names.
    pub answer_acts: BTreeMap<Hash, AnswerAct>,
    /// Each block subject of a sealed item, by its text.
    pub subjects: BTreeMap<String, SubjectFact>,
}

/// One landing of canon's chain, as the durable rules read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandingFact {
    pub landing: Hash,
    pub record: LandingRecord,
    pub at: Location,
    /// The approved version's view.
    pub view: String,
    /// The approved version, when the landing names one the store holds.
    pub approved: Option<Proposal>,
    /// The landing of a re-affirmation card (its view starts with `reaffirm-`): the stated
    /// landing with no assessment.
    pub reaffirm_card: bool,
    /// The assessment the landing names, decoded (its local part).
    pub assessment: Option<Assessment>,
    /// The named assessment's foreign part (repo links, ASSESSMENT encoding 2). The binding
    /// rule re-encodes [`Self::assessment`] with it ([`assessment_wire_id`]), so a changed
    /// local field still changes the id.
    pub assessment_foreign: Option<AssessmentForeign>,
    /// The newest assessment of the approved version whose frame lies before the landing.
    pub bound_assessment: Option<Hash>,
    /// The approved version's changes.
    pub changes: Vec<ChangeFact>,
    /// The seal of the landing.
    pub seal: Option<SealFact>,
}

impl LandingFact {
    /// Whether the M3 rules apply: the landing records an assessment, a policy or
    /// `assurance: policy`, or lands a re-affirmation card.
    pub fn is_m3(&self) -> bool {
        self.record.assessment.is_some()
            || self.record.policy.is_some()
            || self.record.assurance == Assurance::Policy
            || self.reaffirm_card
    }
}

/// The durable projection: the M3 landings of canon's chain, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M3Facts {
    /// The log's first change that records a briefing (exemption (c)).
    pub first_briefed: Option<Location>,
    pub landings: Vec<LandingFact>,
}

impl M3Facts {
    /// The projection of `st`'s canon chain.
    pub fn read(st: &ReadState) -> Result<M3Facts, Violation> {
        let reader = st.reader();
        let first_briefed = reader
            .first_briefed_change()
            .map_err(|e| read_err("the first briefed change", e))?;
        let mut chain = chain_of(reader, st.canon_head())?;
        chain.reverse();
        let mut landings = vec![];
        for (h, rec) in chain {
            if let Some(f) = LandingFact::read(st, h, rec)? {
                landings.push(f);
            }
        }
        Ok(M3Facts {
            first_briefed,
            landings,
        })
    }

    /// The fact of landing `h`.
    pub fn landing(&self, h: &Hash) -> Option<&LandingFact> {
        self.landings.iter().find(|l| l.landing == *h)
    }

    /// The fact of landing `h`, to change in a copy.
    pub fn landing_mut(&mut self, h: &Hash) -> Option<&mut LandingFact> {
        self.landings.iter_mut().find(|l| l.landing == *h)
    }
}

impl LandingFact {
    /// The fact of landing `h` (record `rec`) of `st`'s store; None for a landing the M3 rules
    /// do not cover.
    pub fn read(
        st: &ReadState,
        h: Hash,
        record: LandingRecord,
    ) -> Result<Option<LandingFact>, Violation> {
        let fact = Self::head(st, h, record)?;
        if !fact.is_m3() {
            return Ok(None);
        }
        fact.filled(st).map(Some)
    }

    /// The whole fact of landing `h` (record `rec`), whether or not the M3 rules cover it.
    pub fn read_whole(
        st: &ReadState,
        h: Hash,
        record: LandingRecord,
    ) -> Result<LandingFact, Violation> {
        Self::head(st, h, record)?.filled(st)
    }

    /// The landing's record, place, approved version and view.
    fn head(st: &ReadState, h: Hash, record: LandingRecord) -> Result<LandingFact, Violation> {
        let reader = st.reader();
        let at = reader
            .landing_location(&h)
            .map_err(|e| read_err(format!("landing {}", h.short()), e))?;
        let approved = match record.approved_version {
            Some(a) => Some(
                reader
                    .proposal(&a)
                    .map_err(|e| read_err(format!("proposal {}", a.short()), e))?,
            ),
            None => None,
        };
        let view = match &approved {
            Some(p) => p.view.clone(),
            None => {
                reader
                    .proposal(&record.proposal)
                    .map_err(|e| read_err(format!("proposal {}", record.proposal.short()), e))?
                    .view
            }
        };
        Ok(LandingFact {
            landing: h,
            at,
            reaffirm_card: view.starts_with(REAFFIRM_VIEW_PREFIX),
            view,
            approved,
            assessment: None,
            assessment_foreign: None,
            bound_assessment: None,
            changes: vec![],
            seal: None,
            record,
        })
    }

    /// The fact with its assessment, changes and seal read.
    fn filled(mut self, st: &ReadState) -> Result<LandingFact, Violation> {
        let reader = st.reader();
        let fact = &mut self;
        let at = fact.at;
        if let Some(a) = fact.record.assessment {
            let wire = reader
                .assessment_wire(&a)
                .map_err(|e| read_err(format!("assessment {}", a.short()), e))?;
            fact.assessment_foreign = Some(wire.foreign.clone());
            fact.assessment = Some(wire.local());
        }
        if let Some(version) = fact.record.approved_version {
            fact.bound_assessment = reader
                .assessment_of_proposal(&version, at)
                .map_err(|e| read_err(format!("the assessment of {}", version.short()), e))?;
        }
        if let Some(p) = &fact.approved {
            fact.changes = change_facts(st, p)?;
            fact.seal = seal_fact(st, fact)?;
        }
        Ok(self)
    }
}

/// Canon's chain from `head` back, newest first: each landing and its record. A snapshot reads
/// its own chain this way, never the store's newest.
fn chain_of(
    reader: &StoreReader,
    head: Option<Hash>,
) -> Result<Vec<(Hash, LandingRecord)>, Violation> {
    let mut out: Vec<(Hash, LandingRecord)> = vec![];
    let mut next = head;
    while let Some(h) = next {
        if out.iter().any(|(l, _)| *l == h) {
            return Err(v(
                READ,
                None,
                format!("canon's chain loops at {}", h.short()),
            ));
        }
        let rec = reader
            .landing(&h)
            .map_err(|e| read_err(format!("landing {}", h.short()), e))?;
        next = rec.prev;
        out.push((h, rec));
    }
    Ok(out)
}

/// The `kinds::ASSESSMENT` id of `a`'s encoding now: the hash its bytes would have if written
/// today.
pub fn assessment_id(a: &Assessment) -> Hash {
    Hash::of(&rdy_store::codec::item_new(kinds::ASSESSMENT, a).1)
}

/// The `kinds::ASSESSMENT` id of the whole record `w` encoded now: its local layout when its
/// foreign part is empty and no exception row is a deviation, else encoding 2 with the foreign
/// part, as the store writes it (`Store::bind_assessment_wire`).
pub fn assessment_wire_id(w: &AssessmentV2) -> Hash {
    Hash::of(&rdy_store::codec::item_new(kinds::ASSESSMENT, w).1)
}

/// The effective lifecycle of `id` in `m` is `stable` (its own `lifecycle`, else its nearest
/// ancestor's).
fn stable_in(m: &Materialized, id: &BlockId) -> bool {
    let mut cur = m.get(id);
    let mut steps = 0;
    while let Some(b) = cur {
        if let Some(l) = b
            .fields
            .get("lifecycle")
            .filter(|l| **l != FieldValue::Null)
        {
            return l.as_str() == Some("stable");
        }
        steps += 1;
        if steps > m.len() {
            break;
        }
        cur = b.parent.as_ref().and_then(|p| m.get(p));
    }
    false
}

/// The stable blocks `c`'s ops touch on `m`, in op order, each once.
fn stable_touched(st: &ReadState, m: &Materialized, c: &Change) -> Vec<BlockId> {
    let pack = st.pack();
    let created: BTreeSet<&BlockId> = c
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Create { block } => Some(&block.id),
            _ => None,
        })
        .collect();
    let mut out: Vec<BlockId> = vec![];
    let mut push = |id: &BlockId| {
        if !out.contains(id) {
            out.push(id.clone());
        }
    };
    for op in &c.ops {
        let subject = match op {
            Op::Create { block } => match &block.parent {
                Some(p) if !created.contains(p) => p.clone(),
                _ => continue,
            },
            Op::Update { block, .. } => block.id.clone(),
            Op::Delete { id, .. } => id.clone(),
        };
        let Some(b) = m.get(&subject) else {
            continue;
        };
        if pack.is_presentational(&b.block_type) {
            continue;
        }
        if stable_in(m, &subject) {
            push(&subject);
        }
    }
    out
}

/// The facts of version `p`'s changes, each judged on the canon its chain held before it (the
/// newest landing of `p.canon_at`'s chain whose frame lies before the change) plus `p`'s earlier
/// changes.
fn change_facts(st: &ReadState, p: &Proposal) -> Result<Vec<ChangeFact>, Violation> {
    let reader = st.reader();
    // canon's chain from the version's canon_at back, with each landing's place in the log
    let mut chain: Vec<(Hash, Location)> = vec![];
    let mut next = p.canon_at;
    while let Some(h) = next {
        let at = reader
            .landing_location(&h)
            .map_err(|e| read_err(format!("landing {}", h.short()), e))?;
        chain.push((h, at));
        next = reader
            .landing(&h)
            .map_err(|e| read_err(format!("landing {}", h.short()), e))?
            .prev;
    }
    let mut read: Vec<Change> = vec![];
    for c in &p.changes {
        read.push(
            reader
                .change(c)
                .map_err(|e| read_err(format!("change {}", c.short()), e))?,
        );
    }
    let mut out = vec![];
    // the state the last change was judged on: its canon, how many changes it applies, and it;
    // carried from one change to the next while the canon stays the same
    let mut written: Option<(Option<Hash>, usize, Materialized)> = None;
    for (i, c) in read.iter().enumerate() {
        let id = p.changes[i];
        let at = reader
            .change_location(&id)
            .map_err(|e| read_err(format!("the place of change {}", id.short()), e))?;
        let canon = chain.iter().find(|(_, l)| *l < at).map(|(h, _)| *h);
        if written.as_ref().is_none_or(|(w, _, _)| *w != canon) {
            let base = match canon {
                Some(h) => (*st
                    .materialized(&Scope::At(h))
                    .map_err(|e| read_err(format!("canon at {}", h.short()), e))?)
                .clone(),
                None => Materialized::default(),
            };
            written = Some((canon, 0, base));
        }
        let (_, next, m) = written.as_mut().expect("set above");
        for earlier in &read[*next..i] {
            *m = m.with(&earlier.ops);
        }
        *next = i;
        let m = &*m;
        out.push(ChangeFact {
            id,
            at,
            briefing: c.links.briefing,
            imported: c.links.imported_by.is_some(),
            replay: c.links.replayed_from.is_some(),
            sources: sources_of(reader, c)?,
            stable: stable_touched(st, m, c),
        });
    }
    Ok(out)
}

/// `c`'s replay chain, nearest first (see [`ChangeFact::sources`]).
fn sources_of(reader: &StoreReader, c: &Change) -> Result<Vec<SourceFact>, Violation> {
    let mut out: Vec<SourceFact> = vec![];
    let mut next = c.links.replayed_from;
    while let Some(src) = next {
        if out.len() > MAX_REPLAY_STEPS || out.iter().any(|s| s.change == src) {
            break;
        }
        let source = match reader.change(&src) {
            Ok(s) => s,
            Err(StoreError::NotFound(_)) => break,
            Err(e) => return Err(read_err(format!("change {}", src.short()), e)),
        };
        let at = reader
            .change_location(&src)
            .map_err(|e| read_err(format!("the place of change {}", src.short()), e))?;
        out.push(SourceFact {
            change: src,
            imported: source.links.imported_by.is_some(),
            at,
        });
        next = source.links.replayed_from;
    }
    Ok(out)
}

/// The seal of landing `l` (approved version `p`) with the answers and subjects it names.
fn seal_fact(st: &ReadState, l: &LandingFact) -> Result<Option<SealFact>, Violation> {
    let reader = st.reader();
    let end = st.seq();
    let Some(plan) = reader
        .plan_of_landing(&l.landing, end)
        .map_err(|e| read_err(format!("the seal of {}", l.landing.short()), e))?
    else {
        return Ok(None);
    };
    let record = reader
        .plan(&plan)
        .map_err(|e| read_err(format!("plan {}", plan.short()), e))?;
    let items = record
        .seal
        .as_ref()
        .map(|s| s.items.clone())
        .unwrap_or_default();
    let mut answer_acts = BTreeMap::new();
    for h in items.iter().filter_map(|i| i.answer) {
        let a: rdy_model::Answer = reader
            .get_typed(kinds::ANSWER, &h)
            .map_err(|e| read_err(format!("answer {}", h.short()), e))?;
        answer_acts.insert(h, a.act);
    }
    let mut subjects = BTreeMap::new();
    let version = l.record.approved_version.expect("an approved version");
    let fin = st
        .materialized(&Scope::Proposal(version))
        .map_err(|e| read_err(format!("version {}", version.short()), e))?;
    let tracking = reader.tracking();
    for i in items.iter().filter(|i| i.kind != PlanKind::Tension) {
        let Ok(id) = BlockId::parse(&i.subject) else {
            continue;
        };
        let b: Option<&Block> = fin.get(&id);
        subjects.insert(
            i.subject.clone(),
            SubjectFact {
                stable: stable_in(&fin, &id),
                content: b
                    .filter(|b| !b.is_redacted())
                    .map(|b| b.content_hash_with(tracking)),
                decision: b.is_some_and(|b| b.block_type == decision::DECISION),
            },
        );
    }
    Ok(Some(SealFact {
        plan,
        record,
        answer_acts,
        subjects,
    }))
}

/// The durable M3 rules on `r`'s canon chain, read from one snapshot. Empty: they hold.
pub fn check_m3(r: &rdy_engine::Rdy) -> Vec<Violation> {
    check_m3_in(&r.read())
}

/// [`check_m3`] on the snapshot `st`.
pub fn check_m3_in(st: &ReadState) -> Vec<Violation> {
    match M3Facts::read(st) {
        Ok(f) => check_facts(&f),
        Err(e) => vec![e],
    }
}

/// The durable rules on projection `f`, every violation of every landing.
pub fn check_facts(f: &M3Facts) -> Vec<Violation> {
    f.landings
        .iter()
        .flat_map(|l| check_landing(l, f.first_briefed))
        .collect()
}

/// The durable rules on one landing: `m3_briefing`, `m3_version`, `m3_assessment`, `m3_plan`
/// and `m3_route`.
pub fn check_landing(l: &LandingFact, first_briefed: Option<Location>) -> Vec<Violation> {
    let mut out = vec![];
    if !l.is_m3() {
        return out;
    }
    let assessed = l.record.assessment.is_some();
    if assessed && !l.reaffirm_card {
        out.extend(briefing_rule(l, first_briefed));
    }
    out.extend(binding_rule(l));
    if assessed && !l.reaffirm_card {
        out.extend(plan_rule(l));
    }
    out.extend(route_rule(l));
    out
}

/// Exemptions (b) and (c) of the built briefing rule.
fn exempt(imported: bool, at: Location, first_briefed: Option<Location>) -> bool {
    imported || first_briefed.is_none_or(|first| at < first)
}

/// Exemption (d): a replay whose chain reaches, within [`MAX_REPLAY_STEPS`] steps, a source that
/// (b) or (c) exempts. A repeated source ends the chain.
fn replay_exempt(c: &ChangeFact, first_briefed: Option<Location>) -> bool {
    if !c.replay {
        return false;
    }
    let mut seen = BTreeSet::new();
    for s in c.sources.iter().take(MAX_REPLAY_STEPS) {
        if !seen.insert(s.change) {
            return false;
        }
        if exempt(s.imported, s.at, first_briefed) {
            return true;
        }
    }
    false
}

/// `m3_briefing`: each change of the approved version that touches stable content records a
/// briefing, or an exemption of the built rule applies (import, before the first briefing, a
/// replay chain of at most 64 steps to such a change).
fn briefing_rule(l: &LandingFact, first_briefed: Option<Location>) -> Vec<Violation> {
    let mut out = vec![];
    for c in &l.changes {
        if c.briefing.is_some()
            || exempt(c.imported, c.at, first_briefed)
            || replay_exempt(c, first_briefed)
        {
            continue;
        }
        let Some(first) = c.stable.first() else {
            continue;
        };
        let ids: Vec<String> = c.stable.iter().map(ToString::to_string).collect();
        out.push(v(
            "m3_briefing",
            Some(first),
            format!(
                "landing {}: change {} touches stable content ({}), records no briefing, and no exemption applies",
                l.landing.short(),
                c.id.short(),
                ids.join(", ")
            ),
        ));
    }
    out
}

/// `m3_version` and `m3_assessment`: the assessment the landing names assesses the approved
/// version and keeps its bytes (a later method cannot rewrite them). That it is the version's
/// newest assessment is the temporal audit's rule: the store accepts an older one of the version,
/// and a released fixture holds one.
fn binding_rule(l: &LandingFact) -> Vec<Violation> {
    let mut out = vec![];
    let Some(h) = l.record.assessment else {
        return out;
    };
    let name = l.landing.short();
    let Some(version) = l.record.approved_version else {
        out.push(v(
            "m3_version",
            None,
            format!(
                "landing {name} names assessment {} but no approved version",
                h.short()
            ),
        ));
        return out;
    };
    let Some(a) = &l.assessment else {
        out.push(v(
            "m3_assessment",
            None,
            format!("landing {name}: assessment {} is not held", h.short()),
        ));
        return out;
    };
    if a.proposal != version {
        out.push(v(
            "m3_version",
            None,
            format!(
                "landing {name} approved {} but its assessment {} assesses {}",
                version.short(),
                h.short(),
                a.proposal.short()
            ),
        ));
    }
    let now = match &l.assessment_foreign {
        Some(foreign) => {
            let mut w = AssessmentV2::of(a);
            w.foreign = foreign.clone();
            assessment_wire_id(&w)
        }
        None => assessment_id(a),
    };
    if now != h {
        out.push(v(
            "m3_assessment",
            None,
            format!(
                "landing {name}: assessment {} reads as {} (method {}): its stored bytes changed",
                h.short(),
                now.short(),
                a.method
            ),
        ));
    }
    out
}

/// `m3_plan`: the seal accounts for each item. A tension item is answered (D-45RG6); an open
/// non-code item stands only on drafts (D-FQ7AK); a deferred non-code item names the accepted
/// deferral card (D-AAQ9Z); an answered outcome has its answer, which did not lapse.
fn plan_rule(l: &LandingFact) -> Vec<Violation> {
    let mut out = vec![];
    let Some(seal) = &l.seal else {
        return out;
    };
    let Some(s) = &seal.record.seal else {
        out.push(v(
            "m3_plan",
            None,
            format!(
                "landing {}: plan {} has no seal",
                l.landing.short(),
                seal.plan.short()
            ),
        ));
        return out;
    };
    let name = l.landing.short();
    for i in &s.items {
        let label = format!("{} item {} ({})", i.kind.as_str(), i.id.short(), i.subject);
        let answer = seal.record.answers.iter().find(|a| a.item == i.id);
        let code = i.kind == PlanKind::UpdateImplementation;
        let subject = seal.subjects.get(&i.subject);
        if i.kind == PlanKind::Tension {
            let act = match i.outcome {
                PlanOutcome::Confirmed => Some(PlanAct::Confirm),
                PlanOutcome::Dismissed => Some(PlanAct::Dismiss),
                PlanOutcome::Updated => None,
                _ => {
                    out.push(v(
                        "m3_plan",
                        Some(&i.cause),
                        format!(
                            "landing {name}: {label} sealed {}: it has no answer",
                            i.outcome.as_str()
                        ),
                    ));
                    continue;
                }
            };
            if let Some(act) = act
                && answer.is_none_or(|a| a.act != act)
            {
                out.push(v(
                    "m3_plan",
                    Some(&i.cause),
                    format!(
                        "landing {name}: {label} sealed {} with no {} answer",
                        i.outcome.as_str(),
                        act.as_str()
                    ),
                ));
            }
            continue;
        }
        let act = match i.outcome {
            PlanOutcome::Open => {
                if !code && subject.is_some_and(|s| s.stable) {
                    out.push(v(
                        "m3_plan",
                        Some(&i.cause),
                        format!("landing {name}: {label} is open on stable content"),
                    ));
                }
                continue;
            }
            PlanOutcome::Confirmed => PlanAct::Confirm,
            PlanOutcome::Done => PlanAct::Done,
            PlanOutcome::Deferred => PlanAct::Defer,
            _ => continue,
        };
        if i.outcome == PlanOutcome::Deferred
            && !code
            && i.answer
                .and_then(|h| seal.answer_acts.get(&h))
                .is_none_or(|a| *a != AnswerAct::AcceptDeferrals)
        {
            out.push(v(
                "m3_plan",
                Some(&i.cause),
                format!("landing {name}: {label} is deferred with no accepted deferral card"),
            ));
        }
        let Some(a) = answer.filter(|a| a.act == act) else {
            out.push(v(
                "m3_plan",
                Some(&i.cause),
                format!(
                    "landing {name}: {label} sealed {} with no {} answer",
                    i.outcome.as_str(),
                    act.as_str()
                ),
            ));
            continue;
        };
        if let Some(s) = subject.filter(|s| !s.decision)
            && let Some(content) = s.content
            && a.subject != Some(content)
        {
            out.push(v(
                "m3_plan",
                Some(&i.cause),
                format!(
                    "landing {name}: the {} answer to {label} lapsed: {} changed after it",
                    act.as_str(),
                    i.subject
                ),
            ));
        }
    }
    out
}

/// `m3_route`: the approval route's evidence matches its assurance. A policy landing records
/// the policy, the assessment and the approved version, and no relay; a policy is recorded only
/// with `assurance: policy`; a relay records its agent, the person's words and text or card
/// evidence, a card with its hash; a local landing records no relay.
fn route_rule(l: &LandingFact) -> Vec<Violation> {
    let mut out = vec![];
    let rec = &l.record;
    let name = l.landing.short();
    let mut bad = |d: String| out.push(v("m3_route", None, format!("landing {name}: {d}")));
    let relayed = rec.relayed_by.is_some() || rec.relay_evidence.is_some();
    match rec.assurance {
        Assurance::Policy => {
            if rec.policy.is_none() {
                bad("assurance policy records no policy".into());
            }
            if rec.assessment.is_none() {
                bad("assurance policy records no assessment".into());
            }
            if rec.approved_version.is_none() {
                bad("assurance policy records no approved version".into());
            }
            if relayed {
                bad("assurance policy records a relay".into());
            }
            if let Some(a) = &l.assessment {
                let assessed = a.policy.as_ref();
                if !matches!(
                    assessed.map(|p| &p.verdict),
                    Some(PolicyVerdict::Accepts { .. })
                ) {
                    bad("assurance policy on an assessment whose policy does not accept it".into());
                }
                if assessed.and_then(|p| p.policy.as_ref()) != rec.policy.as_ref() {
                    bad(
                        "the policy it records is not the one its assessment was judged under"
                            .into(),
                    );
                }
            }
        }
        Assurance::Relayed => {
            if rec.policy.is_some() {
                bad("a relayed landing records a policy".into());
            }
            if rec.relayed_by.is_none() || rec.relay_evidence.is_none() {
                bad("a relayed landing records no relaying agent or no words".into());
            }
            match &rec.evidence {
                None => bad("a relayed landing records no evidence".into()),
                Some(e) if e.kind == EvidenceKind::Card && e.card_hash.is_none() => {
                    bad("a relayed card approval records no shown card".into())
                }
                Some(e) if e.kind == EvidenceKind::Elicited => {
                    bad("a relayed landing records elicited evidence".into())
                }
                Some(_) => {}
            }
        }
        Assurance::Local => {
            if rec.policy.is_some() {
                bad("a local landing records a policy".into());
            }
            if relayed {
                bad("a local landing records a relay".into());
            }
        }
    }
    out
}

/// What the person saw before a landing, as the caller's journal recorded it: the version and
/// the card's hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub version: Hash,
    pub card_hash: Option<Hash>,
}

/// A landing observed between two held read snapshots, and what the person saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedLanding {
    pub landing: Hash,
    pub shown: Option<Shown>,
}

/// The five later refusals (D-ME2HD "Later refusals", D-HAG73), as a person's act records them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HoldKind {
    RejectedDecision,
    RejectedReaffirm,
    RefusedDeferrals,
    AcceptedAdvice,
    AcceptedVerdict,
}

impl HoldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HoldKind::RejectedDecision => "rejected decision",
            HoldKind::RejectedReaffirm => "rejected one_way re-affirmation",
            HoldKind::RefusedDeferrals => "refused deferrals",
            HoldKind::AcceptedAdvice => "accepted advice",
            HoldKind::AcceptedVerdict => "accepted check verdict",
        }
    }
}

/// One person act that holds a version, and the item it names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hold {
    pub kind: HoldKind,
    pub item: String,
}

/// The temporal projection of one landing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalFacts {
    /// The landing's durable fact, read from `after`.
    pub landing: LandingFact,
    pub first_briefed: Option<Location>,
    /// Canon's head in `before`.
    pub before_head: Option<Hash>,
    /// The landings of `before`'s canon chain after the assessment's base, oldest first, each
    /// with the blocks its changes write.
    pub since: Vec<(Hash, BTreeSet<BlockId>)>,
    /// The pack digest, gate config digest and live policy in `before`.
    pub pack_digest: Hash,
    pub config_digest: Hash,
    pub live_policy: Option<PolicyRef>,
    /// The person acts recorded in `before` that hold the approved version.
    pub holds: Vec<Hold>,
    pub shown: Option<Shown>,
}

impl TemporalFacts {
    /// The projection of `landing` of `after`, observed after `before`.
    pub fn read(
        before: &ReadState,
        after: &ReadState,
        landing: &ObservedLanding,
    ) -> Result<TemporalFacts, Violation> {
        let h = landing.landing;
        let ar = after.reader();
        let rec = chain_of(ar, after.canon_head())?
            .into_iter()
            .find(|(l, _)| *l == h)
            .map(|(_, r)| r)
            .ok_or_else(|| {
                v(
                    READ,
                    None,
                    format!("landing {} is not in canon's chain", h.short()),
                )
            })?;
        // read whole: a landing that records no M3 field is still under the current contract
        let fact = LandingFact::read_whole(after, h, rec)?;
        let first_briefed = ar
            .first_briefed_change()
            .map_err(|e| read_err("the first briefed change", e))?;
        let br = before.reader();
        let before_head = before.canon_head();
        let mut since = vec![];
        if let Some(a) = &fact.assessment {
            let chain = chain_of(br, before_head)?;
            let cut = chain.iter().position(|(l, _)| Some(*l) == a.base);
            let newer = match cut {
                Some(n) => &chain[..n],
                None => &chain[..],
            };
            for (l, r) in newer.iter().rev() {
                let mut written = BTreeSet::new();
                for c in &r.changes {
                    let ch = br
                        .change(c)
                        .map_err(|e| read_err(format!("change {}", c.short()), e))?;
                    written.extend(ch.ops.iter().map(|op| op.block_id().clone()));
                }
                since.push((*l, written));
            }
        }
        let canon = before
            .materialized(&Scope::Canon)
            .map_err(|e| read_err("canon", e))?;
        let live_policy = rdy_engine::policy::live_policy(before, &canon).map(|p| p.reference());
        let holds = match fact.record.approved_version {
            Some(version) => holds_of(before, &version)?,
            None => vec![],
        };
        Ok(TemporalFacts {
            landing: fact,
            first_briefed,
            before_head,
            since,
            pack_digest: pack_digest(before.pack()),
            config_digest: config_digest(&before.gate_config()),
            live_policy,
            holds,
            shown: landing.shown.clone(),
        })
    }
}

/// `P-<stable short>@<n>` of version `version` (record `p`).
fn version_ref(reader: &StoreReader, version: &Hash, p: &Proposal) -> Result<String, Violation> {
    let mut n = 1;
    let mut cur = p.prev;
    while let Some(prev) = cur {
        n += 1;
        cur = reader
            .proposal(&prev)
            .map_err(|e| read_err(format!("proposal {}", prev.short()), e))?
            .prev;
    }
    Ok(format!("P-{}@{n}", p.stable.unwrap_or(*version).short()))
}

/// The person acts recorded in `st` that hold version `version` (the five later refusals, read
/// from the answers and the semantic records alone).
pub fn holds_of(st: &ReadState, version: &Hash) -> Result<Vec<Hold>, Violation> {
    let reader = st.reader();
    let p = reader
        .proposal(version)
        .map_err(|e| read_err(format!("proposal {}", version.short()), e))?;
    let pref = version_ref(reader, version, &p)?;
    // the latest answer to an item, when the snapshot holds it (a later answer hides an earlier
    // one, so a hold answered over after the snapshot is not read: the projection under-reads)
    let end = st.seq();
    let answered = |item: &str, act: AnswerAct| -> Result<bool, Violation> {
        let latest = reader
            .answer_of(item)
            .map_err(|e| read_err(format!("the answer to {item}"), e))?;
        let Some((h, a)) = latest else {
            return Ok(false);
        };
        let held = reader
            .has_kind_before(kinds::ANSWER, &h, end)
            .map_err(|e| read_err(format!("answer {}", h.short()), e))?;
        Ok(held && a.act == act)
    };
    let mut out = vec![];
    // the decisions the version writes: each block's last create or update, none when deleted
    let mut last: BTreeMap<BlockId, Option<Block>> = BTreeMap::new();
    for c in &p.changes {
        let ch = reader
            .change(c)
            .map_err(|e| read_err(format!("change {}", c.short()), e))?;
        for op in ch.ops {
            match op {
                Op::Create { block } | Op::Update { block, .. }
                    if block.block_type == decision::DECISION =>
                {
                    last.insert(block.id.clone(), Some(block));
                }
                op => {
                    if let Some(b) = last.get_mut(op.block_id()) {
                        *b = None;
                    }
                }
            }
        }
    }
    let canon = st
        .materialized(&Scope::Canon)
        .map_err(|e| read_err("canon", e))?;
    for (id, b) in last {
        let Some(b) = b else { continue };
        let item = format!("{id}@{}", b.version_hash().short());
        if decision::status(&b) == Some(decision::DecisionStatus::Proposed) {
            if answered(&item, AnswerAct::RejectDecision)? {
                out.push(Hold {
                    kind: HoldKind::RejectedDecision,
                    item,
                });
            }
        } else if !rdy_model::engine_fields::reaffirmed_causes(canon.get(&id), &b).is_empty()
            && b.field_str("reversibility") == Some("one_way")
            && answered(&item, AnswerAct::RejectReaffirm)?
        {
            out.push(Hold {
                kind: HoldKind::RejectedReaffirm,
                item,
            });
        }
    }
    let defer = format!("defer:{pref}");
    if answered(&defer, AnswerAct::RefuseDeferrals)? {
        out.push(Hold {
            kind: HoldKind::RefusedDeferrals,
            item: defer,
        });
    }
    // an accepted advice: a semantic tension the version's assessment lists as open advisory,
    // which a person's acceptance made blocking since
    if let Some(a) = reader
        .assessment_of_proposal(version, st.seq())
        .map_err(|e| read_err(format!("the assessment of {}", version.short()), e))?
    {
        let a = reader
            .assessment(&a)
            .map_err(|e| read_err(format!("assessment {}", a.short()), e))?;
        let advisory: Vec<BTreeSet<&BlockId>> = a
            .tensions
            .iter()
            .filter(|t| {
                t.kind == "semantic"
                    && t.severity == AssessmentSeverity::Advisory
                    && t.status == AssessmentTensionStatus::Open
            })
            .map(|t| t.blocks.iter().collect())
            .collect();
        let accepted = st.semantic().tensions().any(|o| {
            // an assessment names a tension's local blocks: a cited side is no local block
            let blocks: BTreeSet<&BlockId> = o
                .record
                .items
                .iter()
                .filter_map(|s| s.local())
                .map(|i| &i.block)
                .collect();
            o.record.blocking && advisory.contains(&blocks)
        });
        if accepted {
            out.push(Hold {
                kind: HoldKind::AcceptedAdvice,
                item: format!("advice:{pref}"),
            });
        }
    }
    // an accepted check verdict for the version
    let mut verdicts = vec![];
    reader
        .for_each_verdict_answer(st.seq(), |_, a| {
            if let Some((review, v)) = a.option.as_deref().and_then(parse_accept_option)
                && v == *version
                && a.item == format!("{VERDICT_ITEM}{review}")
            {
                verdicts.push(a.item);
            }
            Ok(())
        })
        .map_err(|e| read_err("the verdict answers", e))?;
    verdicts.sort();
    verdicts.dedup();
    out.extend(verdicts.into_iter().map(|item| Hold {
        kind: HoldKind::AcceptedVerdict,
        item,
    }));
    Ok(out)
}

/// The temporal rules on projection `t`, then the durable rules on its landing.
///
/// `m3_snapshot`: `before` is the canon the landing read. `m3_assessment`: a landing under the
/// current contract names an assessment, except a re-affirmation card's, and it is the approved
/// version's newest one before the landing. `m3_fresh`: no landing
/// since the assessment's base wrote a block of its reach, and the pack, the gate config and the
/// live policy are the ones it was judged under. `m3_hold`: no person act held the version.
/// `m3_card`: the person saw the approved version, and the card a card approval names.
pub fn check_temporal(t: &TemporalFacts) -> Vec<Violation> {
    let l = &t.landing;
    let rec = &l.record;
    let name = l.landing.short();
    let mut out = vec![];
    if t.before_head != rec.prev {
        out.push(v(
            "m3_snapshot",
            None,
            format!(
                "landing {name} lands on {}, but the before snapshot's canon is {}",
                rec.prev.map_or("the empty canon".into(), |h| h.short()),
                t.before_head
                    .map_or("the empty canon".into(), |h| h.short())
            ),
        ));
    }
    match &l.assessment {
        None if !l.reaffirm_card => out.push(v(
            "m3_assessment",
            None,
            format!("landing {name} names no assessment"),
        )),
        None => {}
        Some(a) => {
            // the approval binds the version's newest assessment (a fresh one after a moved card)
            if let Some(h) = rec.assessment
                && l.bound_assessment != Some(h)
            {
                out.push(v(
                    "m3_assessment",
                    None,
                    format!(
                        "landing {name} names assessment {}, but the newest assessment of its version before it is {}",
                        h.short(),
                        l.bound_assessment.map_or("none".to_string(), |b| b.short())
                    ),
                ));
            }
            let reach: BTreeSet<&BlockId> = a.reach.iter().collect();
            for (since, written) in &t.since {
                let hit: Vec<String> = written
                    .iter()
                    .filter(|b| reach.contains(b))
                    .map(ToString::to_string)
                    .collect();
                if !hit.is_empty() {
                    out.push(v(
                        "m3_fresh",
                        None,
                        format!(
                            "landing {name}: landing {} changed {} of its assessment's reach",
                            since.short(),
                            hit.join(", ")
                        ),
                    ));
                }
            }
            if t.pack_digest != a.pack.digest {
                out.push(v(
                    "m3_fresh",
                    None,
                    format!("landing {name}: the pack changed after its assessment"),
                ));
            }
            if t.config_digest != a.config {
                out.push(v(
                    "m3_fresh",
                    None,
                    format!("landing {name}: the gate config changed after its assessment"),
                ));
            }
            if t.live_policy.as_ref() != a.policy.as_ref().and_then(|p| p.policy.as_ref()) {
                out.push(v(
                    "m3_fresh",
                    None,
                    format!("landing {name}: the authority policy changed after its assessment"),
                ));
            }
        }
    }
    for h in &t.holds {
        out.push(v(
            "m3_hold",
            None,
            format!(
                "landing {name}: a person's {} ({}) held the version",
                h.kind.as_str(),
                h.item
            ),
        ));
    }
    if let Some(shown) = &t.shown {
        if Some(shown.version) != rec.approved_version {
            out.push(v(
                "m3_card",
                None,
                format!(
                    "landing {name} approved {}, but the person saw {}",
                    rec.approved_version
                        .map_or("no version".into(), |h| h.short()),
                    shown.version.short()
                ),
            ));
        }
        if let Some(card) = rec.evidence.as_ref().and_then(|e| e.card_hash)
            && shown.card_hash != Some(card)
        {
            out.push(v(
                "m3_card",
                None,
                format!(
                    "landing {name} names card {}, but the person saw {}",
                    card.short(),
                    shown.card_hash.map_or("no card".into(), |h| h.short())
                ),
            ));
        }
    }
    out.extend(check_landing(l, t.first_briefed));
    out
}

/// The temporal audit of `landing`, observed between the held snapshots `before` and `after`:
/// [`check_temporal`] on its projection, then the existing landing audit
/// (`landing_audit::audit`) of the approved version's changes on `before`. Every violation.
pub fn audit_m3(
    before: &ReadState,
    after: &ReadState,
    landing: &ObservedLanding,
) -> Vec<Violation> {
    let t = match TemporalFacts::read(before, after, landing) {
        Ok(t) => t,
        Err(e) => return vec![e],
    };
    let mut out = check_temporal(&t);
    if let Some(p) = &t.landing.approved {
        out.extend(audit(
            before,
            &Landed {
                view: p.view.clone(),
                changes: p.changes.clone(),
                alternative: t.landing.record.chosen.as_ref().map(|c| c.set),
            },
        ));
    }
    out
}
