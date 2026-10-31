//! `rdy_review` (D-JQD5X, O8, O9, O16): list the checks, run one on your own work, request an
//! independent review, or list requests and their verdicts. A run or a request writes durable
//! CHECK_REQUESTs, one per contradiction pair; the queue and the verdict paths are
//! `rdy_review_next` and `rdy_review_submit` (`crate::semantic::api`).
use super::case::{BuiltCase, build_case, contradiction_cases, wrong_subject};
use super::catalogue::{AGENT_CHECKS, AgentCheck};
use super::code::resolve_code_range;
use super::requests::{
    Answers, Holder, authored_block, durable_label, for_each_request, holder, request_row,
    status_of,
};
use crate::answer::relay::{RelayInput, relayed};
use crate::engine::write_typed;
use crate::read_state::Delta;
use crate::{
    E_BAD_INPUT, E_PERMISSION, Engine, INFLIGHT_SCOPE, RdyError, ReadState, Scope, Session,
};
use rdy_model::check_request::{CheckCode, CheckMode, CheckRequest, CheckSubject};
use rdy_model::{BlockId, Hash};
use rdy_store::StoreError;
use rdy_store::kinds;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

/// The most rows one list returns.
pub const MAX_LIST_ROWS: usize = 256;

/// The most trimmed characters, and bytes, of a request's reason.
const MAX_REASON_CHARS: usize = 2000;
const MAX_REASON_BYTES: usize = 8000;

/// What a refusal of a malformed request names as the shape.
const SHAPE: &str = "{op: \"checks\"} | {op: \"run\", check, subject} | {op: \"request\", check, subject, reason?, evidence?, question?, session_ref?} | {op: \"list\", subject?, open?}";

/// What a refusal of a malformed subject says.
const SUBJECT_SHAPE: &str =
    "subject must be one block, change or P-x@n, two block ids, or {decision, code}";

/// A subject as a caller writes it (O8): one block, change hash or `P-x@n`; two block ids; or a
/// decision and a code range `<repo>@<from>..<to>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubjectInput {
    One(String),
    Pair(String, String),
    Code { decision: String, code: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeSubject {
    decision: String,
    code: String,
}

impl<'de> Deserialize<'de> for SubjectInput {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(s) => Ok(SubjectInput::One(s)),
            Value::Array(items) => match items.as_slice() {
                [Value::String(a), Value::String(b)] => {
                    Ok(SubjectInput::Pair(a.clone(), b.clone()))
                }
                _ => Err(D::Error::custom(SUBJECT_SHAPE)),
            },
            v @ Value::Object(_) => {
                let c = CodeSubject::deserialize(v).map_err(D::Error::custom)?;
                Ok(SubjectInput::Code {
                    decision: c.decision,
                    code: c.code,
                })
            }
            _ => Err(D::Error::custom(SUBJECT_SHAPE)),
        }
    }
}

/// One `rdy_review` request, tagged by `op`. Each op refuses the keys of the others.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewRequest {
    /// The catalogue, in its fixed order.
    Checks {},
    /// An author-run request on the session's own work; the case returns at once.
    Run {
        check: String,
        subject: SubjectInput,
    },
    /// An independent request: on the session's own work, or relayed for a person.
    Request {
        check: String,
        subject: SubjectInput,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        evidence: Option<String>,
        #[serde(default)]
        question: Option<String>,
        #[serde(default)]
        session_ref: Option<String>,
    },
    /// Requests and their verdicts.
    List {
        #[serde(default)]
        subject: Option<SubjectInput>,
        #[serde(default)]
        open: Option<bool>,
    },
}

impl ReviewRequest {
    /// `v` as a request; an unknown key is refused with the nearest known one.
    pub fn from_value(v: Value) -> Result<ReviewRequest, RdyError> {
        ReviewRequest::deserialize(&v).map_err(|e| {
            let msg = e.to_string();
            crate::did_you_mean::unknown_field_error_in("rdy_review", &msg, &v).unwrap_or_else(
                || {
                    RdyError::new(
                        E_BAD_INPUT,
                        format!("rdy_review: {msg}"),
                        format!("args for rdy_review: {SHAPE}"),
                    )
                },
            )
        })
    }
}

/// The check named exactly `name`, else D-JQD5X's refusal.
pub fn parse_check(name: &str) -> Result<AgentCheck, RdyError> {
    AgentCheck::parse(name).ok_or_else(|| {
        RdyError::new(
            E_BAD_INPUT,
            format!("{name} is not a check"),
            "list the checks: rdy_review {op: \"checks\"} / rdy review checks",
        )
    })
}

