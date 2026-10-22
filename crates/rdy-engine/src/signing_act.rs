//! Signed acts (D-1Z951, D-17AA8, D-QVH4N): an approval or a tag and its signatures are written
//! in one batch, and a signature is always of the exact record that batch writes.
//!
//! # The protocol
//!
//! 1. Under the writer, the act's existing validation runs and its complete prospective record
//!    is prepared without writing (the landing with its acceptance work and frames, or the tag
//!    and its relay's final `ANSWER`), with every clock value it uses already read. The store's
//!    log end, the loaded pack, the signing settings and the redaction epoch are captured. When
//!    the settings ask for no signature, the act is committed in that same write.
//! 2. With the writer free, the caller's agent signs the statements the mode table asks for,
//!    each built from the prospective record. Each new signature is checked once against that
//!    record; a reply that does not verify for the requested key is `E_NO_AGENT` `invalid reply`.
//!    Trust is advisory and never read here.
//! 3. Under the writer again, a removal's report is checked again first (`E_REDACTION_STALE`
//!    returns at once, never retried). Then a moved log, pack, setting or epoch discards the
//!    preparation and its signatures and starts again from step 1, at most three attempts in all
//!    (`E_STALE_BASE` after the third). The request and its evidence are reused as given.
//! 4. The act's existing commit writes the act and, after its subject and the tag's `ANSWER`,
//!    its signatures, and the existing seal and publish follow.
//! 5. Consequences and the result's `signatures` (relay before person) are attached after the
//!    commit.
//!
//! # The mode table (closed)
//!
//! The act is classified by the asking session's kind and the prepared record. A human's own
//! act signs with `signing.key`; a relayed act signs its relay with `signing.agent_key`, then,
//! with `signing.key` set, the person's statement too. Under `auto` an unset key asks no agent,
//! and the first failure saves the act unsigned with one `W_SIGNING_FAILED`; under `always` a
//! failure writes nothing. A policy approval, an agent's own unrelayed act and an answer that
//! makes no landing or tag are never signed.
//!
//! # Later signing
//!
//! [`run_sign`] signs a stored landing or tag: a human their own act, an agent a relayed one
//! (its relay by default; the person's statement with the machine key's relay beside it).
use crate::allowed_signers::{AllowedSigners, read_allowed_signers};
use crate::answer::relay::RelayInput;
use crate::answer::{AnswerInfo, AnswerRequest, AnswerStep, Answerer, PreparedAnswer};
use crate::config_levels::{effective, user_file_from_env};
use crate::engine::{Engine, with_writer_result};
use crate::gate::{LandInfo, LandOptions, PreparedApproval, ProposalRef};
use crate::signature_check::{
    CheckContext, SIGNED_VALID, STATEMENT_MATCH, SignatureCheck, SignatureSubject, check_signature,
    repo_ids, state_signatures, subject_context,
};
use crate::signing_config::{SignMode, SigningConfig};
use crate::signing_crypto::{key_fingerprint, resolve_key, sign_sshsig};
use crate::version::{PreparedTagAct, TagInfo, landing_spelling, parse_version};
use crate::{
    E_BAD_INPUT, E_INTERNAL, E_NOT_FOUND, E_PERMISSION, E_STALE_BASE, NoAgentCause, Rdy, RdyError,
    Session, W_SIGNING_FAILED, no_agent, signing_failed, signing_key_missing, signing_limit,
    signing_selector,
};
use rdy_model::signature::{Signature, StatementError, StatementKind, StatementSource, statement};
use rdy_model::{Answer, Assurance, Hash, Provenance, RepoId, Role, Tag};
use rdy_schema::{Issue, Pack, Severity};
use rdy_store::StoreError;
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// How many times an act is prepared and signed before a moving project ends it.
const ATTEMPTS: usize = 3;

/// The setting of the person's key.
const PERSON_KEY: &str = "signing.key";
/// The setting of the machine agent's key.
const AGENT_KEY: &str = "signing.agent_key";

/// An act that may be signed, as its callers already name it.
#[derive(Clone, Debug)]
pub enum SigningAct {
    /// An approval of `proposal` (`land_with`); with `options.relay`, the person's approval
    /// relayed by the asking session.
    Approve {
        proposal: String,
        options: LandOptions,
    },
    /// One answer of the person's queue (`Rdy::answer`); only an approval is signed.
    Answer(AnswerRequest),
    /// Each of `refs` with its recommended option (`Rdy::answer_all_recommended`), in order;
    /// each approval its own signed batch.
    Recommended {
        refs: Vec<String>,
        card_hashes: Vec<String>,
        relay: RelayInput,
    },
    /// A version (`Rdy::tag_as`).
    Tag {
        name: String,
        landing: Option<String>,
        message: Option<String>,
        relay: RelayInput,
        implicit: bool,
    },
}

