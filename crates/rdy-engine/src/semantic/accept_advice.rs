//! A person's acceptance of a reviewer's advice (D-WMCFW, D-2K8Q5): one answer for the advised semantic tensions of one proposal version, given plainly by a human session or relayed by an agent as itself with the person's words (FW-B9, FW-UX1, D-67ZN7). An accepted tension blocks until a side's content changes.
use crate::answer::relay::{RelayInput, RelayRefusal, relayed};
use crate::error::{E_BAD_INPUT, E_PERMISSION, E_PROPOSAL_MOVED, RdyError};
use crate::gate::{
    ProposalRef, landed_as, latest_version, open_proposals, resolve_in, withdrawn, withdrawn_error,
};
use crate::proposal_show::is_proposal_ref;
use crate::semantic::state::OpenTension;
use crate::tensions::api::AcceptRequest;
use crate::tensions::semantic::pair_of;
use crate::tensions::{
    SideRef, Tension, TensionSeverity, TensionStatus, tension_id, tension_label,
};
use crate::{Rdy, ReadState, Session};
use rdy_model::{Hash, SemanticTension};
use rdy_store::Relay;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// What an acceptance wrote: the proposal version, one row per accepted tension, and the
/// severity the tensions now have.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AcceptInfo {
    /// `P-<stable short>@<n>`.
    pub proposal: String,
    pub accepted: Vec<AcceptedRow>,
    pub severity: TensionSeverity,
    /// Every tension that now blocks because of this acceptance: it binds to the pair's two
    /// texts, so another view's or proposal's form of the same pair blocks too. By view, then
    /// tension.
    pub now_blocking: Vec<NowBlockingRow>,
}

/// A tension that blocks after an acceptance, in one view that holds a change side of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NowBlockingRow {
    /// `tn-<12 hex>`.
    pub tension: String,
    pub view: String,
    /// The view's open proposal (`P-<stable short>@<n>`) when it holds the change side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
}

/// One accepted tension: its label now (the severity is in the id, so it is new), its label
/// before, and the semantic tension version the acceptance wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AcceptedRow {
    pub id: String,
    pub was: String,
    pub version: String,
}

/// An advised semantic tension of a proposal version: the tension and its pair's open record.
#[derive(Clone, Debug)]
pub(crate) struct Advised {
    pub tension: Arc<Tension>,
    pub open: Arc<OpenTension>,
}

/// The id `t` has while it is advisory. A semantic tension's id hashes its severity, so an
/// acceptance gives it a new one; the shown set's digest and a retry name it by this one.
fn advisory_id(t: &Tension) -> Hash {
    tension_id(t.kind, TensionSeverity::Advisory, &t.involved)
}

/// The digest of the advised set `advised`, as the propose reply and `rdy show P-x@n` give it
/// and an acceptance passes it back: 12 hex of the hash of every tension's advisory id, sorted.
/// An acceptance does not change it. `None` when the set is empty.
pub(crate) fn advised_digest(advised: &[Advised]) -> Option<String> {
    if advised.is_empty() {
        return None;
    }
    let ids: BTreeSet<Hash> = advised.iter().map(|a| advisory_id(&a.tension)).collect();
    let mut bytes = b"rdy-advised-1\0".to_vec();
    for id in ids {
        bytes.extend_from_slice(&id.0);
    }
    Some(Hash::of(&bytes).short())
}

impl ReadState {
    /// The advised semantic tensions of a version of view `view` with changes `changes`, by
    /// tension id: each open (not dismissed) tension of the version's landing form, as the gate
    /// judges it (final ruling 2), whose pair (`pair_of`) has an open record with the reviewer's
    /// advice. Accepted ones are in the list (their record is blocking).
    pub(crate) fn advised_of(
        &self,
        view: &str,
        changes: &[Hash],
    ) -> Result<Vec<Advised>, RdyError> {
        let sem = self.semantic();
        // no record, nothing advised: the landing form is not read
        if !sem.has_tensions() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for t in self.landing_stored_tensions(view, changes)? {
            let Some(open) = pair_of(&t).and_then(|k| sem.tension(&k)) else {
                continue;
            };
            if open.record.advice.is_none() || self.tension_status(&t) != TensionStatus::Open {
                continue;
            }
            let open = Arc::clone(open);
            out.push(Advised { tension: t, open });
        }
        Ok(out)
    }
}

