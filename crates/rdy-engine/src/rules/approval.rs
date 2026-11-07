//! A rule request through the existing gate (FW-MH.rules, D-ZSRBN): [`run_rule`] is the one entry
//! point of `rdy_rule`.
//!
//! A mutation becomes one complete change and its proposal ([`Rdy::rule_prepare`]). Then, with the
//! policy's `rules_from_chat` at `land`, the person's words are the approval: the landing records
//! a text relay, with no options, no choice and no card hash, because no card was shown. The same
//! words confirm the rule's own supersede or revoke ([`RuleContext::Text`]) and nothing else. With
//! `card`, an open concern on the version, or an exception the words do not cover, the call stops
//! at the normal cards. The answer form answers those cards through the ordinary binding, in the
//! destination, and each answer means what it means on any card.
//!
//! Every preparation runs under the destination's writer, through the signed-act protocol
//! ([`crate::signing_act`]), so a signing retry and a card's answer meet the same checks: the
//! policy from canon, the old decision's other dependents, then the gate's own freshness,
//! readiness and policy role.
//!
//! The failure table of the call:
//!
//! - relay, role, signing or a new dependent, in a mutation: nothing lands and the view closes
//!   with [`RELAY_FAILED`]; a close that cannot write is `corrected.cleanup: "pending"`;
//! - a refusal the next attempt may not meet (a conflict, a moved version, three moved signing
//!   attempts) or an exception the words do not cover: the view and the proposal stay, and
//!   `corrected` names them;
//! - any refusal of an answer: the proposal stays;
//! - a landing stands whatever its cleanup does: trouble there is a warning, and a repeat
//!   finishes it. A repeat returns the stored result and never lands twice.
use super::lifecycle::{self, StoredRule};
use super::{Destination, Prepared, PreparedState, RELAY_FAILED, RuleCaller, RuleMutation};
use crate::answer::relay::{RelayInput, relayed};
use crate::answer::{AnswerStep, RuleContext, changed, rule_answerer};
use crate::check::verdicts::{matching_verdicts, open_hold};
use crate::engine::{Engine, with_writer_result};
use crate::error::*;
use crate::gate::{
    LandOptions, PreparedApproval, ProposalRef, landed_as, latest_version, open_proposal_of_view,
    ref_hash, resolve_in, version_number,
};
use crate::pending::PendingQuery;
use crate::pending::queue::{answered, approval_holds, only_against};
use crate::pending::{CARD_MAX_TOKENS, Item};
use crate::rule_types::{
    AnswerRule, CardEntry, Outcome, ProjectInfo, RelayWords, Reply, RuleInfo, RuleRequest,
    Scope as RuleScope, other_flow,
};
use crate::signature_check::signature_checks;
use crate::signing_act::run_rule_act;
use crate::signing_crypto::SigningAgent;
use crate::{Rdy, ReadState, Scope, Session};
use rdy_model::{ActorRun, AnswerAct, ApprovalEvidence, EvidenceKind, Hash, RelayEvidence, Role};
use rdy_schema::policy::RulesFromChat;
use rdy_schema::{Issue, Severity};
use rdy_store::{Relay, StoreError};
use serde_json::{Value, json};

/// The member that marks a reply of [`RuleStep::Stop`]: no reply of a landing or an answer has it.
const STOP: &str = "rule_stop";

/// Whether `v` is a stop rather than an act's reply.
pub(crate) fn is_stop(v: &Value) -> bool {
    v.get(STOP).is_some()
}

/// Who makes a rule call, in the destination project. A daemon or a direct host builds it from the
/// session it holds. The person is the destination's configured human.
#[derive(Clone, Debug)]
pub struct RuleCall {
    /// The caller's own session: it owns the view, writes the change and relays.
    pub session: Session,
    /// The configured human whose words the caller relays.
    pub person: String,
    /// The session reference a request without one reads: `mcp` or `cli`.
    pub surface: String,
    pub destination: Destination,
    /// The agent a command line names; absent over MCP, where the session is the relay.
    pub relayed_by: Option<String>,
}