/// Runs `req` for session `s`. `person` is the project's human actor (a relayed request is
/// theirs); `implicit_relay` is true over MCP, where the session is the relay; `relayed_by` is the
/// command line's relaying actor, read by `request` only.
pub fn run_review(
    engine: &dyn Engine,
    s: &Session,
    person: &str,
    implicit_relay: bool,
    relayed_by: Option<String>,
    req: ReviewRequest,
) -> Result<Value, RdyError> {
    match req {
        ReviewRequest::Checks {} => Ok(json!({
            "checks": AGENT_CHECKS
                .iter()
                .map(|c| json!({"name": c.name, "subject": c.subject, "verdicts": c.verdicts}))
                .collect::<Vec<_>>(),
        })),
        ReviewRequest::List { subject, open } => {
            list(&engine.read(), subject.as_ref(), open.unwrap_or(false))
        }
        ReviewRequest::Run { check, subject } => create(
            engine,
            s,
            Ask {
                check,
                subject,
                mode: CheckMode::Author,
                reason: None,
                relay: None,
            },
            person,
            implicit_relay,
        ),
        ReviewRequest::Request {
            check,
            subject,
            reason,
            evidence,
            question,
            session_ref,
        } => {
            let relay = RelayInput {
                relayed_by,
                evidence,
                question,
                session_ref,
                kind: None,
            };
            create(
                engine,
                s,
                Ask {
                    check,
                    subject,
                    mode: CheckMode::Independent,
                    reason,
                    relay: Some(relay),
                },
                person,
                implicit_relay,
            )
        }
    }
}

/// A run or a request, as given.
struct Ask {
    check: String,
    subject: SubjectInput,
    mode: CheckMode,
    reason: Option<String>,
    /// A request's relay keys; `None` for a run.
    relay: Option<RelayInput>,
}

/// The scope a session's subjects resolve in: its own view when it exists, else the in-flight
/// scope (canon and every open view); and that scope's label.
fn session_scope(st: &ReadState, s: &Session) -> (Scope, String) {
    let v = s.default_view();
    if st.has_view(&v) {
        (Scope::View(v.clone()), v)
    } else {
        (Scope::Inflight, INFLIGHT_SCOPE.to_string())
    }
}

/// A block of `scope` that `text` names: an id, key or path, through the existing resolvers.
fn resolve_block(st: &ReadState, scope: &Scope, text: &str) -> Result<BlockId, RdyError> {
    let id = st.resolve_id(scope, text)?;
    st.block(scope, &id)?;
    Ok(id)
}

/// `input` resolved in `scope` for `check`: the subject a request stores before its primary
/// blocks are known (a change or version names none yet), and the scope label it stores.
fn resolve_subject(
    st: &ReadState,
    scope: &Scope,
    label: &str,
    check: AgentCheck,
    input: &SubjectInput,
) -> Result<(CheckSubject, String), RdyError> {
    let empty = CheckSubject {
        blocks: vec![],
        change: None,
        version: None,
        code: None,
    };
    let reader = st.reader();
    match input {
        SubjectInput::One(text) if text.starts_with("P-") => {
            let p = crate::gate::resolve_in(st, text)?;
            let view = st.proposal_version(&p.version)?.view;
            Ok((
                CheckSubject {
                    version: Some(p.version),
                    ..empty
                },
                view,
            ))
        }
        SubjectInput::One(text) => {
            if let Ok(h) = text.to_ascii_lowercase().parse::<Hash>() {
                if reader.has_kind(kinds::CHANGE, &h)? {
                    let view = reader.change(&h)?.view;
                    return Ok((
                        CheckSubject {
                            change: Some(h),
                            ..empty
                        },
                        view,
                    ));
                }
                if reader.has_kind(kinds::PROPOSAL, &h)? {
                    let view = st.proposal_version(&h)?.view;
                    return Ok((
                        CheckSubject {
                            version: Some(h),
                            ..empty
                        },
                        view,
                    ));
                }
                return Err(RdyError::new(
                    crate::E_NOT_FOUND,
                    format!("no change {}", h.short()),
                    "check the id with rdy view show <view>",
                ));
            }
            let id = resolve_block(st, scope, text)?;
            Ok((
                CheckSubject {
                    blocks: vec![id],
                    ..empty
                },
                label.to_string(),
            ))
        }
        SubjectInput::Pair(a, b) => {
            let (a, b) = (resolve_block(st, scope, a)?, resolve_block(st, scope, b)?);
            if a == b {
                return Err(wrong_subject(
                    check,
                    &format!("{a}, {b}"),
                    "pair that names one block twice",
                ));
            }
            let mut blocks = vec![a, b];
            blocks.sort();
            Ok((CheckSubject { blocks, ..empty }, label.to_string()))
        }
        SubjectInput::Code { decision, code } => {
            if check != AgentCheck::DiffContradicts {
                return Err(wrong_subject(check, code, "code range"));
            }
            let d = resolve_block(st, scope, decision)?;
            let code = resolve_code_range(check, st.root(), code)?;
            Ok((
                CheckSubject {
                    blocks: vec![d],
                    code: Some(code),
                    ..empty
                },
                label.to_string(),
            ))
        }
    }
}

