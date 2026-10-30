//! Disputes (D-ZWP7Q "Disputes", D-C8RZW, D-QQCZK, O21): a person opens a dispute of a landing,
//! resolves it, and lists disputes with their state.
//!
//! Opening and resolving are a person's acts (D-KQ4CA), relayed as `rdy view own` is
//! (`views::own`, D-HA7YZ):
//! - no relay input: a human session acts as itself and writes no `ANSWER`, because its own
//!   record holds the act; an agent session is refused (`E_PERMISSION`);
//! - a relay: the relay table ([`relayed`]) decides, the act runs as the person, and one batch
//!   holds the record and an `ANSWER` that holds the relay (the store sets its item, act and
//!   target: `dispute:<landing>` with act `dispute`, or `resolve:<dispute>` with act
//!   `resolve_dispute`).
//!
//! Only the opener withdraws. `upheld` may name a correcting landing of canon's chain. A landing
//! may hold more than one open dispute. A dispute shows and never blocks: no gate, readiness,
//! freshness or landing path reads one.
//!
//! [`run_dispute`] is the one request runner of the MCP tool `rdy_dispute` and the daemon op
//! `dispute`.
use crate::answer::relay::{RelayInput, relayed};
use crate::engine::{Engine, write_typed};
use crate::error::*;
use crate::version::answer_write_error;
use crate::views::own::{RELAY_FIX, answer_of, no_relay};
use crate::{Delta, Rdy, ReadState, Session, rfc3339};
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::{ActorRun, AnswerAct, Hash, Provenance};
use rdy_store::{Relay, kinds};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most rows `list` gives; `omitted` counts the rest.
pub const MAX_LIST: usize = 1_000;

/// One dispute as the surfaces show it (O21). `state` is the dispute's own state: `open`, or its
/// resolution's outcome. A landing's state (O22) is another rule.
#[derive(Clone, Debug, Serialize)]
pub struct DisputeRow {
    pub id: String,
    pub landing: String,
    pub opened_by: ActorRun,
    pub reason: String,
    /// When the dispute was opened, RFC 3339 UTC.
    pub at: String,
    pub state: &'static str,
    pub resolution: Option<ResolutionRow>,
}

/// How a dispute ended.
#[derive(Clone, Debug, Serialize)]
pub struct ResolutionRow {
    pub id: String,
    pub outcome: &'static str,
    pub by: ActorRun,
    /// The correcting landing of an upheld dispute, when one was named.
    pub correction: Option<String>,
}

/// What an open or a resolve did: the dispute as it now stands, and for a relayed act the agent
/// that relayed it and the `ANSWER` that holds the relay.
#[derive(Clone, Debug, Serialize)]
pub struct DisputeAct {
    pub dispute: DisputeRow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<ActorRun>,
    /// The `ANSWER` record's hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// The disputes a list gives, oldest first, and how many more there are.
#[derive(Clone, Debug, Serialize)]
pub struct DisputeList {
    pub disputes: Vec<DisputeRow>,
    pub omitted: usize,
}

/// One `rdy_dispute` / `dispute` request: `op` is `open`, `resolve` or `list`. Unknown keys are
/// refused (`did_you_mean` names a near key).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisputeRequest {
    pub op: String,
    /// open: the landing to dispute; list: only this landing's disputes.
    pub landing: Option<String>,
    /// resolve: the dispute.
    pub dispute: Option<String>,
    /// open: why, 1 to 2000 characters.
    pub reason: Option<String>,
    /// resolve: upheld, dismissed or withdrawn.
    pub outcome: Option<String>,
    /// resolve, upheld only: the correcting landing.
    pub correction: Option<String>,
    /// list: only open disputes.
    pub open: Option<bool>,
    /// open and resolve, at the command line: the relaying agent, its actor alone.
    pub relayed_by: Option<String>,
    /// open and resolve: the person's words, quoted, when an agent relays the act.
    pub evidence: Option<String>,
    /// open and resolve: the question the person answered.
    pub question: Option<String>,
    /// open and resolve: where the person said it (default `mcp` over MCP, `cli` at the command
    /// line).
    pub session_ref: Option<String>,
}