/// What [`run_sign`] did: the subject, the requested signature's check, and whether that
/// signature was already stored (then nothing was written and no agent was asked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignResult {
    pub subject: Hash,
    pub check: SignatureCheck,
    pub already_signed: bool,
}

/// Runs `request` for `session` (the asking session) and `person` (the project's human actor,
/// from the caller), signed as the signing settings say: the act's reply as JSON, with
/// `signatures` (the ordered checks of the new signatures, relay before person; empty when
/// unsigned) on an approval or tag, or on each approval of an answer, and any signing warning
/// after the act's own warnings. Consequences are attached as the act's existing reply attaches
/// them.
pub fn run_signed_act(
    engine: &dyn Engine,
    agent: &dyn crate::signing_crypto::SigningAgent,
    session: &Session,
    person: &str,
    request: SigningAct,
) -> Result<Value, RdyError> {
    match request {
        SigningAct::Approve { proposal, options } => {
            let approver = if options.relay.is_some() {
                Session::human(person.to_string(), Role::Approver)
            } else {
                session.clone()
            };
            let act = Single::Approve {
                approver,
                proposal,
                options,
            };
            let mut v = run_single(engine, agent, session, person, act)?;
            crate::consequence::report_approval(engine, &mut v);
            Ok(v)
        }
        SigningAct::Answer(req) => {
            let mut v = run_single(engine, agent, session, person, Single::Answer(req))?;
            crate::consequence::report_answer_landings(engine, &mut v);
            Ok(v)
        }
        SigningAct::Tag {
            name,
            landing,
            message,
            relay,
            implicit,
        } => run_single(
            engine,
            agent,
            session,
            person,
            Single::Tag {
                name,
                landing,
                message,
                relay,
                implicit,
            },
        ),
        SigningAct::Recommended {
            refs,
            card_hashes,
            relay,
        } => run_recommended(engine, agent, session, person, refs, card_hashes, relay),
    }
}

/// Each of `refs` with its recommended option: the pre-scan once under the writer (nothing is
/// answered when it refuses), then each item through the signed-act protocol, in order, stopping
/// at the first refusal with the answers given before it under `corrected.done`.
fn run_recommended(
    engine: &dyn Engine,
    agent: &dyn crate::signing_crypto::SigningAgent,
    session: &Session,
    person: &str,
    refs: Vec<String>,
    card_hashes: Vec<String>,
    relay: RelayInput,
) -> Result<Value, RdyError> {
    let (s, p) = (session.clone(), person.to_string());
    let (r2, h2) = (refs.clone(), card_hashes.clone());
    let by = with_writer_result(engine, move |r| {
        r.recommended_answerer(&s, &p, &r2, &h2, &relay)
    })?;
    let mut done: Vec<Value> = vec![];
    for (n, r) in refs.iter().enumerate() {
        let act = Single::Recommended {
            by: by.clone(),
            r#ref: r.clone(),
            shown: card_hashes.get(n).cloned(),
        };
        match run_single(engine, agent, session, person, act) {
            Ok(v) => done.push(v),
            Err(mut e) => {
                let list = Value::Array(done);
                match &mut e.corrected {
                    Some(Value::Object(m)) => {
                        m.insert("done".into(), list);
                    }
                    _ => e.corrected = Some(json!({ "done": list })),
                }
                return Err(e);
            }
        }
    }
    if let Some(last) = done.last_mut() {
        last["next"] = json!(crate::answer::next_of(&engine.read(), person, &by));
    }
    let mut v = Value::Array(done);
    crate::consequence::report_answer_landings(engine, &mut v);
    Ok(v)
}

/// One act of the protocol.
#[derive(Clone)]
enum Single {
    Approve {
        approver: Session,
        proposal: String,
        options: LandOptions,
    },
    Answer(AnswerRequest),
    Recommended {
        by: Answerer,
        r#ref: String,
        shown: Option<String>,
    },
    Tag {
        name: String,
        landing: Option<String>,
        message: Option<String>,
        relay: RelayInput,
        implicit: bool,
    },
}

/// A prepared act, not yet written.
enum PreparedAct {
    Landing(Box<PreparedApproval>),
    Answer(Box<PreparedAnswer>),
    Tag(Box<PreparedTagAct>),
}