fn bad(message: String, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

/// Who accepts and the relay `req` records (FW-B9, FW-UX1, D-67ZN7), by the relay table
/// ([`relayed`]): a human session accepts plainly, as itself, and names no relay (C5); an agent
/// session accepts only by relaying the person's words, as itself: over MCP the session is the
/// relay (no `relayed_by`, `implicit` when it names none), at the command line `relayed_by` is
/// its own actor alone, and the relay records the session's run. A relayed acceptance runs as
/// the person `person`. The table's
/// refusals keep the acceptance's own text for an agent with no words.
fn relay_of(
    s: &Session,
    person: &str,
    req: &AcceptRequest,
) -> Result<(Session, Option<Relay>), RdyError> {
    let input = RelayInput {
        relayed_by: req.relayed_by.clone(),
        evidence: req.evidence.clone(),
        question: req.question.clone(),
        session_ref: req.session_ref.clone(),
        kind: None,
    };
    let implicit = !s.is_human() && req.relayed_by.is_none();
    match relayed(s, person, &input, implicit) {
        // a human session's acceptance is plain: the session's own actor
        Ok((_, None)) => Ok((s.clone(), None)),
        Ok((who, Some(relay))) => Ok((who, Some(relay))),
        Err(RelayRefusal::NoEvidence) => Err(RdyError::new(
            E_PERMISSION,
            "an agent accepts advice only by relaying a person's words",
            "ask the person, then pass evidence (their words, quoted), question and session_ref",
        )),
        Err(refusal) => Err(refusal.error()),
    }
}

/// The exact proposal version `text` names, open and latest, with its view and changes. A
/// version that a close of its view withdrew is `E_VIEW_CLOSED` (D-K0SS1).
fn open_version(st: &ReadState, text: &str) -> Result<(ProposalRef, String, Vec<Hash>), RdyError> {
    let exact = (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        || (is_proposal_ref(text) && text.contains('@'));
    if !exact {
        return Err(bad(
            "an acceptance names the exact proposal version the person was shown".to_string(),
            "pass P-x@n from rdy_propose",
        ));
    }
    let pref = resolve_in(st, text)?;
    let p = st.reader().proposal(&pref.version)?;
    if landed_as(st, &p)?.is_some() || latest_version(st, &pref.stable)? != pref.version {
        return Err(RdyError::new(
            E_PROPOSAL_MOVED,
            format!("{pref} is not the latest open version"),
            "propose again and show the person the new version",
        ));
    }
    // D-K0SS1: a close of the version's view withdrew it
    if withdrawn(st, &pref.version, &p)? {
        return Err(withdrawn_error(&pref, &p.view));
    }
    Ok((pref, p.view, p.changes))
}

/// The advised tensions `texts` names, by tension id; every one not yet accepted when `texts` is
/// empty. A text matches a tension's id or its advisory id, so the label shown before an
/// acceptance still names the tension after it (a retry whose reply was lost).
fn chosen<'a>(
    advised: &'a [Advised],
    texts: &[String],
    pref: &ProposalRef,
) -> Result<Vec<&'a Advised>, RdyError> {
    if texts.is_empty() {
        if advised.is_empty() {
            return Err(bad(
                format!("{pref} has no advised semantic tension to accept"),
                "nothing to accept; a tension without advice stays advisory",
            ));
        }
        let open: Vec<&Advised> = advised.iter().filter(|a| !a.open.record.blocking).collect();
        if open.is_empty() {
            return Err(bad(
                format!("{pref}: the advice is accepted already"),
                "nothing to do; each accepted tension blocks until a side's content changes",
            ));
        }
        return Ok(open);
    }
    let mut picked: BTreeMap<Hash, &Advised> = BTreeMap::new();
    for text in texts {
        let hex = text
            .strip_prefix("tn-")
            .unwrap_or(text)
            .to_ascii_lowercase();
        let found: Vec<&Advised> =
            if (6..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                advised
                    .iter()
                    .filter(|a| {
                        a.tension.id.to_string().starts_with(&hex)
                            || advisory_id(&a.tension).to_string().starts_with(&hex)
                    })
                    .collect()
            } else {
                vec![]
            };
        match found.as_slice() {
            [one] if one.open.record.blocking => {
                return Err(bad(format!("{text} is accepted already"), "leave it out"));
            }
            [one] => {
                picked.insert(one.tension.id, one);
            }
            _ => {
                return Err(bad(
                    format!("{text} is not an advised semantic tension of {pref}"),
                    "pass the advised tensions rdy_propose lists, or leave tensions out to accept them all",
                ));
            }
        }
    }
    Ok(picked.into_values().collect())
}

impl Rdy {
    /// Accepts the reviewers' advice to block on the advised semantic tensions of the exact
    /// proposal version `req.proposal` names (all of them, or those `req.tensions` names), as
    /// one answer from a person: given plainly by a human session, or relayed by an agent with
    /// the person's words (`relay_of`; `person` is the project's human actor, from the caller,
    /// O18). Writes one acceptance version per pair, in one batch. No role is needed: the
    /// person's words are the authority.
    pub fn accept_advice(
        &mut self,
        s: &Session,
        person: &str,
        req: AcceptRequest,
    ) -> Result<AcceptInfo, RdyError> {
        let (who, relay) = relay_of(s, person, &req)?;
        self.accept_as(&who, relay, req)
    }