impl RuleCall {
    fn caller(&self) -> RuleCaller<'_> {
        RuleCaller {
            session: &self.session,
            person: &self.person,
            surface: &self.surface,
            destination: self.destination,
        }
    }

    /// The relay table's input for `words`, with the chat session `session_ref`.
    fn relay_input(&self, words: &RelayWords, session_ref: Option<String>) -> RelayInput {
        RelayInput {
            relayed_by: self.relayed_by.clone(),
            evidence: Some(words.evidence.clone()),
            question: words.question.clone(),
            session_ref,
            kind: None,
        }
    }
}

/// What a rule call returns.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleResult {
    pub reply: Reply,
    /// The warnings of a call that stopped short of a landing. A landing holds its own.
    pub warnings: Vec<Issue>,
}

impl RuleResult {
    /// The reply as the tool returns it.
    pub fn to_json(&self) -> Value {
        let mut v = self.reply.to_json();
        if !self.warnings.is_empty() {
            v["warnings"] = serde_json::to_value(&self.warnings).unwrap_or(Value::Null);
        }
        v
    }
}

/// One act of the signed-act protocol in a rule's flow. It holds no input a caller can name, and
/// does not deserialize.
#[derive(Clone)]
pub(crate) enum RuleAct {
    Approval(RuleApproval),
    Answer(RuleCardAnswer),
}

/// The person's words as the approval of the proposal `reference`.
#[derive(Clone)]
pub(crate) struct RuleApproval {
    scope: RuleScope,
    reference: String,
    view: String,
    relay: Relay,
    relay_input: RelayInput,
}

/// The person's answer to a card of a rule's proposal.
#[derive(Clone)]
pub(crate) struct RuleCardAnswer {
    scope: RuleScope,
    item: String,
    option: String,
    card_hash: String,
    relayed_by: Option<String>,
    words: RelayWords,
}

/// What the first step of a rule's act did.
pub(crate) enum RuleStep {
    /// The act ends here: a stop, as [`is_stop`] marks it.
    Stop(Value),
    /// The approval is prepared, and waits for its signatures.
    Landing(Box<PreparedApproval>),
    /// The answer's own step.
    Answer(AnswerStep),
}

fn stop(kind: &str, mut v: Value) -> RuleStep {
    v[STOP] = json!(kind);
    RuleStep::Stop(v)
}

fn entry(i: &Item) -> Value {
    json!({
        "ref": i.r#ref,
        "card_hash": i.card_hash,
        "card": serde_json::to_value(i).unwrap_or(Value::Null),
    })
}

fn entries_of(v: &Value) -> Vec<CardEntry> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|e| CardEntry {
            item_ref: e["ref"].as_str().unwrap_or_default().to_string(),
            card_hash: e["card_hash"].as_str().unwrap_or_default().to_string(),
            card: e["card"].clone(),
        })
        .collect()
}

/// What the policy in canon says about a rule from chat. The draft never decides this: it is
/// always read from canon. A policy that does not read is no absent policy: `E_VALIDATION` with
/// its issues.
pub(super) fn policy_mode(st: &ReadState) -> Result<RulesFromChat, RdyError> {
    let canon = st.materialized(&Scope::Canon)?;
    let Some(live) = crate::policy::live_policy(st, &canon) else {
        return Ok(RulesFromChat::Land);
    };
    if let Some(fields) = &live.fields {
        return Ok(fields.rules_from_chat);
    }
    let block = canon.get(&live.block);
    let parent = block
        .and_then(|b| b.parent.as_ref())
        .and_then(|p| canon.get(p));
    let issues = block.map_or_else(Vec::new, |b| {
        rdy_schema::validate_block(st.pack(), b, parent)
    });
    let (message, fix) = issues
        .iter()
        .find(|i| i.severity == Severity::Error)
        .map(|i| (i.message.clone(), i.fix.clone()))
        .unwrap_or_else(|| {
            (
                format!("the authority policy {} does not validate", live.block),
                "fix the authority policy".to_string(),
            )
        });
    let mut e = RdyError::new(E_VALIDATION, message, fix);
    e.issues = issues;
    Err(e)
}