impl PreparedAct {
    fn landing(&self) -> Option<&PreparedApproval> {
        match self {
            PreparedAct::Landing(l) => Some(l),
            PreparedAct::Answer(a) => Some(&a.landing),
            PreparedAct::Tag(_) => None,
        }
    }

    /// The prospective subject, as a check reads it.
    fn subject(&self) -> SignatureSubject {
        match self {
            PreparedAct::Tag(t) => SignatureSubject::Tag {
                hash: t.store.hash(),
                record: t.store.tag().clone(),
                answer: t.store.answer().map(|(h, a)| (h, a.clone())),
            },
            PreparedAct::Landing(l) => landing_subject(l),
            PreparedAct::Answer(a) => landing_subject(&a.landing),
        }
    }

    /// The act's row of the mode table.
    fn class(&self, human: bool) -> Class {
        let relayed = match self {
            PreparedAct::Tag(t) => t.relayed_by.is_some(),
            PreparedAct::Landing(l) => is_relayed(l),
            PreparedAct::Answer(a) => is_relayed(&a.landing),
        };
        let policy = self
            .landing()
            .is_some_and(|l| l.store.landing().assurance == Assurance::Policy);
        match (policy, relayed, human) {
            (true, _, _) => Class::None,
            (false, true, _) => Class::Relayed,
            (false, false, true) => Class::Local,
            (false, false, false) => Class::None,
        }
    }
}

/// The prospective landing of `l`, as a check reads it.
fn landing_subject(l: &PreparedApproval) -> SignatureSubject {
    SignatureSubject::Landing {
        hash: l.store.hash(),
        record: l.store.landing().clone(),
    }
}

/// Whether the prepared landing records a relayed approval.
fn is_relayed(l: &PreparedApproval) -> bool {
    l.store.landing().assurance == Assurance::Relayed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// A human's own act.
    Local,
    /// A person's act relayed by an agent.
    Relayed,
    /// Never signed: a policy approval, or an agent's own unrelayed act.
    None,
}

/// One signature the mode table asks for.
#[derive(Clone, Debug)]
struct Want {
    kind: StatementKind,
    setting: &'static str,
    selector: String,
}

/// The mode table's outcome before any agent is asked.
enum Plan {
    /// No signature, no warning.
    Unsigned,
    /// No signature, with the first signing error as the warning.
    Warn(RdyError),
    /// Refused: nothing is written.
    Refuse(RdyError),
    /// These signatures, in order.
    Sign(Vec<Want>),
}

/// The closed mode table (module docs). `own` is the person's statement kind of the act.
fn plan(config: &SigningConfig, class: Class, own: StatementKind) -> Plan {
    if config.sign == SignMode::Never || class == Class::None {
        return Plan::Unsigned;
    }
    let always = config.sign == SignMode::Always;
    let person = config.key.clone().map(|selector| Want {
        kind: own,
        setting: PERSON_KEY,
        selector,
    });
    match class {
        Class::None => Plan::Unsigned,
        Class::Local => match person {
            Some(p) => Plan::Sign(vec![p]),
            None if always => Plan::Refuse(signing_key_missing(PERSON_KEY)),
            None => Plan::Unsigned,
        },
        Class::Relayed => match (config.agent_key.clone(), person) {
            (Some(selector), person) => {
                let relay = Want {
                    kind: StatementKind::Relay,
                    setting: AGENT_KEY,
                    selector,
                };
                Plan::Sign(std::iter::once(relay).chain(person).collect())
            }
            (None, _) if always => Plan::Refuse(signing_key_missing(AGENT_KEY)),
            (None, None) => Plan::Unsigned,
            (None, Some(_)) => Plan::Warn(signing_key_missing(AGENT_KEY)),
        },
    }
}

/// What a prepared attempt captured: the project positions its preparation is current for.
struct Captured {
    log_end: Location,
    pack: Arc<Pack>,
    config: SigningConfig,
    epoch: u64,
}

/// The signing settings of the project at `root`. A settings error is an ordinary error.
fn signing_config(root: &std::path::Path) -> Result<SigningConfig, RdyError> {
    let eff = effective(root, user_file_from_env().as_deref())?;
    SigningConfig::from_effective(root, &eff)
}

/// An act prepared under the writer, with what signing it needs.
struct Attempt {
    act: PreparedAct,
    wants: Vec<Want>,
    captured: Captured,
    repo: RepoId,
    signed_by: Provenance,
    mode: SignMode,
}

/// The first writer step of an attempt.
enum Step {
    /// The act needed no agent and was written.
    Done(Box<Done>),
    /// The act is prepared and waits for its signatures.
    Prepared(Box<Attempt>),
}

