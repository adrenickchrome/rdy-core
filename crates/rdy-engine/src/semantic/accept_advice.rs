//! A person's acceptance of a reviewer's advice (D-WMCFW, D-2K8Q5): one answer for the advised semantic tensions of one proposal version, relayed from chat with the person's words (FW-B9, FW-UX1) or given at the CLI. An accepted tension blocks until a side's content changes.
use crate::error::{E_BAD_INPUT, E_PERMISSION, E_PROPOSAL_MOVED, RdyError};
use crate::gate::{ProposalRef, landed_as, latest_version, resolve_in};
use crate::proposal_show::is_proposal_ref;
use crate::semantic::state::OpenTension;
use crate::tensions::api::AcceptRequest;
use crate::tensions::semantic::pair_of;
use crate::tensions::{Tension, TensionSeverity, TensionStatus, tension_label};
use crate::{Rdy, ReadState, Session};
use rdy_model::{ActorRun, Hash, RelayEvidence, SemanticTension};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most characters `evidence` or `question` takes, after trimming.
const MAX_QUOTE: usize = 2000;

/// What an acceptance wrote: the proposal version, one row per accepted tension, and the
/// severity the tensions now have.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AcceptInfo {
    /// `P-<stable short>@<n>`.
    pub proposal: String,
    pub accepted: Vec<AcceptedRow>,
    pub severity: TensionSeverity,
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

impl ReadState {
    /// The advised semantic tensions with one of `changes` as a side, by tension id: each open
    /// (not dismissed) tension whose pair (`pair_of`) has an open record with the reviewer's
    /// advice. Accepted ones are in the list (their record is blocking).
    pub(crate) fn advised_of(&self, changes: &[Hash]) -> Result<Vec<Advised>, RdyError> {
        let sem = self.semantic();
        // no record, nothing advised: the tensions of the changes are not read
        if !sem.has_tensions() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for t in self.tensions_of_changes(changes)? {
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

/// The relay `req` records, checked (FW-B9, FW-UX1): the person's words, the question and the
/// session, and the agent that relays them at the command line. An agent session accepts only
/// by relaying the person's words; a human session accepts in their own right, or relays with
/// `relayed_by` and `evidence` together.
fn relay_of(
    s: &Session,
    req: &AcceptRequest,
) -> Result<(Option<RelayEvidence>, Option<ActorRun>), RdyError> {
    let evidence = req
        .evidence
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    if !s.is_human() {
        if evidence.is_none() {
            return Err(RdyError::new(
                E_PERMISSION,
                "an agent accepts advice only by relaying a person's words",
                "ask the person, then pass evidence (their words, quoted), question and session_ref",
            ));
        }
        if req.relayed_by.is_some() {
            return Err(bad(
                "relayed_by names the agent relaying at the command line; over MCP the session is the relay".to_string(),
                "drop relayed_by",
            ));
        }
    } else if req.relayed_by.is_some() != evidence.is_some() {
        return Err(bad(
            "relayed_by and evidence go together".to_string(),
            "pass both (--relayed-by and --evidence) or neither",
        ));
    }
    for (field, given) in [
        ("question", req.question.is_some()),
        ("session_ref", req.session_ref.is_some()),
    ] {
        if given && evidence.is_none() {
            return Err(bad(
                format!("{field} needs evidence"),
                &format!("pass evidence with it, or drop {field}"),
            ));
        }
    }
    let question = req
        .question
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    if [evidence, question]
        .iter()
        .flatten()
        .any(|t| t.chars().count() > MAX_QUOTE)
    {
        return Err(bad(
            format!("evidence and question take at most {MAX_QUOTE} characters each"),
            "quote the person's answer, not the whole conversation",
        ));
    }
    let by = req.relayed_by.as_deref().map(actor_run).transpose()?;
    let relay = evidence.map(|quoted| {
        let default = if s.is_human() { "cli" } else { "mcp" };
        let session_ref = req
            .session_ref
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(default);
        RelayEvidence {
            quoted: quoted.to_string(),
            session_ref: session_ref.to_string(),
            question: question.map(str::to_string),
        }
    });
    Ok((relay, by))
}

/// `actor[/run]`, with `rdy approve --relayed-by`'s checks and texts.
fn actor_run(text: &str) -> Result<ActorRun, RdyError> {
    let (actor, run) = match text.split_once('/') {
        Some((actor, run)) => (actor.to_string(), Some(run.to_string())),
        None => (text.to_string(), None),
    };
    if actor.trim().is_empty() {
        return Err(bad(
            "relayed_by needs an actor".to_string(),
            "name the relaying agent, e.g. --relayed-by agent:name",
        ));
    }
    if run.as_deref().is_some_and(|r| r.trim().is_empty()) {
        return Err(bad(
            "relayed_by has an empty run".to_string(),
            "name the run after '/', e.g. --relayed-by agent:name/run-7, or drop the '/'",
        ));
    }
    Ok(ActorRun { actor, run })
}

/// The exact proposal version `text` names, open and latest.
fn open_version(st: &ReadState, text: &str) -> Result<(ProposalRef, Vec<Hash>), RdyError> {
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
    Ok((pref, p.changes))
}

/// The advised tensions `texts` names, by tension id; every one not yet accepted when `texts` is
/// empty.
fn chosen<'a>(
    advised: &'a [Advised],
    texts: &[String],
    pref: &ProposalRef,
) -> Result<Vec<&'a Advised>, RdyError> {
    if texts.is_empty() {
        let open: Vec<&Advised> = advised.iter().filter(|a| !a.open.record.blocking).collect();
        if open.is_empty() {
            return Err(bad(
                format!("{pref} has no advised semantic tension to accept"),
                "nothing to accept; a tension without advice stays advisory",
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
                    .filter(|a| a.tension.id.to_string().starts_with(&hex))
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
    /// one answer from a person: given by a human session, or relayed with the person's words.
    /// Writes one acceptance version per pair, in one batch. No role is needed: the person's
    /// words are the authority.
    pub fn accept_advice(
        &mut self,
        s: &Session,
        req: AcceptRequest,
    ) -> Result<AcceptInfo, RdyError> {
        let (relay, relayed_by) = relay_of(s, &req)?;
        let st = self.read();
        let (pref, changes) = open_version(&st, &req.proposal)?;
        let advised = st.advised_of(&changes)?;
        let chosen = chosen(&advised, &req.tensions, &pref)?;
        let provenance = s.provenance();
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
        // each tension's new label: the tension of the same pair and sides in the new state. The
        // acceptance is written: a set that cannot be read now leaves the old label, not an error
        let now = self
            .read()
            .tensions_of_changes(&changes)
            .unwrap_or_default();
        let accepted = chosen
            .iter()
            .map(|a| {
                let key = a.open.record.key;
                let of_key = |t: &&Arc<Tension>| pair_of(t) == Some(key);
                let new = now
                    .iter()
                    .filter(of_key)
                    .find(|t| t.sides == a.tension.sides && t.involved == a.tension.involved)
                    .or_else(|| now.iter().find(of_key))
                    .map_or_else(|| tension_label(&a.tension.id), |t| tension_label(&t.id));
                AcceptedRow {
                    id: new,
                    was: tension_label(&a.tension.id),
                    version: version_of[&key].to_string(),
                }
            })
            .collect();
        Ok(AcceptInfo {
            proposal: pref.to_string(),
            accepted,
            severity: TensionSeverity::Blocking,
        })
    }
}