    /// [`Rdy::accept_advice`] once the relay table has run: `who` accepts, and `relay` is the
    /// relay the versions record. The relay fields of `req` are not read.
    pub(crate) fn accept_as(
        &mut self,
        who: &Session,
        relay: Option<Relay>,
        req: AcceptRequest,
    ) -> Result<AcceptInfo, RdyError> {
        let (relay, relayed_by) = match relay {
            Some(r) => (Some(r.evidence), Some(r.by)),
            None => (None, None),
        };
        let st = self.read();
        let (pref, view, changes) = open_version(&st, &req.proposal)?;
        let advised = st.advised_of(&view, &changes)?;
        // D-WMCFW: the person answers for the advised set they were shown, and only for it. With
        // no advice at all, `chosen` says so whatever the digest
        let given = req.advised_digest.trim().to_ascii_lowercase();
        if !advised.is_empty() && advised_digest(&advised).as_deref() != Some(given.as_str()) {
            return Err(RdyError::new(
                E_PROPOSAL_MOVED,
                format!("the advised tensions of {pref} changed since they were shown"),
                format!("show {pref} again and answer for the tensions it lists now"),
            ));
        }
        let chosen = chosen(&advised, &req.tensions, &pref)?;
        let provenance = who.provenance();
        let mut keys = BTreeSet::new();
        let versions: Vec<SemanticTension> = chosen
            .iter()
            .filter(|a| keys.insert(a.open.record.key))
            .map(|a| {
                let first = &a.open.record;
                SemanticTension {
                    key: first.key,
                    items: first.items.clone(),
                    review: first.review,
                    advice: first.advice.clone(),
                    blocking: true,
                    relay: relay.clone(),
                    relayed_by: relayed_by.clone(),
                    prev: Some(a.open.version),
                    provenance: provenance.clone(),
                }
            })
            .collect();
        let ids = self.record_tension_versions(&versions)?;
        let version_of: BTreeMap<Hash, Hash> = versions.iter().map(|v| v.key).zip(ids).collect();
        // the acceptance is written: what cannot be read now leaves the old labels and an empty
        // now_blocking, not an error
        let now = self.read();
        let forms = forms_of(&now, &versions);
        let accepted = chosen
            .iter()
            .map(|a| {
                let key = a.open.record.key;
                let of_key = |t: &&Arc<Tension>| pair_of(t) == Some(key);
                // the tension of the same pair and sides in the new state
                let new = forms
                    .iter()
                    .filter(of_key)
                    .find(|t| t.sides == a.tension.sides && t.involved == a.tension.involved)
                    .or_else(|| forms.iter().find(of_key))
                    .map_or_else(|| tension_label(&a.tension.id), |t| tension_label(&t.id));
                AcceptedRow {
                    id: new,
                    was: tension_label(&a.tension.id),
                    version: version_of[&key].to_string(),
                }
            })
            .collect();
        let now_blocking = now_blocking(&now, &forms);
        Ok(AcceptInfo {
            proposal: pref.to_string(),
            accepted,
            severity: TensionSeverity::Blocking,
            now_blocking,
        })
    }
}

/// Every tension of the pairs `versions` accepted, in state `st`: each form of each pair, in any
/// view or proposal (the acceptance binds to the pair's two texts). By id; empty when the set
/// cannot be read.
fn forms_of(st: &ReadState, versions: &[SemanticTension]) -> Vec<Arc<Tension>> {
    let Ok(set) = st.tensions() else {
        return vec![];
    };
    let mut out: BTreeMap<Hash, Arc<Tension>> = BTreeMap::new();
    for v in versions {
        let Some(item) = v.items.first() else {
            continue;
        };
        for t in set.involving_block(&item.block) {
            if pair_of(&t) == Some(v.key) {
                out.entry(t.id).or_insert(t);
            }
        }
    }
    out.into_values().collect()
}

/// The rows of `forms` that block in state `st`, one per tension and view that holds a change
/// side of it, with the view's open proposal when the proposal holds that side. By view, then
/// tension. A form with every side canon has no row: it never refuses a landing.
fn now_blocking(st: &ReadState, forms: &[Arc<Tension>]) -> Vec<NowBlockingRow> {
    let views_of = st.inflight_set().ok();
    let proposals = open_proposals(st).unwrap_or_default();
    let mut rows: BTreeMap<(String, String), Option<String>> = BTreeMap::new();
    for t in forms {
        if t.severity != TensionSeverity::Blocking || st.tension_status(t) != TensionStatus::Open {
            continue;
        }
        for side in &t.sides {
            let SideRef::Change(c) = side else {
                continue;
            };
            let views = views_of
                .as_ref()
                .and_then(|set| set.views_of.get(c).cloned())
                .unwrap_or_default();
            for view in views {
                let proposal = proposals
                    .iter()
                    .find(|(_, p)| p.view == view && p.changes.contains(c))
                    .map(|(pref, _)| pref.to_string());
                let row = rows.entry((view, tension_label(&t.id))).or_default();
                if row.is_none() {
                    *row = proposal;
                }
            }
        }
    }
    rows.into_iter()
        .map(|((view, tension), proposal)| NowBlockingRow {
            tension,
            view,
            proposal,
        })
        .collect()
}