/// The second writer step of an attempt.
enum Commit {
    Done(Box<Done>),
    /// The project moved since the preparation: start again.
    Moved,
}

/// A written act, before its reply is built.
enum Done {
    Landing {
        info: Box<LandInfo>,
        checks: Vec<SignatureCheck>,
        warning: Option<RdyError>,
    },
    Answer {
        info: Box<AnswerInfo>,
        checks: Vec<SignatureCheck>,
        warning: Option<RdyError>,
        approval: bool,
    },
    Tag {
        info: Box<TagInfo>,
        checks: Vec<SignatureCheck>,
        warning: Option<RdyError>,
    },
}

/// One act through the protocol (module docs), at most [`ATTEMPTS`] times.
fn run_single(
    engine: &dyn Engine,
    agent: &dyn crate::signing_crypto::SigningAgent,
    session: &Session,
    person: &str,
    act: Single,
) -> Result<Value, RdyError> {
    // the proposal version the first preparation resolved: a retry approves that version or
    // is refused as moved, never a newer one a versionless name now resolves to
    let mut pin: Option<ProposalRef> = None;
    for _ in 0..ATTEMPTS {
        let (s, p, a) = (session.clone(), person.to_string(), act.clone());
        let step = with_writer_result(engine, move |r| prepare(r, &s, &p, a, pin))?;
        let attempt = match step {
            Step::Done(done) => return Ok(reply(*done)),
            Step::Prepared(attempt) => attempt,
        };
        if pin.is_none() {
            pin = attempt.act.landing().and_then(PreparedApproval::pin);
        }
        let signed = match sign_act(agent, &attempt) {
            Ok(signed) => Signed {
                signatures: signed,
                warning: None,
            },
            Err(e) if attempt.mode == SignMode::Auto => Signed {
                signatures: vec![],
                warning: Some(signing_failed(&e)),
            },
            Err(e) => return Err(e),
        };
        let p = person.to_string();
        match with_writer_result(engine, move |r| commit(r, *attempt, signed, &p))? {
            Commit::Done(done) => return Ok(reply(*done)),
            Commit::Moved => continue,
        }
    }
    Err(RdyError::new(
        E_STALE_BASE,
        format!("the project changed during all {ATTEMPTS} signing attempts"),
        "read the current proposal or tag target, then try again",
    ))
}

/// The signatures of an attempt and their checks, or none with the signing warning.
struct Signed {
    signatures: Vec<(Signature, SignatureCheck)>,
    warning: Option<RdyError>,
}

/// Step 1, under the writer: validates and prepares the act, reads the signing settings, and
/// commits at once when no agent is needed. With `pin` (a retry), a landing must approve that
/// version: a newer one is the existing refusal of a moved proposal.
fn prepare(
    r: &mut Rdy,
    s: &Session,
    person: &str,
    act: Single,
    pin: Option<ProposalRef>,
) -> Result<Step, RdyError> {
    let (prepared, own) = match act {
        Single::Approve {
            approver,
            proposal,
            options,
        } => (
            PreparedAct::Landing(Box::new(r.prepare_landing(&approver, &proposal, options)?)),
            StatementKind::Approval,
        ),
        Single::Answer(req) => match r.answer_step(s, person, req)? {
            AnswerStep::Done(info) => {
                return Ok(Step::Done(Box::new(Done::Answer {
                    info,
                    checks: vec![],
                    warning: None,
                    approval: false,
                })));
            }
            AnswerStep::Approval(a) => (PreparedAct::Answer(a), StatementKind::Approval),
        },
        Single::Recommended { by, r#ref, shown } => {
            match r.recommended_step(&by, person, &r#ref, shown.as_deref())? {
                AnswerStep::Done(info) => {
                    return Ok(Step::Done(Box::new(Done::Answer {
                        info,
                        checks: vec![],
                        warning: None,
                        approval: false,
                    })));
                }
                AnswerStep::Approval(a) => (PreparedAct::Answer(a), StatementKind::Approval),
            }
        }
        Single::Tag {
            name,
            landing,
            message,
            relay,
            implicit,
        } => (
            PreparedAct::Tag(Box::new(r.prepare_tag_as(
                s,
                person,
                &relay,
                implicit,
                &name,
                landing.as_deref(),
                message,
            )?)),
            StatementKind::Tag,
        ),
    };
    if let (Some(pin), Some(l)) = (pin, prepared.landing())
        && l.pref().version != pin.version
    {
        return Err(l.moved_from(&r.read(), &pin)?);
    }
    // the settings are read only after the act's own validation passed
    let config = signing_config(r.root())?;
    let mode = config.sign;
    let wants = match plan(&config, prepared.class(s.is_human()), own) {
        Plan::Refuse(e) => return Err(e),
        Plan::Unsigned => return write(r, prepared, vec![], None, person).map(Step::Done),
        Plan::Warn(e) => {
            return write(r, prepared, vec![], Some(signing_failed(&e)), person).map(Step::Done);
        }
        Plan::Sign(wants) => wants,
    };
    let captured = Captured {
        log_end: r.store().log_end(),
        pack: Arc::clone(&r.pack),
        config,
        epoch: r.store().redaction_epoch(),
    };
    Ok(Step::Prepared(Box::new(Attempt {
        act: prepared,
        wants,
        captured,
        repo: r.read().repo_id()?,
        signed_by: s.provenance(),
        mode,
    })))
}