impl DisputeRequest {
    fn relay(&self) -> RelayInput {
        RelayInput {
            relayed_by: self.relayed_by.clone(),
            evidence: self.evidence.clone(),
            question: self.question.clone(),
            session_ref: self.session_ref.clone(),
            kind: None,
        }
    }

    /// The first field, in declaration order, that is set although `op` does not take it.
    fn forbidden(&self) -> Option<&'static str> {
        let relay = ["relayed_by", "evidence", "question", "session_ref"];
        let takes: &[&str] = match self.op.as_str() {
            "open" => &["landing", "reason"],
            "resolve" => &["dispute", "outcome", "correction"],
            _ => &["landing", "open"],
        };
        let person_act = self.op != "list";
        [
            ("landing", self.landing.is_some()),
            ("dispute", self.dispute.is_some()),
            ("reason", self.reason.is_some()),
            ("outcome", self.outcome.is_some()),
            ("correction", self.correction.is_some()),
            ("open", self.open.is_some()),
            ("relayed_by", self.relayed_by.is_some()),
            ("evidence", self.evidence.is_some()),
            ("question", self.question.is_some()),
            ("session_ref", self.session_ref.is_some()),
        ]
        .into_iter()
        .find(|(name, set)| *set && !takes.contains(name) && !(person_act && relay.contains(name)))
        .map(|(name, _)| name)
    }
}

fn bad(message: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

/// The outcome's wire name.
pub fn outcome_str(o: DisputeOutcome) -> &'static str {
    match o {
        DisputeOutcome::Upheld => "upheld",
        DisputeOutcome::Dismissed => "dismissed",
        DisputeOutcome::Withdrawn => "withdrawn",
    }
}

/// The outcome its wire name names.
fn parse_outcome(text: &str) -> Result<DisputeOutcome, RdyError> {
    match text {
        "upheld" => Ok(DisputeOutcome::Upheld),
        "dismissed" => Ok(DisputeOutcome::Dismissed),
        "withdrawn" => Ok(DisputeOutcome::Withdrawn),
        _ => Err(bad(
            format!("unknown outcome {text:?}"),
            "pass upheld, dismissed or withdrawn",
        )),
    }
}