/// The relay of `words`: the relay table's checks, then the exact words it trims.
fn exact_relay(call: &RuleCall, words: &RelayWords) -> Result<Relay, RdyError> {
    let input = call.relay_input(words, words.session_ref.clone());
    let implicit = call.relayed_by.is_none();
    let (_, relay) =
        relayed(&call.session, &call.person, &input, implicit).map_err(|e| e.error())?;
    let Some(relay) = relay else {
        // a human session with evidence never gets here: the table refuses it
        return Err(RdyError::new(
            E_PERMISSION,
            "an agent answers only by relaying a person's words",
            "ask the person, then pass their words as evidence and your question",
        ));
    };
    Ok(Relay {
        by: relay.by,
        evidence: RelayEvidence {
            quoted: words.evidence.clone(),
            session_ref: words
                .session_ref
                .clone()
                .unwrap_or_else(|| call.surface.clone()),
            question: words.question.clone(),
        },
    })
}

fn project_info(st: &ReadState) -> Result<ProjectInfo, RdyError> {
    Ok(ProjectInfo {
        name: st.project_name(),
        repo_id: st.repo_id()?.to_string(),
    })
}

/// The refusal of the rule's old decision when other blocks rest on it now, in canon or in another
/// open view: read at this preparation, never from the draft or the stored assessment.
fn check_dependents(
    st: &ReadState,
    scope: RuleScope,
    view: &str,
    content: &rdy_model::Change,
) -> Result<(), RdyError> {
    let (Some((_, old, _)), Some(rule)) = (
        lifecycle::replaced(content),
        lifecycle::target_rule(content),
    ) else {
        return Ok(());
    };
    let others = super::dependents(st, &old, rule, view)?;
    if others.is_empty() {
        return Ok(());
    }
    Err(super::shared_refusal(st, scope, &old, others)?)
}

impl Rdy {
    /// The first step of a rule's act, under the writer, at every attempt.
    pub(crate) fn rule_step(
        &mut self,
        s: &Session,
        person: &str,
        act: &RuleAct,
    ) -> Result<RuleStep, RdyError> {
        match act {
            RuleAct::Approval(a) => self.rule_approval_step(s, person, a),
            RuleAct::Answer(a) => self.rule_card_step(s, person, a),
        }
    }

    /// The person's words as the approval of `a`'s proposal (module docs).
    fn rule_approval_step(
        &mut self,
        s: &Session,
        person: &str,
        a: &RuleApproval,
    ) -> Result<RuleStep, RdyError> {
        let st = self.read();
        let pref = resolve_in(&st, &a.reference)?;
        let p = st.reader().proposal(&pref.version)?;
        // a competing retry may have landed it: that landing is this request's result
        if let Some(landing) = landed_as(&st, &p)? {
            return Ok(stop("landed", json!({"landing": landing.to_string()})));
        }
        if policy_mode(&st)? == RulesFromChat::Card {
            drop(st);
            return self.rule_cards_stop(person, &pref);
        }
        // the exact proposal of the checked request: its receipt and its change set, again
        let content = lifecycle::stored_rule(&st, &a.view)?
            .filter(|rule| p.view == a.view && p.changes.first() == Some(&rule.first))
            .ok_or_else(|| lifecycle::changed(&a.view))?
            .content;
        check_dependents(&st, a.scope, &a.view, &content)?;
        // the person refused this version on its card: their words on the same request do not
        // bring it back
        if answered(&st, &pref.to_string(), AnswerAct::RejectProposal)? {
            return Err(changed(E_ITEM_MOVED, &a.reference));
        }
        // only card evidence binds a verdict: text never lands over an open concern
        let verdicts = matching_verdicts(&st, &pref, &p)?;
        if open_hold(&st, &pref, &verdicts)? {
            drop(st);
            return self.rule_cards_stop(person, &pref);
        }
        drop(st);
        let approver = Session::human(person.to_string(), Role::Approver)?;
        let options = LandOptions {
            strict: false,
            relay: Some(a.relay.clone()),
            evidence: Some(ApprovalEvidence {
                kind: EvidenceKind::Text,
                options: vec![],
                choice: None,
                card_hash: None,
            }),
        };
        let landing = match self.prepare_landing(&approver, &a.reference, options.clone()) {
            Err(e) if e.code == E_EXCEPTION_OPEN => {
                self.rule_confirm(s, person, a, &pref, &content, e)?;
                self.prepare_landing(&approver, &a.reference, options)
            }
            other => other,
        };
        match landing {
            Ok(l) => Ok(RuleStep::Landing(Box::new(l))),
            Err(e) if e.code == E_PROPOSAL_NOT_READY => Ok(not_ready_stop(&a.reference, e.issues)),
            Err(e) => Err(e),
        }
    }