/// Step 3 and 4, under the writer: the removal check, the freshness check, then the commit.
fn commit(r: &mut Rdy, attempt: Attempt, signed: Signed, person: &str) -> Result<Commit, RdyError> {
    if let Some(l) = attempt.act.landing() {
        r.recheck_removals(l)?;
    }
    let c = &attempt.captured;
    let current = r.store().log_end() == c.log_end
        && Arc::ptr_eq(&r.pack, &c.pack)
        && r.store().redaction_epoch() == c.epoch
        && signing_config(r.root())? == c.config;
    if !current {
        return Ok(Commit::Moved);
    }
    write(r, attempt.act, signed.signatures, signed.warning, person).map(Commit::Done)
}

/// Writes the prepared act with its signatures through the act's existing commit.
fn write(
    r: &mut Rdy,
    act: PreparedAct,
    signatures: Vec<(Signature, SignatureCheck)>,
    warning: Option<RdyError>,
    person: &str,
) -> Result<Box<Done>, RdyError> {
    let (records, checks): (Vec<Signature>, Vec<SignatureCheck>) = signatures.into_iter().unzip();
    Ok(Box::new(match act {
        PreparedAct::Landing(l) => Done::Landing {
            info: Box::new(r.commit_landing(*l, &records)?),
            checks,
            warning,
        },
        PreparedAct::Answer(a) => Done::Answer {
            info: Box::new(r.commit_answer(*a, &records, person)?),
            checks,
            warning,
            approval: true,
        },
        PreparedAct::Tag(t) => Done::Tag {
            info: Box::new(r.commit_tag_as(*t, &records)?),
            checks,
            warning,
        },
    }))
}

/// Step 2, with the writer free: the planned signatures of the prepared record, in order, each
/// checked once. The first failure ends it.
fn sign_act(
    agent: &dyn crate::signing_crypto::SigningAgent,
    attempt: &Attempt,
) -> Result<Vec<(Signature, SignatureCheck)>, RdyError> {
    let context = CheckContext {
        subject: attempt.act.subject(),
        current_repo: attempt.repo,
        source_repos: vec![],
        // a new signature is checked as made here: never as another project's
        holder: false,
    };
    attempt
        .wants
        .iter()
        .map(|w| sign_one(agent, w, &context, &attempt.signed_by, None))
        .collect()
}

/// One new signature of `want` over the subject of `context`, checked once against it (with
/// `trust`, or none). A reply that does not verify as the requested key's signature of the
/// requested statement is `E_NO_AGENT` `invalid reply`.
fn sign_one(
    agent: &dyn crate::signing_crypto::SigningAgent,
    want: &Want,
    context: &CheckContext,
    signed_by: &Provenance,
    trust: Option<&AllowedSigners>,
) -> Result<(Signature, SignatureCheck), RdyError> {
    let key = resolve_key(want.setting, &want.selector, agent)?;
    let source = source_of(&context.subject, want.kind)?;
    let text =
        statement(source, Some(context.current_repo), &key.fingerprint).map_err(statement_error)?;
    let namespace = want.kind.namespace();
    let sig = sign_sshsig(agent, &key, namespace, &text)?;
    let record = Signature {
        subject: context.subject.hash(),
        namespace: namespace.to_string(),
        statement: text,
        sig,
        signed_by: signed_by.clone(),
    };
    let check = check_signature(&record, context, trust);
    let ok = check.signed == SIGNED_VALID
        && check.statement == STATEMENT_MATCH
        && check.key.as_deref() == Some(key.fingerprint.as_str())
        && check.kind.as_deref() == Some(want.kind.as_str());
    if !ok {
        return Err(no_agent(NoAgentCause::InvalidReply));
    }
    Ok((record, check))
}