/// The cases of `check` on `named` in scope `label`, and the pairs left out: one per
/// contradiction pair (none when a decision has no pair), else one.
fn build_all(
    st: &ReadState,
    check: AgentCheck,
    label: &str,
    named: &CheckSubject,
) -> Result<(Vec<BuiltCase>, usize), RdyError> {
    let h: Holder =
        holder(st, label, named)?.ok_or_else(|| StoreError::UnknownView(label.to_string()))?;
    if check == AgentCheck::Contradiction {
        contradiction_cases(named, h.inputs(st))
    } else {
        Ok((vec![build_case(check, named, h.inputs(st))?], 0))
    }
}

/// Whether `actor` may run or request `check` on its own work: it wrote a primary block of the
/// subject (another run is the same actor), or it is the named change's or version's author.
fn own_work(
    st: &ReadState,
    label: &str,
    named: &CheckSubject,
    cases: &[BuiltCase],
    actor: &str,
) -> Result<bool, RdyError> {
    if let Some(c) = &named.change
        && st.reader().change(c)?.provenance.actor == actor
    {
        return Ok(true);
    }
    if let Some(v) = &named.version
        && st.proposal_version(v)?.provenance.actor == actor
    {
        return Ok(true);
    }
    let mut blocks = named.blocks.clone();
    for c in cases {
        blocks.extend(c.subject_blocks.iter().cloned());
    }
    blocks.sort();
    blocks.dedup();
    let Some(h) = holder(st, label, named)? else {
        return Ok(false);
    };
    Ok(authored_block(st, h.after(), &blocks, actor)?.is_some())
}

/// A request's reason, trimmed: 1 to 2000 characters and at most 8000 bytes.
fn checked_reason(reason: Option<String>) -> Result<Option<String>, RdyError> {
    let Some(r) = reason else {
        return Ok(None);
    };
    let t = r.trim();
    if r.len() > MAX_REASON_BYTES || t.is_empty() || t.chars().count() > MAX_REASON_CHARS {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "review reason must have 1 to 2000 characters",
            "give a short reason, or omit reason",
        ));
    }
    Ok(Some(t.to_string()))
}

/// The reply of a run or a request: the first request, its case hash and case, every request,
/// and the pairs left out. With no request, the first three are null.
fn reply(ids: &[Hash], cases: &[BuiltCase], omitted: usize) -> Value {
    let rows: Vec<Value> = ids
        .iter()
        .zip(cases)
        .map(|(id, c)| {
            json!({"request": durable_label(id), "case_hash": c.hash.to_string(), "case": c.case})
        })
        .collect();
    let first = rows.first();
    json!({
        "request": first.map_or(Value::Null, |r| r["request"].clone()),
        "case_hash": first.map_or(Value::Null, |r| r["case_hash"].clone()),
        "case": first.map_or(Value::Null, |r| r["case"].clone()),
        "requests": rows,
        "omitted": omitted,
    })
}