    /// Confirms the rule's own supersede or revoke with the person's words, when that is all that
    /// holds the version back. Anything else keeps `refusal`, and nothing is confirmed.
    fn rule_confirm(
        &mut self,
        s: &Session,
        person: &str,
        a: &RuleApproval,
        pref: &ProposalRef,
        content: &rdy_model::Change,
        refusal: RdyError,
    ) -> Result<(), RdyError> {
        let Some((decision, old, word)) = lifecycle::replaced(content) else {
            return Err(refusal);
        };
        let open = approval_holds(&self.read(), pref, true)?.open;
        let detail = format!("{word} {old}");
        let own = format!("{decision}@");
        if !open
            .iter()
            .all(|i| i.r#ref.starts_with(&own) && only_against(i, &detail))
        {
            return Err(refusal);
        }
        let input = RelayInput {
            kind: Some(EvidenceKind::Text),
            ..a.relay_input.clone()
        };
        let context = RuleContext::Text {
            proposal: pref.to_string(),
            decision,
        };
        let by = rule_answerer(s, person, &input, context)?;
        for item in &open {
            self.rule_answer_step(&by, person, &item.r#ref, "confirm", None)?;
        }
        Ok(())
    }

    /// The cards of `pref`, as the queue shows them: a card stop. A version that is not ready has
    /// none: its issues are the stop. A version the person refused has left the queue: the normal
    /// refusal of a moved item.
    fn rule_cards_stop(&mut self, person: &str, pref: &ProposalRef) -> Result<RuleStep, RdyError> {
        let r = pref.to_string();
        // an assessment the project moved past gets its fresh card first (RULING F1)
        match self.check_fresh(&r) {
            Err(e) if ![E_PROPOSAL_MOVED, E_PROPOSAL_NOT_READY].contains(&e.code) => return Err(e),
            _ => {}
        }
        let st = self.read();
        let q = st.eligible(&query(person))?;
        let items = q.batch_items(&r)?;
        if let Some((first, rest)) = items.split_first().filter(|(f, _)| f.r#ref == r) {
            let cards: Vec<Value> = rest.iter().map(entry).collect();
            return Ok(stop("card", json!({"entry": entry(first), "cards": cards})));
        }
        match q.not_ready.iter().find(|n| n.r#ref == r) {
            Some(n) => Ok(not_ready_stop(&r, n.issues.clone())),
            None => Err(changed(E_ITEM_MOVED, &r)),
        }
    }

    /// The person's answer to a card of `a`'s proposal: its receipt checked, then an exact earlier
    /// answer's result, then the dependents for an approval, then the ordinary binding and act.
    fn rule_card_step(
        &mut self,
        s: &Session,
        person: &str,
        a: &RuleCardAnswer,
    ) -> Result<RuleStep, RdyError> {
        let st = self.read();
        let resolved = resolve_item(&st, &a.item)?;
        if let Some(v) = recorded(&st, s, person, a, &resolved)? {
            return Ok(RuleStep::Stop(v));
        }
        if a.option == "approve" {
            check_dependents(&st, a.scope, &resolved.rule.view, &resolved.rule.content)?;
        }
        drop(st);
        let input = RelayInput {
            relayed_by: a.relayed_by.clone(),
            evidence: Some(a.words.evidence.clone()),
            question: a.words.question.clone(),
            // the original request's chat session names the answer's
            session_ref: Some(resolved.rule.session_ref.clone()),
            kind: None,
        };
        let context = RuleContext::Card {
            proposal: resolved.pref.to_string(),
        };
        let by = rule_answerer(s, person, &input, context)?;
        Ok(RuleStep::Answer(self.rule_answer_step(
            &by,
            person,
            &a.item,
            &a.option,
            Some(&a.card_hash),
        )?))
    }
}

fn query(person: &str) -> PendingQuery {
    PendingQuery {
        person: person.to_string(),
        session_ref: None,
        max_tokens: CARD_MAX_TOKENS,
    }
}

fn not_ready_stop(r: &str, issues: Vec<Issue>) -> RuleStep {
    stop(
        "not_ready",
        json!({
            "ref": r,
            "issues": serde_json::to_value(&issues).unwrap_or(Value::Array(vec![])),
        }),
    )
}

/// An answer's item, resolved to the request that its proposal's receipt names.
struct Resolved {
    rule: StoredRule,
    pref: ProposalRef,
}

/// The proposal and the verified request of item `item` (`P-x@n`, `defer:P-x@n`, `advice:P-x@n` or
/// `<D-id>@<version>`). Anything that is not this flow's is the other-flow refusal: an item of
/// another destination has no proposal here.
fn resolve_item(st: &ReadState, item: &str) -> Result<Resolved, RdyError> {
    let proposal = item
        .strip_prefix("defer:")
        .or_else(|| item.strip_prefix("advice:"))
        .unwrap_or(item);
    if proposal.starts_with("P-") {
        let pref = match resolve_in(st, proposal) {
            Ok(pref) => pref,
            Err(e) if e.code == E_NOT_FOUND || e.code == E_BAD_INPUT => {
                return Err(other_flow(item));
            }
            Err(e) => return Err(e),
        };
        let p = st.reader().proposal(&pref.version)?;
        let rule = lifecycle::stored_rule(st, &p.view)?
            .filter(|r| p.changes.first() == Some(&r.first))
            .ok_or_else(|| other_flow(item))?;
        return Ok(Resolved { rule, pref });
    }
    let id = item.split_once('@').map_or(item, |(id, _)| id);
    let rule = lifecycle::rule_of_decision(st, id)?.ok_or_else(|| other_flow(item))?;
    let pref = rule_proposal(st, &rule)?;
    Ok(Resolved { rule, pref })
}

/// The proposal of a verified request: the one that landed, or the view's open one.
fn rule_proposal(st: &ReadState, rule: &StoredRule) -> Result<ProposalRef, RdyError> {
    let reader = st.reader();
    if let Some(landing) = reader.landing_of(&rule.first)? {
        let rec = reader.landing(&landing)?;
        let version = rec.approved_version.unwrap_or(rec.proposal);
        return resolve_in(st, &version.to_string());
    }
    if open_proposal_of_view(st, &rule.view)?.is_none() {
        return Err(StoreError::ViewClosed(rule.view.clone()).into());
    }
    let open = reader
        .get_ref(&format!("open/{}", rule.view))?
        .ok_or_else(|| StoreError::ViewClosed(rule.view.clone()))?;
    let stable = ref_hash(&open, "an open ref")?;
    let version = latest_version(st, &stable)?;
    Ok(ProposalRef {
        version,
        stable,
        n: version_number(st, &version)?,
    })
}

/// The result of an exact earlier answer: the same item, option, card hash, caller and words. Any
/// other answer goes on to the ordinary freshness and role checks.
fn recorded(
    st: &ReadState,
    s: &Session,
    person: &str,
    a: &RuleCardAnswer,
    resolved: &Resolved,
) -> Result<Option<Value>, RdyError> {
    let reader = st.reader();
    let by = ActorRun {
        actor: s.actor.clone(),
        run: s.run.clone(),
    };
    let same_words = |e: Option<&RelayEvidence>| {
        e.is_some_and(|e| e.quoted == a.words.evidence && e.question == a.words.question)
    };
    if a.option == "approve" {
        let p = reader.proposal(&resolved.pref.version)?;
        let Some(landing) = landed_as(st, &p)? else {
            return Ok(None);
        };
        let rec = reader.landing(&landing)?;
        let same = rec.approved_version == Some(resolved.pref.version)
            && rec.relayed_by.as_ref() == Some(&by)
            && same_words(rec.relay_evidence.as_ref())
            && rec.evidence.as_ref().is_some_and(|e| {
                e.kind == EvidenceKind::Card
                    && e.card_hash.map(|h| h.to_string()).as_deref() == Some(a.card_hash.as_str())
            });
        return Ok(same.then(|| json!({STOP: "landed", "landing": landing.to_string()})));
    }
    let found = if a.option == "later" {
        reader.snooze_of(person, &a.item)?
    } else if let Some((review, _)) = crate::check::verdicts::parse_accept_option(&a.option) {
        let item = format!("{}{review}", crate::check::verdicts::VERDICT_ITEM);
        reader.answer_of(&item)?
    } else {
        reader.answer_of(&a.item)?
    };
    let Some((h, rec)) = found else {
        return Ok(None);
    };
    let same = rec.option.as_deref() == Some(a.option.as_str())
        && rec.relayed_by.as_ref() == Some(&by)
        && same_words(rec.relay_evidence.as_ref())
        && rec.card_hash.map(|c| c.to_string()).as_deref() == Some(a.card_hash.as_str());
    Ok(same.then(|| {
        json!({
            STOP: "answered",
            "answer": {
                "ref": a.item,
                "act": rec.act.as_str(),
                "option": a.option,
                "label": rec.label,
                "assurance": "relayed",
                "answer": h.to_string(),
                "landing": null,
                "relayed_by": by,
                "next": null,
            },
        })
    }))
}

/// Runs one `rdy_rule` request in the destination project `engine` serves (module docs). The
/// caller's `agent` signs, with no writer held.
pub fn run_rule(
    engine: &dyn Engine,
    agent: &dyn SigningAgent,
    call: &RuleCall,
    request: &RuleRequest,
) -> Result<RuleResult, RdyError> {
    match request {
        RuleRequest::Answer(a) => run_answer(engine, agent, call, a),
        _ => run_mutation(engine, agent, call, request),
    }
}

/// The JSON of the stored landing `landing`, with its signatures and consequences, as a repeat
/// returns it.
fn stored_landing(engine: &dyn Engine, landing: &str) -> Result<Value, RdyError> {
    let st = engine.read();
    let h: Hash = landing.parse().map_err(|_| {
        RdyError::new(
            E_INTERNAL,
            format!("{landing} is not a landing"),
            "report this as a bug in rdy",
        )
    })?;
    let rec = st.reader().landing(&h)?;
    let mut v = serde_json::to_value(st.stored_land_info(&h)?).map_err(internal_json)?;
    v["relayed_by"] = json!(rec.relayed_by);
    v["signatures"] =
        serde_json::to_value(signature_checks(&st, &h, None)?).map_err(internal_json)?;
    drop(st);
    crate::consequence::report_approval(engine, &mut v);
    Ok(v)
}

fn internal_json(e: serde_json::Error) -> RdyError {
    RdyError::new(
        E_INTERNAL,
        format!("a rule reply did not serialize: {e}"),
        "report this as a bug in rdy",
    )
}

/// Closes the landed view, and adds any trouble to the landing's warnings: it stands regardless.
fn finish_landing(
    engine: &dyn Engine,
    call: &RuleCall,
    view: &str,
    landing: &mut Value,
    extra: Vec<Issue>,
) {
    let (s, name) = (call.session.clone(), view.to_string());
    let cleanup = with_writer_result(engine, move |r| Ok(r.rule_close_landed(&s, &name)));
    let trouble = match cleanup {
        Ok(warning) => warning,
        Err(e) => Some(Issue {
            severity: Severity::Warning,
            code: e.code,
            block: None,
            message: format!("the landed view {view} did not close: {}", e.message),
            fix: format!("close the view with rdy view close {view}"),
        }),
    };
    add_warnings(landing, &extra);
    add_warnings(landing, trouble.as_slice());
}

/// Adds `issues` after the warnings `landing` holds.
fn add_warnings(landing: &mut Value, issues: &[Issue]) {
    let added: Vec<Value> = issues
        .iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    if added.is_empty() {
        return;
    }
    match landing.get_mut("warnings").and_then(Value::as_array_mut) {
        Some(list) => list.extend(added),
        None => landing["warnings"] = Value::Array(added),
    }
}

/// `e`, with `add`'s members in its `corrected` beside what it holds.
fn with_corrected(mut e: RdyError, add: Value) -> RdyError {
    let mut corrected = match e.corrected.take() {
        Some(Value::Object(o)) => o,
        _ => serde_json::Map::new(),
    };
    if let Value::Object(add) = add {
        corrected.extend(add);
    }
    e.corrected = Some(Value::Object(corrected));
    e
}

/// Whether a mutation's refusal at the approval ends its automatic flow: the relay, the role, the
/// signing or a new dependent refused it. Any other refusal can pass on a retry, and keeps the
/// proposal.
fn ends_flow(code: &str) -> bool {
    [
        E_PERMISSION,
        E_BAD_INPUT,
        E_LIMIT,
        E_NO_AGENT,
        E_SIGNING_KEY_MISSING,
        E_SIGNING_KEY_TYPE,
    ]
    .contains(&code)
}

fn run_mutation(
    engine: &dyn Engine,
    agent: &dyn SigningAgent,
    call: &RuleCall,
    request: &RuleRequest,
) -> Result<RuleResult, RdyError> {
    let (c, q) = (call.clone(), request.clone());
    let (prepared, relay) = with_writer_result(engine, move |r| {
        let Some(m) = RuleMutation::of(&q) else {
            return Err(RdyError::new(
                E_INTERNAL,
                "an answer reached the rule draft",
                "report this as a bug in rdy",
            ));
        };
        let relay = exact_relay(&c, m.relay())?;
        Ok((r.rule_prepare(&c.caller(), m)?, relay))
    })?;
    let Prepared {
        scope,
        project,
        rule,
        decision,
        view,
        state,
        warnings,
    } = prepared;
    let reply = |outcome: Outcome| RuleResult {
        reply: Reply {
            scope,
            project: project.clone(),
            rule: rule.clone(),
            decision: decision.clone(),
            outcome,
        },
        warnings: vec![],
    };
    let reference = match state {
        PreparedState::Landed { landing } => {
            // the repeat already finished the cleanup, and its warnings say how that went
            let mut v = stored_landing(engine, &landing)?;
            add_warnings(&mut v, &warnings);
            return Ok(reply(Outcome::Landed { landing: v }));
        }
        PreparedState::Proposed { reference, .. } => reference,
    };
    let Some(m) = RuleMutation::of(request) else {
        return Err(RdyError::new(
            E_INTERNAL,
            "an answer reached the rule approval",
            "report this as a bug in rdy",
        ));
    };
    let act = RuleAct::Approval(RuleApproval {
        scope,
        reference: reference.clone(),
        view: view.clone(),
        relay_input: call.relay_input(m.relay(), Some(relay.evidence.session_ref.clone())),
        relay,
    });
    let done = run_rule_act(engine, agent, &call.session, &call.person, act);
    let v = match done {
        Ok(v) => v,
        Err(e) if ends_flow(e.code) => {
            let landing = close_failed(engine, call, &view, &reference, e)?;
            let mut landing = stored_landing(engine, &landing.to_string())?;
            finish_landing(engine, call, &view, &mut landing, warnings);
            return Ok(reply(Outcome::Landed { landing }));
        }
        Err(e) => {
            return Err(with_corrected(
                e,
                json!({"view": view, "proposal_ref": reference, "ref": reference}),
            ));
        }
    };
    let mut result = match v.get(STOP).and_then(Value::as_str) {
        None => {
            let mut landing = v;
            landing["relayed_by"] = json!(ActorRun {
                actor: call.session.actor.clone(),
                run: call.session.run.clone(),
            });
            finish_landing(engine, call, &view, &mut landing, warnings);
            return Ok(reply(Outcome::Landed { landing }));
        }
        Some("landed") => {
            let landing = v["landing"].as_str().unwrap_or_default().to_string();
            let mut landing = stored_landing(engine, &landing)?;
            finish_landing(engine, call, &view, &mut landing, warnings);
            return Ok(reply(Outcome::Landed { landing }));
        }
        Some("card") => reply(Outcome::Card {
            view,
            proposal_ref: reference.clone(),
            item_ref: reference,
            card_hash: v["entry"]["card_hash"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            card: v["entry"]["card"].clone(),
            cards: entries_of(&v["cards"]),
        }),
        Some(_) => reply(Outcome::NotReady {
            view,
            item_ref: reference,
            issues: v["issues"].as_array().cloned().unwrap_or_default(),
        }),
    };
    result.warnings = warnings;
    Ok(result)
}

/// A mutation whose relay, role or signing refused: its view closes through the normal close, and
/// the refusal stands. A competing retry that landed meanwhile wins: nothing withdraws it, and its
/// landing is what this returns.
fn close_failed(
    engine: &dyn Engine,
    call: &RuleCall,
    view: &str,
    reference: &str,
    e: RdyError,
) -> Result<Hash, RdyError> {
    let (s, name, r2, fallback) = (
        call.session.clone(),
        view.to_string(),
        reference.to_string(),
        e.clone(),
    );
    let closed = with_writer_result(engine, move |r| {
        let st = r.read();
        let pref = resolve_in(&st, &r2)?;
        let p = st.reader().proposal(&pref.version)?;
        if let Some(landing) = landed_as(&st, &p)? {
            return Ok(Ok(landing));
        }
        drop(st);
        Ok(Err(r.rule_close_failed(&s, &name, RELAY_FAILED, e)))
    });
    match closed {
        Ok(outcome) => outcome,
        Err(_) => Err(with_corrected(
            fallback,
            json!({"view": view, "cleanup": "pending"}),
        )),
    }
}

/// The rule and the decision a reply names, as the view or canon holds them now.
fn described(
    st: &ReadState,
    rule: &StoredRule,
    landed: bool,
) -> Result<(RuleInfo, String), RdyError> {
    lifecycle::describe(st, &rule.view, &rule.content, landed)
}

fn run_answer(
    engine: &dyn Engine,
    agent: &dyn SigningAgent,
    call: &RuleCall,
    a: &AnswerRule,
) -> Result<RuleResult, RdyError> {
    if !call.session.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot write", call.session.role),
            "start the session as a writer",
        ));
    }
    let act = RuleAct::Answer(RuleCardAnswer {
        scope: a.scope,
        item: a.item_ref.clone(),
        option: a.option.clone(),
        card_hash: a.card_hash.to_string(),
        relayed_by: call.relayed_by.clone(),
        words: a.relay.clone(),
    });
    let v = match run_rule_act(engine, agent, &call.session, &call.person, act) {
        Ok(v) => v,
        Err(e) if e.code == E_PROPOSAL_NOT_READY => return Err(not_ready_answer(engine, a, e)),
        Err(e) => return Err(e),
    };
    let st = engine.read();
    let Resolved { rule, pref } = resolve_item(&st, &a.item_ref)?;
    let project = project_info(&st)?;
    let proposal_ref = pref.to_string();
    let landing = match v.get(STOP).and_then(Value::as_str) {
        Some("landed") => {
            let landing = v["landing"].as_str().unwrap_or_default().to_string();
            drop(st);
            Some(stored_landing(engine, &landing)?)
        }
        _ if v.get("landing").is_some_and(Value::is_object) => {
            drop(st);
            let mut landing = v["landing"].clone();
            landing["signatures"] = v["signatures"].clone();
            landing["relayed_by"] = v["relayed_by"].clone();
            Some(landing)
        }
        _ => None,
    };
    if let Some(mut landing) = landing {
        finish_landing(engine, call, &rule.view, &mut landing, vec![]);
        let st = engine.read();
        let (info, decision) = described(&st, &rule, true)?;
        return Ok(RuleResult {
            reply: Reply {
                scope: a.scope,
                project,
                rule: info,
                decision,
                outcome: Outcome::Landed { landing },
            },
            warnings: vec![],
        });
    }
    let st = engine.read();
    // a repeat can find its view landed or closed: canon then names the rule
    let settled = st.reader().landing_of(&rule.first)?.is_some()
        || st.view_head(&rule.view).is_none_or(|h| h.closed);
    let (info, decision) = described(&st, &rule, settled)?;
    let remaining = st
        .eligible(&query(&call.person))?
        .batch_items(&proposal_ref)?;
    let answer = if is_stop(&v) { v["answer"].clone() } else { v };
    Ok(RuleResult {
        reply: Reply {
            scope: a.scope,
            project,
            rule: info,
            decision,
            outcome: Outcome::Answered {
                view: rule.view.clone(),
                proposal_ref,
                item_ref: a.item_ref.clone(),
                answer,
                remaining_cards: entries_of(&Value::Array(remaining.iter().map(entry).collect())),
            },
        },
        warnings: vec![],
    })
}

/// A card's approval of a version that is not ready now keeps its refusal and adds where it stands:
/// the destination and the ref. The pending view stays.
fn not_ready_answer(engine: &dyn Engine, a: &AnswerRule, e: RdyError) -> RdyError {
    let st = engine.read();
    let Ok(Resolved { rule, pref }) = resolve_item(&st, &a.item_ref) else {
        return e;
    };
    let Ok(project) = project_info(&st) else {
        return e;
    };
    with_corrected(
        e,
        json!({
            "scope": a.scope.as_str(),
            "project": {"name": project.name, "repo_id": project.repo_id},
            "view": rule.view,
            "proposal_ref": pref.to_string(),
            "ref": a.item_ref,
        }),
    )
}