/// The statement source of `kind` over `subject`; a subject that cannot form it is the shared
/// signing `E_BAD_INPUT`.
fn source_of(
    subject: &SignatureSubject,
    kind: StatementKind,
) -> Result<StatementSource<'_>, RdyError> {
    match (subject, kind) {
        (SignatureSubject::Landing { record, .. }, StatementKind::Approval) => {
            Ok(StatementSource::Approval(record))
        }
        (SignatureSubject::Landing { record, .. }, StatementKind::Relay) => {
            Ok(StatementSource::RelayApproval(record))
        }
        (SignatureSubject::Tag { hash, record, .. }, StatementKind::Tag) => {
            Ok(StatementSource::Tag {
                hash: *hash,
                tag: record,
            })
        }
        (
            SignatureSubject::Tag {
                hash,
                record,
                answer: Some((answer_hash, answer)),
            },
            StatementKind::Relay,
        ) => Ok(StatementSource::RelayTag {
            hash: *hash,
            tag: record,
            answer_hash: *answer_hash,
            answer,
        }),
        _ => Err(statement_error(StatementError::Source)),
    }
}

/// A statement that cannot be built: a bound is the shared signing `E_LIMIT`; anything else the
/// shared signing `E_BAD_INPUT`.
fn statement_error(e: StatementError) -> RdyError {
    match e {
        StatementError::Limit(n) => signing_limit(&format!("{n} bytes")),
        StatementError::Syntax | StatementError::Source => RdyError::new(
            E_BAD_INPUT,
            "the record cannot form a signing statement",
            "check the record's actor, run and relay evidence",
        ),
    }
}

/// The reply of a written act (module docs, step 5), before consequences.
fn reply(done: Done) -> Value {
    let to_json = |v: Result<Value, serde_json::Error>| {
        v.unwrap_or_else(|e| {
            serde_json::to_value(RdyError::new(
                E_INTERNAL,
                format!("a write result did not serialize: {e}"),
                "report this as a bug in rdy",
            ))
            .unwrap_or(Value::Null)
        })
    };
    match done {
        Done::Landing {
            info,
            checks,
            warning,
        } => {
            let mut v = to_json(serde_json::to_value(&info));
            add_signing(&mut v, None, &checks, warning);
            v
        }
        Done::Answer {
            info,
            checks,
            warning,
            approval,
        } => {
            let mut v = to_json(serde_json::to_value(&info));
            if approval {
                add_signing(&mut v, Some("landing"), &checks, warning);
            }
            v
        }
        Done::Tag {
            info,
            checks,
            warning,
        } => {
            // a tag with no relay keeps the reply `tag` gives: the version alone
            let mut v = if info.relayed_by.is_none() {
                to_json(serde_json::to_value(&info.version))
            } else {
                to_json(serde_json::to_value(&info))
            };
            add_signing(&mut v, None, &checks, warning);
            v
        }
    }
}

/// Adds `signatures` to `v` and appends the signing warning after the act's own warnings (in
/// `v[at].warnings` when `at` names the landing of an answer).
fn add_signing(
    v: &mut Value,
    at: Option<&str>,
    checks: &[SignatureCheck],
    warning: Option<RdyError>,
) {
    v["signatures"] = serde_json::to_value(checks).unwrap_or(Value::Array(vec![]));
    let Some(w) = warning else {
        return;
    };
    let issue = Issue {
        severity: Severity::Warning,
        code: W_SIGNING_FAILED,
        block: None,
        message: w.message,
        fix: w.fix,
    };
    let issue = serde_json::to_value(&issue).unwrap_or(Value::Null);
    let target = match at {
        Some(key) => &mut v[key],
        None => v,
    };
    match target.get_mut("warnings").and_then(Value::as_array_mut) {
        Some(list) => list.push(issue),
        None => target["warnings"] = json!([issue]),
    }
}

// ---- later signing ----