/// Writes the requests of `ask` for session `s` (O8, O9).
fn create(
    engine: &dyn Engine,
    s: &Session,
    ask: Ask,
    person: &str,
    implicit: bool,
) -> Result<Value, RdyError> {
    if !s.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot write", s.role),
            "start the session as a writer",
        ));
    }
    let reason = checked_reason(ask.reason)?;
    let check = parse_check(&ask.check)?;
    let st = engine.read();
    let (scope, session_label) = session_scope(&st, s);
    let (named, label) = resolve_subject(&st, &scope, &session_label, check, &ask.subject)?;
    let (cases, omitted) = build_all(&st, check, &label, &named)?;
    // who asks: the session on its own work, else a person through the relay table
    let relay_given = ask.relay.as_ref().is_some_and(|r| {
        r.relayed_by.is_some()
            || r.evidence.is_some()
            || r.question.is_some()
            || r.session_ref.is_some()
    });
    let (requested_by, relay) = match &ask.relay {
        Some(r) if s.is_human() || relay_given => {
            let (who, relay) = relayed(s, person, r, implicit).map_err(|e| e.error())?;
            (who.provenance(), relay)
        }
        _ => {
            if !own_work(&st, &label, &named, &cases, &s.actor)? {
                return Err(RdyError::new(
                    E_PERMISSION,
                    format!(
                        "actor {} wrote no block of this {} subject",
                        s.actor,
                        check.spec().name
                    ),
                    "run checks on your own work, or relay a person's request with their evidence: rdy_review / rdy review request --relayed-by",
                ));
            }
            (s.provenance(), None)
        }
    };
    if cases.is_empty() {
        return Ok(reply(&[], &[], omitted));
    }
    let seq = st.seq();
    drop(st);
    let mode = ask.mode;
    write_typed(engine, move |r| {
        // a snapshot the log moved past is built again on the writer's state
        let now = r.read();
        let (cases, omitted) = if now.seq() == seq {
            (cases, omitted)
        } else {
            build_all(&now, check, &label, &named)?
        };
        drop(now);
        if cases.is_empty() {
            return Ok(reply(&[], &[], omitted));
        }
        let rows: Vec<CheckRequest> = cases
            .iter()
            .map(|c| CheckRequest {
                check: check.spec().name.to_string(),
                subject: CheckSubject {
                    blocks: c.subject_blocks.clone(),
                    ..named.clone()
                },
                scope: label.clone(),
                case_hash: c.hash,
                mode,
                reason: reason.clone(),
                requested_by: requested_by.clone(),
                relayed_by: relay.as_ref().map(|r| r.by.clone()),
                relay: relay.as_ref().map(|r| r.evidence.clone()),
            })
            .collect();
        let ids = r.store_mut().put_check_requests(&rows)?;
        r.publish(Delta::Seq)?;
        Ok(reply(&ids, &cases, omitted))
    })
}

/// What a list's subject filter matches (review M1): membership of a primary block, an exact
/// pair, a change's or version's full hash, or a decision with its resolved code range.
enum Filter {
    Block(BlockId),
    Pair(Vec<BlockId>),
    Change(Hash),
    Version(Hash),
    Code(BlockId, CheckCode),
}

impl Filter {
    fn matches(&self, s: &CheckSubject) -> bool {
        match self {
            Filter::Block(b) => s.blocks.contains(b),
            Filter::Pair(p) => s.blocks == *p,
            Filter::Change(h) => s.change == Some(*h),
            Filter::Version(h) => s.version == Some(*h),
            Filter::Code(d, c) => {
                s.blocks.as_slice() == std::slice::from_ref(d) && s.code.as_ref() == Some(c)
            }
        }
    }
}

/// The requests whose subject `subject` matches (all when `None`), open ones only with `open`,
/// in full hash order: at most [`MAX_LIST_ROWS`], and the exact count left out. It writes
/// nothing and takes no lease.
fn list(st: &ReadState, subject: Option<&SubjectInput>, open: bool) -> Result<Value, RdyError> {
    let filter = match subject {
        None => None,
        Some(input) => {
            // a filter names blocks wherever a request may: canon and every open view
            let (scope, label) = (Scope::Inflight, INFLIGHT_SCOPE.to_string());
            let check = match input {
                SubjectInput::Code { .. } => AgentCheck::DiffContradicts,
                _ => AgentCheck::Contradiction,
            };
            let (named, _) = resolve_subject(st, &scope, &label, check, input)?;
            Some(match (input, named) {
                (
                    _,
                    CheckSubject {
                        change: Some(h), ..
                    },
                ) => Filter::Change(h),
                (
                    _,
                    CheckSubject {
                        version: Some(h), ..
                    },
                ) => Filter::Version(h),
                (
                    SubjectInput::Code { .. },
                    CheckSubject {
                        blocks,
                        code: Some(c),
                        ..
                    },
                ) => Filter::Code(blocks[0].clone(), c),
                (SubjectInput::Pair(..), CheckSubject { blocks, .. }) => Filter::Pair(blocks),
                (_, CheckSubject { blocks, .. }) => Filter::Block(blocks[0].clone()),
            })
        }
    };
    let answers = Answers::read(st)?;
    let mut rows = vec![];
    let mut omitted = 0usize;
    for_each_request(st, |h, r| {
        if filter.as_ref().is_some_and(|f| !f.matches(&r.subject)) {
            return Ok(true);
        }
        let full = rows.len() >= MAX_LIST_ROWS;
        if full && !open {
            omitted += 1;
            return Ok(true);
        }
        let status = status_of(st, &answers, &h, &r)?;
        if open && status != "open" {
            return Ok(true);
        }
        if full {
            omitted += 1;
        } else {
            rows.push(request_row(&h, &r, status, answers.of(&h, &r)));
        }
        Ok(true)
    })?;
    Ok(json!({"requests": rows, "omitted": omitted}))
}