fn to_json<T: Serialize>(v: T) -> Result<Value, RdyError> {
    serde_json::to_value(v).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a dispute reply did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// Runs `req` for session `s`. `person` is the project's human actor, from the caller (O18): a
/// relayed act runs as `person`. Over MCP the request has no `relayed_by`, so the session is the
/// relay.
pub fn run_dispute(
    engine: &dyn Engine,
    s: &Session,
    person: &str,
    req: DisputeRequest,
) -> Result<Value, RdyError> {
    if !matches!(req.op.as_str(), "open" | "resolve" | "list") {
        return Err(bad(
            format!("unknown dispute op {:?}", req.op),
            "use open, resolve or list",
        ));
    }
    if let Some(field) = req.forbidden() {
        return Err(bad(
            format!("{} does not take {field}", req.op),
            format!("drop {field}, or use the dispute operation that takes it"),
        ));
    }
    let (s, person, relay) = (s.clone(), person.to_string(), req.relay());
    match req.op.as_str() {
        "open" => write_typed(engine, move |r| {
            r.dispute_open_as(
                &s,
                &person,
                &relay,
                req.landing.as_deref().unwrap_or(""),
                req.reason.as_deref().unwrap_or(""),
            )
        }),
        "resolve" => write_typed(engine, move |r| {
            let (who, by) = r.person_act(&s, &person, &relay, RESOLVE_REFUSAL)?;
            let dispute = req.dispute.as_deref().ok_or_else(|| {
                bad(
                    "resolve needs dispute",
                    "pass dispute: the dispute's id or a prefix of 6 or more",
                )
            })?;
            let outcome = req.outcome.as_deref().ok_or_else(|| {
                bad(
                    "resolve needs outcome",
                    "pass outcome: upheld, dismissed or withdrawn",
                )
            })?;
            let outcome = parse_outcome(outcome)?;
            r.resolve_as(&who, by, dispute, outcome, req.correction.as_deref())
        }),
        _ => to_json(
            engine
                .read()
                .disputes(req.landing.as_deref(), req.open.unwrap_or(false))?,
        ),
    }
}

/// O21: an agent with no relay.
const OPEN_REFUSAL: &str = "only a person opens a dispute";
const RESOLVE_REFUSAL: &str = "only a person resolves a dispute";

impl Rdy {
    /// The session a person's dispute act runs as, and the relay it records (the module docs):
    /// with no relay input, `s` itself if it is a human session, else `E_PERMISSION` with
    /// `refusal`; else the relay table's answer.
    fn person_act(
        &self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        refusal: &str,
    ) -> Result<(Session, Option<Relay>), RdyError> {
        let itself = || {
            if s.is_human() {
                Ok((s.clone(), None))
            } else {
                Err(RdyError::new(E_PERMISSION, refusal, RELAY_FIX))
            }
        };
        if no_relay(relay)? {
            return itself();
        }
        let implicit = !s.is_human() && relay.relayed_by.is_none();
        match relayed(s, person, relay, implicit).map_err(|e| e.error())? {
            (who, Some(by)) => Ok((who, Some(by))),
            (_, None) => itself(),
        }
    }

    /// Opens a dispute of `landing` (a landing of canon's chain, by full hash or a unique prefix
    /// of 6 or more hex) with `reason`, for the person `person` (the module docs). The reason is
    /// checked with the record's own bound before anything is written.
    pub fn dispute_open_as(
        &mut self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        landing: &str,
        reason: &str,
    ) -> Result<DisputeAct, RdyError> {
        let (who, by) = self.person_act(s, person, relay, OPEN_REFUSAL)?;
        let landing = self.read().resolve_landing(landing)?;
        let opened_by = who.provenance();
        let d = Dispute {
            landing,
            at_ms: opened_by.at_ms,
            opened_by,
            reason: reason.to_string(),
        };
        d.validate().map_err(|_| {
            bad(
                "a dispute reason is 1 to 2000 characters",
                "give the reason in 1 to 2000 characters",
            )
        })?;
        let relayed_by = by.as_ref().map(|b| b.by.clone());
        // the store sets the answer's item, act and target
        let answer = by.map(|b| answer_of(String::new(), AnswerAct::Dispute, &who, b));
        let (id, answer) = self
            .store
            .put_dispute(&d, answer)
            .map_err(answer_write_error)?;
        self.publish(Delta::Seq)?;
        Ok(DisputeAct {
            dispute: self.read().dispute_row(&id)?,
            relayed_by,
            answer: answer.map(|h| h.to_string()),
        })
    }

    /// Resolves `dispute` (its full hash or a unique prefix of 6 or more hex) with `outcome`, for
    /// the person `person` (the module docs). `correction` names a landing of canon's chain and
    /// goes only with `upheld`. Only the opener withdraws, and a dispute has one resolution.
    pub fn dispute_resolve_as(
        &mut self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        dispute: &str,
        outcome: DisputeOutcome,
        correction: Option<&str>,
    ) -> Result<DisputeAct, RdyError> {
        let (who, by) = self.person_act(s, person, relay, RESOLVE_REFUSAL)?;
        self.resolve_as(&who, by, dispute, outcome, correction)
    }

    /// [`Rdy::dispute_resolve_as`] once the person is known.
    fn resolve_as(
        &mut self,
        who: &Session,
        by: Option<Relay>,
        dispute: &str,
        outcome: DisputeOutcome,
        correction: Option<&str>,
    ) -> Result<DisputeAct, RdyError> {
        let st = self.read();
        let id = st.resolve_dispute(dispute)?;
        if correction.is_some() && outcome != DisputeOutcome::Upheld {
            return Err(bad(
                "only an upheld dispute names a correction",
                "drop correction, or resolve it as upheld",
            ));
        }
        if let Some(res) = st.resolution(&id)? {
            return Err(bad(
                format!(
                    "dispute {} is already resolved ({})",
                    id.short(),
                    outcome_str(res.outcome)
                ),
                "open a new dispute",
            ));
        }
        if outcome == DisputeOutcome::Withdrawn {
            let d = st.dispute(&id)?;
            if d.opened_by.actor != who.actor {
                return Err(RdyError::new(
                    E_PERMISSION,
                    format!(
                        "only the person who opened dispute {} withdraws it",
                        id.short()
                    ),
                    "resolve it as upheld or dismissed",
                ));
            }
        }
        let correction = correction.map(|c| st.resolve_landing(c)).transpose()?;
        drop(st);
        let r = DisputeResolution {
            dispute: id,
            outcome,
            by: who.provenance(),
            correction,
        };
        let relayed_by = by.as_ref().map(|b| b.by.clone());
        let answer = by.map(|b| answer_of(String::new(), AnswerAct::ResolveDispute, who, b));
        let (_, answer) = self
            .store
            .put_dispute_resolution(&r, answer)
            .map_err(answer_write_error)?;
        self.publish(Delta::Seq)?;
        Ok(DisputeAct {
            dispute: self.read().dispute_row(&id)?,
            relayed_by,
            answer: answer.map(|h| h.to_string()),
        })
    }
}

fn actor_run(p: &Provenance) -> ActorRun {
    ActorRun {
        actor: p.actor.clone(),
        run: p.run.clone(),
    }
}

/// The first `max` of `keys` in order (oldest first: `at_ms`, then id), and how many more.
fn oldest_first(mut keys: Vec<(i64, Hash)>, max: usize) -> (Vec<Hash>, usize) {
    keys.sort_unstable();
    let omitted = keys.len().saturating_sub(max);
    keys.truncate(max);
    (keys.into_iter().map(|(_, h)| h).collect(), omitted)
}

impl ReadState {
    /// The disputes, oldest first (`at_ms`, then id): those of `landing` (a landing of canon's
    /// chain, as [`ReadState::resolve_landing`] takes it) when given, else all; only the open
    /// ones when `open`. At most [`MAX_LIST`] rows; `omitted` counts the rest. `DisputeRow.state`
    /// is the dispute's own state: `open`, or its resolution's outcome.
    pub fn disputes(&self, landing: Option<&str>, open: bool) -> Result<DisputeList, RdyError> {
        let ids = match landing {
            Some(text) => {
                let l = self.resolve_landing(text)?;
                self.reader.disputes_of(&l, self.seq)?
            }
            None => self.reader.all_disputes(self.seq, usize::MAX)?.0,
        };
        let mut keys = Vec::with_capacity(ids.len());
        for id in ids {
            if open && self.reader.resolution_of(&id, self.seq)?.is_some() {
                continue;
            }
            keys.push((self.dispute(&id)?.at_ms, id));
        }
        let (kept, omitted) = oldest_first(keys, MAX_LIST);
        let disputes = kept
            .iter()
            .map(|id| self.dispute_row(id))
            .collect::<Result<_, _>>()?;
        Ok(DisputeList { disputes, omitted })
    }

    /// A stored dispute, by full hash or a unique prefix of 6 or more hex characters (O21).
    pub fn resolve_dispute(&self, text: &str) -> Result<Hash, RdyError> {
        let not_found = || {
            RdyError::new(
                E_NOT_FOUND,
                format!("no dispute {text}"),
                "list disputes with rdy_dispute {op: \"list\"} / rdy dispute list",
            )
        };
        let want = text.to_ascii_lowercase();
        if !(6..=64).contains(&want.len()) || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(not_found());
        }
        if want.len() == 64 {
            let h: Hash = want.parse().map_err(|_| not_found())?;
            return if self.reader.has_kind_before(kinds::DISPUTE, &h, self.seq)? {
                Ok(h)
            } else {
                Err(not_found())
            };
        }
        let (all, _) = self.reader.all_disputes(self.seq, usize::MAX)?;
        let mut hits = all.into_iter().filter(|h| h.to_string().starts_with(&want));
        match (hits.next(), hits.next()) {
            (Some(h), None) => Ok(h),
            _ => Err(not_found()),
        }
    }

    fn dispute(&self, id: &Hash) -> Result<Dispute, RdyError> {
        Ok(self.reader.get_typed(kinds::DISPUTE, id)?)
    }

    /// The resolution of `id`, None while it is open.
    fn resolution(&self, id: &Hash) -> Result<Option<DisputeResolution>, RdyError> {
        match self.reader.resolution_of(id, self.seq)? {
            Some(r) => Ok(Some(self.reader.get_typed(kinds::DISPUTE_RESOLUTION, &r)?)),
            None => Ok(None),
        }
    }

    /// The row of dispute `id` (O21).
    pub fn dispute_row(&self, id: &Hash) -> Result<DisputeRow, RdyError> {
        let d = self.dispute(id)?;
        let resolution = match self.reader.resolution_of(id, self.seq)? {
            Some(rid) => {
                let r: DisputeResolution =
                    self.reader.get_typed(kinds::DISPUTE_RESOLUTION, &rid)?;
                Some(ResolutionRow {
                    id: rid.to_string(),
                    outcome: outcome_str(r.outcome),
                    by: actor_run(&r.by),
                    correction: r.correction.map(|c| c.to_string()),
                })
            }
            None => None,
        };
        Ok(DisputeRow {
            id: id.to_string(),
            landing: d.landing.to_string(),
            opened_by: actor_run(&d.opened_by),
            reason: d.reason,
            at: rfc3339(d.at_ms),
            state: resolution.as_ref().map_or("open", |r| r.outcome),
            resolution,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> Hash {
        Hash([n; 32])
    }

    #[test]
    fn ap5_a_list_keeps_the_oldest_rows_and_counts_the_rest() {
        // oldest first by time, then by id
        let keys = vec![(5, h(1)), (1, h(9)), (5, h(0)), (3, h(4))];
        assert_eq!(
            oldest_first(keys.clone(), MAX_LIST),
            (vec![h(9), h(4), h(0), h(1)], 0)
        );
        assert_eq!(oldest_first(keys, 2), (vec![h(9), h(4)], 2));
        // MAX_LIST rows, and one more is omitted
        let many: Vec<(i64, Hash)> = (0..=MAX_LIST as i64).map(|i| (i, h(0))).collect();
        let (kept, omitted) = oldest_first(many, MAX_LIST);
        assert_eq!((kept.len(), omitted), (1_000, 1));
    }

    #[test]
    fn ap5_each_op_takes_only_its_own_fields() {
        let req = |v: serde_json::Value| -> DisputeRequest { serde_json::from_value(v).unwrap() };
        assert_eq!(
            req(serde_json::json!({"op": "open", "landing": "a", "reason": "r", "evidence": "e", "question": "q", "session_ref": "s", "relayed_by": "agent:a"})).forbidden(),
            None
        );
        assert_eq!(
            req(serde_json::json!({"op": "open", "outcome": "upheld"})).forbidden(),
            Some("outcome")
        );
        assert_eq!(
            req(serde_json::json!({"op": "resolve", "dispute": "d", "outcome": "upheld", "correction": "c", "evidence": "e"})).forbidden(),
            None
        );
        assert_eq!(
            req(serde_json::json!({"op": "resolve", "landing": "l"})).forbidden(),
            Some("landing")
        );
        assert_eq!(
            req(serde_json::json!({"op": "list", "landing": "l", "open": true})).forbidden(),
            None
        );
        assert_eq!(
            req(serde_json::json!({"op": "list", "evidence": "e"})).forbidden(),
            Some("evidence")
        );
        assert!(
            serde_json::from_value::<DisputeRequest>(serde_json::json!({"op": "list", "x": 1}))
                .is_err()
        );
        assert_eq!(parse_outcome("upheld").unwrap(), DisputeOutcome::Upheld);
        assert_eq!(parse_outcome("Upheld").unwrap_err().code, E_BAD_INPUT);
        assert_eq!(
            ["upheld", "dismissed", "withdrawn"].map(|t| outcome_str(parse_outcome(t).unwrap())),
            ["upheld", "dismissed", "withdrawn"]
        );
    }
}