/// Signs the stored landing or tag `target` (`tag:<version>`, or a full landing hash or a unique
/// prefix of at least six hex characters, historical landings included) as `session`, with the
/// statement kind `kind` (default: the person's own kind for a human, `relay` for an agent).
///
/// A human signs only their own act, with `signing.key`. An agent signs only a relayed act: its
/// relay with `signing.agent_key`, or the person's statement with `signing.key`, the machine
/// key's relay then being created in the same batch when no valid one is stored. A policy
/// approval is never signed. A valid stored signature of the kind by the selected key is
/// returned as it is (`already_signed`), with no agent asked and nothing written; one stored
/// meanwhile by a concurrent request wins over the prepared ones. The trust file is parsed
/// before anything is written, and used in the one check of each new signature.
pub fn run_sign(
    engine: &dyn Engine,
    agent: &dyn crate::signing_crypto::SigningAgent,
    session: &Session,
    target: &str,
    kind: Option<StatementKind>,
) -> Result<SignResult, RdyError> {
    let st = engine.read();
    let config = signing_config(&engine.root())?;
    let (current, sources) = repo_ids(&st)?;
    let hash = resolve_target(&st, target)?;
    let context = subject_context(&st, &hash, current, sources.clone())?;
    let (own, owner, relayed) = match &context.subject {
        SignatureSubject::Landing { record, .. } => {
            if record.assurance == Assurance::Policy {
                return Err(RdyError::new(
                    E_PERMISSION,
                    "policy approvals cannot be signed",
                    "wait for the authority policy to define their signer",
                ));
            }
            (
                StatementKind::Approval,
                record.approved_by.actor.clone(),
                record.assurance == Assurance::Relayed,
            )
        }
        SignatureSubject::Tag {
            hash,
            record,
            answer,
        } => (
            StatementKind::Tag,
            record.tagged_by.actor.clone(),
            answer.as_ref().is_some_and(|(_, a)| relays_tag(a, hash)),
        ),
        SignatureSubject::Missing { .. } => {
            return Err(no_stored_landing(target));
        }
    };
    if let Some(k) = kind
        && k != own
        && k != StatementKind::Relay
    {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("kind {} does not fit {target}", k.as_str()),
            "use approval or relay for a landing, and tag or relay for a tag",
        ));
    }
    let human = session.is_human();
    let kind = kind.unwrap_or(if human { own } else { StatementKind::Relay });
    let allowed = if human {
        kind == own && session.actor == owner
    } else {
        relayed
    };
    if !allowed {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("this session cannot sign {} for {target}", kind.as_str()),
            "sign as the recorded person, or relay the person's act from an agent session",
        ));
    }
    // the selected key, by the pure parser, before any agent is opened
    let setting = if kind == StatementKind::Relay {
        AGENT_KEY
    } else {
        PERSON_KEY
    };
    let (selector, fingerprint) = selected(&config, setting)?;
    let trust = read_allowed_signers(config.allowed_signers.as_deref())?;
    let mut seen: BTreeMap<Hash, SignatureCheck> = BTreeMap::new();
    if let Some(existing) =
        observe(&st, &context, trust.as_ref(), &mut seen)?.find_valid(kind, &fingerprint)
    {
        return Ok(SignResult {
            subject: hash,
            check: existing,
            already_signed: true,
        });
    }
    // an agent's person statement brings the machine key's relay beside it
    let mut wants = vec![];
    let mut companion = None;
    if !human && kind != StatementKind::Relay {
        let (agent_selector, agent_fp) = selected(&config, AGENT_KEY)?;
        if Observed(&seen)
            .find_valid(StatementKind::Relay, &agent_fp)
            .is_none()
        {
            wants.push(Want {
                kind: StatementKind::Relay,
                setting: AGENT_KEY,
                selector: agent_selector,
            });
            companion = Some(agent_fp);
        }
    }
    wants.push(Want {
        kind,
        setting,
        selector,
    });
    let signing_context = CheckContext {
        subject: context.subject.clone(),
        current_repo: current,
        source_repos: vec![],
        holder: false,
    };
    let signed_by = session.provenance();
    let mut signed = vec![];
    for w in &wants {
        signed.push(sign_one(
            agent,
            w,
            &signing_context,
            &signed_by,
            trust.as_ref(),
        )?);
    }
    let requested = signed.last().expect("the requested signature").1.clone();
    with_writer_result(engine, move |r| {
        let st = r.read();
        let context = subject_context(&st, &hash, current, sources)?;
        let observed = observe(&st, &context, trust.as_ref(), &mut seen)?;
        if let Some(existing) = observed.find_valid(kind, &fingerprint) {
            return Ok(SignResult {
                subject: hash,
                check: existing,
                already_signed: true,
            });
        }
        let companion_stored = companion
            .as_deref()
            .is_some_and(|fp| observed.find_valid(StatementKind::Relay, fp).is_some());
        let records: Vec<Signature> = signed
            .into_iter()
            .filter(|(g, _)| !(companion_stored && g.namespace == StatementKind::Relay.namespace()))
            .map(|(g, _)| g)
            .collect();
        drop(st);
        r.store_mut()
            .write(rdy_store::signature::frames(&records)?)?;
        r.seal_open()?;
        Ok(SignResult {
            subject: hash,
            check: requested,
            already_signed: false,
        })
    })
}

/// Whether the answer `a` is the relay of the tag `tag`: it wrote exactly that tag, relayed.
fn relays_tag(a: &Answer, tag: &Hash) -> bool {
    a.targets.as_slice() == [*tag] && a.relayed_by.is_some()
}

/// The configured selector of `setting` and its fingerprint, read by the pure parser. Unset
/// (null) is `E_SIGNING_KEY_MISSING`; a set one that is malformed, an empty or blank one included,
/// is the shared signing `E_BAD_INPUT`.
fn selected(config: &SigningConfig, setting: &str) -> Result<(String, String), RdyError> {
    let selector = match setting {
        PERSON_KEY => config.key.clone(),
        _ => config.agent_key.clone(),
    }
    .ok_or_else(|| signing_key_missing(setting))?;
    let fingerprint = key_fingerprint(&selector).map_err(|_| signing_selector(setting))?;
    Ok((selector, fingerprint))
}

/// The checks of every signature of the context's subject that `st` holds (never one of a batch
/// that is still open), each checked once in this request: `seen` keeps the reports of hashes
/// already checked.
fn observe<'s>(
    st: &crate::ReadState,
    context: &CheckContext,
    trust: Option<&AllowedSigners>,
    seen: &'s mut BTreeMap<Hash, SignatureCheck>,
) -> Result<Observed<'s>, RdyError> {
    let reader = st.reader();
    let ids = state_signatures(st, &context.subject.hash())?;
    for id in ids {
        if let std::collections::btree_map::Entry::Vacant(slot) = seen.entry(id) {
            let record = reader.signature(&id)?;
            slot.insert(check_signature(&record, context, trust));
        }
    }
    Ok(Observed(seen))
}

/// The checks a request has observed, by signature hash.
struct Observed<'s>(&'s BTreeMap<Hash, SignatureCheck>);

impl Observed<'_> {
    /// The first valid, covering signature of `kind` by the key `fingerprint`.
    fn find_valid(&self, kind: StatementKind, fingerprint: &str) -> Option<SignatureCheck> {
        self.0
            .values()
            .find(|c| {
                c.signed == SIGNED_VALID
                    && c.statement == STATEMENT_MATCH
                    && c.kind.as_deref() == Some(kind.as_str())
                    && c.key.as_deref() == Some(fingerprint)
            })
            .cloned()
    }
}

/// `E_NOT_FOUND` for a target that names no stored landing.
fn no_stored_landing(target: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("no stored landing matches {target:?}"),
        "pass a full landing hash or a longer unique prefix",
    )
}

/// The subject `target` names: `tag:<version>` a tag (by its ref), else a stored landing by full
/// hash (the typed lookup) or unique prefix (a scan of the stored LANDING frames that keeps at
/// most two distinct matches).
fn resolve_target(st: &crate::ReadState, target: &str) -> Result<Hash, RdyError> {
    if let Some(name) = target.strip_prefix("tag:") {
        let want = parse_version(name)?;
        let tag: Option<Tag> = st
            .version_tags()?
            .into_iter()
            .find(|(v, _)| v.cmp_precedence(&want) == std::cmp::Ordering::Equal)
            .map(|(_, t)| t);
        let Some(tag) = tag else {
            return Err(RdyError::new(
                E_NOT_FOUND,
                format!("no version {name}"),
                "list versions with rdy tags",
            ));
        };
        let value = st
            .reader()
            .get_ref(&format!("tag/{}", tag.name))?
            .ok_or_else(|| {
                RdyError::new(
                    E_NOT_FOUND,
                    format!("no version {name}"),
                    "list versions with rdy tags",
                )
            })?;
        let bytes = <[u8; 32]>::try_from(value.as_slice()).map_err(|_| {
            RdyError::from(StoreError::Decode(format!(
                "tag ref of {} is not a hash",
                tag.name
            )))
        })?;
        return Ok(Hash(bytes));
    }
    let want = landing_spelling(target)?;
    let reader = st.reader();
    if want.len() == 64 {
        let h: Hash = want.parse().map_err(|_| no_stored_landing(target))?;
        return if reader.has_kind(kinds::LANDING, &h)? {
            Ok(h)
        } else {
            Err(no_stored_landing(target))
        };
    }
    let mut hits: Vec<Hash> = Vec::with_capacity(2);
    reader.for_each_object_of(kinds::LANDING, |h| {
        if hits.len() < 2 && !hits.contains(&h) && h.to_string().starts_with(&want) {
            hits.push(h);
        }
        Ok(())
    })?;
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err(no_stored_landing(target)),
        _ => Err(RdyError::new(
            E_NOT_FOUND,
            format!("{target:?} matches more than one stored landing"),
            "give more characters of the landing id",
        )),
    }
}
