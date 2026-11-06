//! The `rdy_rule` contract (FW-MH.rules, D-ZSRBN): the closed input forms of the tool, checked
//! before any writer opens, and the shapes of its replies.
//!
//! A request is one JSON object with at most these keys: `scope, op, text, id, ruleset,
//! applies_to, why, evidence, question, session_ref, answer`. Its form follows from `op` (`add`,
//! `edit`, `remove`) or from `answer`; each form requires and refuses fixed keys. Every bound is
//! a number checked before a derived payload is built. This module owns no store access: it
//! reads no project, resolves no path and opens nothing. Resolving an id or a ruleset path, the
//! pack's areas and the policy belong to the steps after it.
//!
//! The refusals reuse the tool parameter wording (`unknown parameter`, `must be a string`), the
//! relay's own limits and words (`answer::relay`), the block id grammar, the item ref grammar,
//! the accept option grammar and the instruction selector grammar.
use crate::answer::is_item_ref;
use crate::answer::relay::{MAX_QUOTE, RelayRefusal};
use crate::did_you_mean::nearest;
use crate::error::{E_BAD_INPUT, E_LIMIT, E_VALIDATION, RdyError};
use rdy_model::{Block, BlockId, Hash};
use rdy_schema::instruction::{
    self, INSTRUCTION, MAX_SELECTOR_BYTES, MAX_SELECTORS, SELECTOR_FIX, SELECTOR_LIMIT_FIX,
    selector_ok,
};
use rdy_schema::{Issue, Pack, Severity};
use serde_json::{Map, Value, json};

/// The tool's name, as its refusals name it.
pub const TOOL: &str = "rdy_rule";

/// The most bytes of a rule's `text`.
pub const MAX_TEXT_BYTES: usize = 65_536;
/// The most Unicode characters of `why`.
pub const MAX_WHY_CHARS: usize = 2_000;
/// The most bytes of `why`: four per character.
pub const MAX_WHY_BYTES: usize = 8_000;
/// The most Unicode characters of `evidence` and of `question`: the relay's limit.
pub const MAX_EVIDENCE_CHARS: usize = MAX_QUOTE;
/// The most bytes of `evidence` and of `question`: four per character. A value this long has
/// more than [`MAX_EVIDENCE_CHARS`] characters, so the character check names it first.
pub const MAX_EVIDENCE_BYTES: usize = 8_000;
/// The most Unicode characters of `session_ref`: a view name's length.
pub const MAX_SESSION_REF_CHARS: usize = 256;
/// The most bytes of `session_ref`.
pub const MAX_SESSION_REF_BYTES: usize = 1_024;
/// The most bytes of `id`.
pub const MAX_ID_BYTES: usize = 96;
/// The most bytes of `ruleset`.
pub const MAX_RULESET_BYTES: usize = 1_024;
/// The most bytes of `answer.ref`.
pub const MAX_REF_BYTES: usize = 128;
/// The most bytes of `answer.option`: exactly `accept_verdict:<64 hex>:<64 hex>`.
pub const MAX_OPTION_BYTES: usize = 144;

/// The top-level keys, in the order they are checked and named.
pub const KEYS: [&str; 11] = [
    "scope",
    "op",
    "text",
    "id",
    "ruleset",
    "applies_to",
    "why",
    "evidence",
    "question",
    "session_ref",
    "answer",
];
/// The keys of `answer`, in the order they are checked and named.
pub const ANSWER_KEYS: [&str; 5] = ["ref", "option", "card_hash", "evidence", "question"];

const SCOPES: [&str; 2] = ["person", "project"];
const OPS: [&str; 3] = ["add", "edit", "remove"];

/// Where a rule lives: the person's meta project, or the project the session works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Person,
    Project,
}

impl Scope {
    /// The wire word.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Person => "person",
            Scope::Project => "project",
        }
    }

    /// The ruleset handle prefix of this destination.
    fn handle(self) -> &'static str {
        self.as_str()
    }
}

/// A ruleset the caller named: a doc id, or a logical path in the destination. Resolution is the
/// next step's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RulesetRef {
    Doc(BlockId),
    Path(String),
}

/// The person's words, as the relay records them. `evidence` and `question` keep the submitted
/// bytes; the relay validator trims them for its own checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayWords {
    pub evidence: String,
    pub question: Option<String>,
    /// Absent reads `mcp` or `cli`, as for any relay.
    pub session_ref: Option<String>,
}

/// An add: a new instruction with `text`, under `ruleset` or the destination's default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddRule {
    pub scope: Scope,
    pub text: String,
    pub why: String,
    pub ruleset: Option<RulesetRef>,
    /// Absent reads `["all"]`.
    pub applies_to: Option<Vec<String>>,
    pub relay: RelayWords,
}

/// An edit: new text for instruction `id`. Absent `ruleset` and `applies_to` keep the existing
/// ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRule {
    pub scope: Scope,
    pub id: BlockId,
    pub text: String,
    pub why: String,
    pub ruleset: Option<RulesetRef>,
    pub applies_to: Option<Vec<String>>,
    pub relay: RelayWords,
}

/// A remove: retire instruction `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveRule {
    pub scope: Scope,
    pub id: BlockId,
    pub why: String,
    pub relay: RelayWords,
}

/// An answer to a card this flow showed. `option` is an option id of that card, or the
/// `accept_verdict:<review>:<version>` grammar; `card_hash` is the hash the person saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerRule {
    pub scope: Scope,
    pub item_ref: String,
    pub option: String,
    pub card_hash: Hash,
    pub relay: RelayWords,
}

/// A checked request of `rdy_rule`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleRequest {
    Add(AddRule),
    Edit(EditRule),
    Remove(RemoveRule),
    Answer(AnswerRule),
}

impl RuleRequest {
    pub fn scope(&self) -> Scope {
        match self {
            RuleRequest::Add(r) => r.scope,
            RuleRequest::Edit(r) => r.scope,
            RuleRequest::Remove(r) => r.scope,
            RuleRequest::Answer(r) => r.scope,
        }
    }
}

impl AddRule {
    /// The `W_INSTRUCTION_SELECTOR` warnings of the given selectors under `pack`.
    pub fn selector_warnings(&self, pack: &Pack) -> Vec<Issue> {
        selector_warnings(pack, None, self.applies_to.as_deref())
    }
}

impl EditRule {
    /// The `W_INSTRUCTION_SELECTOR` warnings of the given selectors under `pack`.
    pub fn selector_warnings(&self, pack: &Pack) -> Vec<Issue> {
        selector_warnings(pack, Some(&self.id), self.applies_to.as_deref())
    }
}

/// The existing selector warnings for `selectors`, as the instruction check words them. A new
/// instruction has no id yet, so its warnings carry no block.
fn selector_warnings(
    pack: &Pack,
    id: Option<&BlockId>,
    selectors: Option<&[String]>,
) -> Vec<Issue> {
    let Some(selectors) = selectors else {
        return vec![];
    };
    let scratch = scratch_instruction(id, selectors);
    let mut out = instruction::selector_warnings(pack, &scratch);
    if id.is_none() {
        for w in &mut out {
            w.block = None;
        }
    }
    out
}

fn scratch_instruction(id: Option<&BlockId>, selectors: &[String]) -> Block {
    Block {
        id: id
            .cloned()
            .unwrap_or_else(|| BlockId::parse("ai-00000").expect("a valid placeholder id")),
        block_type: INSTRUCTION.to_string(),
        parent: None,
        order: String::new(),
        fields: [(
            instruction::APPLIES_TO.to_string(),
            rdy_model::FieldValue::List(
                selectors
                    .iter()
                    .map(|s| rdy_model::FieldValue::Str(s.clone()))
                    .collect(),
            ),
        )]
        .into(),
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
        read_identity: None,
    }
}

// ---- refusals ----

fn bad(message: String, fix: String) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

fn required(path: &str, ty: &str) -> RdyError {
    bad(
        format!("{TOOL}: parameter \"{path}\" is required"),
        format!("pass \"{path}\" as {ty}"),
    )
}

fn not_allowed(path: &str, form: &str) -> RdyError {
    bad(
        format!("{TOOL}: parameter \"{path}\" is not allowed with {form}"),
        format!("remove \"{path}\" or use the matching rdy_rule form"),
    )
}

fn wrong_type(path: &str, expected: &str, actual: &Value) -> RdyError {
    bad(
        format!(
            "{TOOL}: parameter \"{path}\" must be {expected}, not {}",
            actual_word(actual)
        ),
        format!("pass \"{path}\" as {expected}"),
    )
}

fn actual_word(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn unknown(path: &str, key: &str, known: &[&str]) -> RdyError {
    let all = known.join(", ");
    let fix = match nearest(key, known) {
        Some(y) => format!("did you mean \"{y}\"? the parameters here are {all}"),
        None => format!("remove \"{path}\"; the parameters here are {all}"),
    };
    bad(format!("{TOOL}: unknown parameter \"{path}\""), fix)
}

fn choices(path: &str, list: &[&str]) -> RdyError {
    let all = list.join(", ");
    bad(
        format!("{TOOL}: \"{path}\" must be {all}"),
        format!("pass one of: {all}"),
    )
}

fn blank(path: &str) -> RdyError {
    bad(
        format!("{TOOL}: \"{path}\" must not be blank"),
        format!("pass nonblank text for \"{path}\""),
    )
}

fn exceeds(path: &str, limit: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("{TOOL}: \"{path}\" exceeds {limit}"),
        format!("use at most {limit} for \"{path}\""),
    )
}

/// The refusal of an empty evidence: the relay's own.
fn no_evidence() -> RdyError {
    RelayRefusal::NoEvidence.error()
}

// ---- parsing ----

/// The checked request in `v`, the tool's arguments after the harness took its own keys out.
/// Nothing is read or written outside `v`.
pub fn parse_request(v: &Value) -> Result<RuleRequest, RdyError> {
    let Value::Object(top) = v else {
        return Err(bad(
            format!(
                "{TOOL}: the parameters must be an object, not {}",
                actual_word(v)
            ),
            "pass the parameters as an object".to_string(),
        ));
    };
    for k in top.keys() {
        if !KEYS.contains(&k.as_str()) {
            return Err(unknown(k, k, &KEYS));
        }
    }
    for k in KEYS {
        if let Some(x) = top.get(k) {
            top_type(k, x)?;
        }
    }
    let scope = match top.get("scope") {
        None => return Err(required("scope", "a string")),
        Some(x) => scope_of(x)?,
    };
    let op = match top.get("op") {
        None => {
            return match top.get("answer") {
                Some(answer) => answer_form(scope, top, answer).map(RuleRequest::Answer),
                None => Err(required("op", "a string")),
            };
        }
        Some(x) => match x.as_str() {
            Some(o) if OPS.contains(&o) => o,
            _ => return Err(choices("op", &OPS)),
        },
    };
    let form = match op {
        "add" => Form::Add,
        "edit" => Form::Edit,
        _ => Form::Remove,
    };
    for k in form.refused() {
        if top.contains_key(*k) {
            return Err(not_allowed(k, op));
        }
    }
    for k in form.required() {
        if !top.contains_key(*k) {
            return Err(required(k, "a string"));
        }
    }
    let text = match form {
        Form::Remove => String::new(),
        _ => text_of(top.get("text").and_then(Value::as_str).unwrap_or_default())?,
    };
    let id = match form {
        Form::Add => None,
        _ => Some(id_of(
            top.get("id").and_then(Value::as_str).unwrap_or_default(),
        )?),
    };
    let ruleset = match top.get("ruleset").and_then(Value::as_str) {
        Some(r) => Some(ruleset_of(scope, r)?),
        None => None,
    };
    let applies_to = match top.get("applies_to") {
        Some(list) => Some(selectors_of(id.as_ref(), list)?),
        None => None,
    };
    let why = why_of(top.get("why").and_then(Value::as_str).unwrap_or_default())?;
    let relay = relay_of(
        "question",
        top.get("evidence").and_then(Value::as_str),
        top.get("question").and_then(Value::as_str),
        top.get("session_ref").and_then(Value::as_str),
    )?;
    Ok(match form {
        Form::Add => RuleRequest::Add(AddRule {
            scope,
            text,
            why,
            ruleset,
            applies_to,
            relay,
        }),
        Form::Edit => RuleRequest::Edit(EditRule {
            scope,
            id: id.expect("an edit has an id"),
            text,
            why,
            ruleset,
            applies_to,
            relay,
        }),
        Form::Remove => RuleRequest::Remove(RemoveRule {
            scope,
            id: id.expect("a remove has an id"),
            why,
            relay,
        }),
    })
}

#[derive(Clone, Copy)]
enum Form {
    Add,
    Edit,
    Remove,
}

impl Form {
    fn required(self) -> &'static [&'static str] {
        match self {
            Form::Add => &["text", "why", "evidence"],
            Form::Edit => &["id", "text", "why", "evidence"],
            Form::Remove => &["id", "why", "evidence"],
        }
    }

    fn refused(self) -> &'static [&'static str] {
        match self {
            Form::Add => &["id", "answer"],
            Form::Edit => &["answer"],
            Form::Remove => &["text", "ruleset", "applies_to", "answer"],
        }
    }
}

/// The structural check of one top-level value: its type, and the cheap bound of a list.
fn top_type(key: &str, x: &Value) -> Result<(), RdyError> {
    match key {
        "answer" => {
            if !x.is_object() {
                return Err(wrong_type(key, "an object", x));
            }
        }
        "applies_to" => {
            let Value::Array(items) = x else {
                return Err(wrong_type(key, "a list of strings", x));
            };
            if items.len() > MAX_SELECTORS {
                return Err(exceeds(key, &format!("{MAX_SELECTORS} selectors")));
            }
            for (i, item) in items.iter().enumerate() {
                if !item.is_string() {
                    return Err(wrong_type(&format!("{key}[{i}]"), "a string", item));
                }
            }
        }
        _ => {
            if !x.is_string() {
                return Err(wrong_type(key, "a string", x));
            }
        }
    }
    Ok(())
}

fn scope_of(x: &Value) -> Result<Scope, RdyError> {
    match x.as_str() {
        Some("person") => Ok(Scope::Person),
        Some("project") => Ok(Scope::Project),
        _ => Err(choices("scope", &SCOPES)),
    }
}

fn text_of(t: &str) -> Result<String, RdyError> {
    if t.len() > MAX_TEXT_BYTES {
        return Err(exceeds("text", &format!("{MAX_TEXT_BYTES} bytes")));
    }
    if t.trim().is_empty() {
        return Err(blank("text"));
    }
    Ok(t.to_string())
}

fn why_of(t: &str) -> Result<String, RdyError> {
    // more than 8000 bytes holds more than 2000 characters, so one check names both
    if t.len() > MAX_WHY_BYTES || t.chars().count() > MAX_WHY_CHARS {
        return Err(exceeds("why", &format!("{MAX_WHY_CHARS} characters")));
    }
    if t.trim().is_empty() {
        return Err(blank("why"));
    }
    if t.contains(['\r', '\n']) {
        return Err(bad(
            format!("{TOOL}: \"why\" must be one line"),
            "give the reason in one line".to_string(),
        ));
    }
    Ok(t.to_string())
}

fn id_of(t: &str) -> Result<BlockId, RdyError> {
    if t.len() > MAX_ID_BYTES {
        return Err(exceeds("id", &format!("{MAX_ID_BYTES} bytes")));
    }
    if t.trim().is_empty() {
        return Err(blank("id"));
    }
    BlockId::parse(t).map_err(|e| {
        bad(
            e.to_string(),
            "pass the id of an instruction, such as ai-ABCDE".to_string(),
        )
    })
}

/// A ruleset: a doc id or a path, or the destination's own handle on one. The other
/// destination's handle and a built-in handle are outside the scope.
fn ruleset_of(scope: Scope, t: &str) -> Result<RulesetRef, RdyError> {
    if t.len() > MAX_RULESET_BYTES {
        return Err(exceeds("ruleset", &format!("{MAX_RULESET_BYTES} bytes")));
    }
    if t.trim().is_empty() {
        return Err(blank("ruleset"));
    }
    let outside = || {
        bad(
            format!("ruleset {t} is outside the selected scope"),
            "use a ruleset in the selected project".to_string(),
        )
    };
    let rest = match t.split_once(':') {
        Some(("rdy", _)) => return Err(outside()),
        Some((l @ ("person" | "project"), rest)) => {
            if l != scope.handle() {
                return Err(outside());
            }
            rest
        }
        _ => t,
    };
    if rest.trim().is_empty() {
        return Err(blank("ruleset"));
    }
    Ok(match BlockId::parse(rest) {
        Ok(id) => RulesetRef::Doc(id),
        Err(_) => RulesetRef::Path(rest.to_string()),
    })
}

/// The selectors: each at most 256 bytes and in the instruction grammar. A bad selector is
/// `E_VALIDATION` with the instruction check's issue for it, in list order.
fn selectors_of(id: Option<&BlockId>, list: &Value) -> Result<Vec<String>, RdyError> {
    let items: Vec<&str> = list
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if items.is_empty() {
        return Err(RdyError::new(
            E_LIMIT,
            format!("{TOOL}: \"applies_to\" needs 1 to {MAX_SELECTORS} selectors"),
            SELECTOR_LIMIT_FIX,
        ));
    }
    if items.iter().any(|s| s.len() > MAX_SELECTOR_BYTES) {
        return Err(exceeds(
            "applies_to",
            &format!("{MAX_SELECTOR_BYTES} bytes"),
        ));
    }
    let who = match id {
        Some(id) => format!("instruction {id}"),
        None => "instruction".to_string(),
    };
    let issues: Vec<Issue> = items
        .iter()
        .filter(|s| !selector_ok(s))
        .map(|s| Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: id.cloned(),
            message: format!("{who} has invalid selector {s}"),
            fix: SELECTOR_FIX.to_string(),
        })
        .collect();
    if let Some(first) = issues.first() {
        let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
        e.issues = issues;
        return Err(e);
    }
    Ok(items.into_iter().map(str::to_string).collect())
}

/// The relay words of a request. `evidence` is required and nonblank; the character limits are
/// the relay's own, checked on the submitted bytes before any trimming.
fn relay_of(
    question_path: &str,
    evidence: Option<&str>,
    question: Option<&str>,
    session_ref: Option<&str>,
) -> Result<RelayWords, RdyError> {
    let long = |t: &str| t.len() > MAX_EVIDENCE_BYTES || t.chars().count() > MAX_EVIDENCE_CHARS;
    if evidence.is_some_and(long) || question.is_some_and(long) {
        return Err(RelayRefusal::TooLong.error());
    }
    let evidence = evidence.unwrap_or_default();
    if evidence.trim().is_empty() {
        return Err(no_evidence());
    }
    if let Some(q) = question
        && q.trim().is_empty()
    {
        return Err(blank(question_path));
    }
    if let Some(s) = session_ref {
        if s.len() > MAX_SESSION_REF_BYTES || s.chars().count() > MAX_SESSION_REF_CHARS {
            return Err(RelayRefusal::SessionRefTooLong.error());
        }
        if s.trim().is_empty() {
            return Err(blank("session_ref"));
        }
    }
    Ok(RelayWords {
        evidence: evidence.to_string(),
        question: question.map(str::to_string),
        session_ref: session_ref.map(str::to_string),
    })
}

fn answer_form(
    scope: Scope,
    top: &Map<String, Value>,
    answer: &Value,
) -> Result<AnswerRule, RdyError> {
    for k in KEYS {
        if k != "scope" && k != "answer" && top.contains_key(k) {
            return Err(not_allowed(k, "answer"));
        }
    }
    let Value::Object(a) = answer else {
        return Err(wrong_type("answer", "an object", answer));
    };
    for k in a.keys() {
        if !ANSWER_KEYS.contains(&k.as_str()) {
            return Err(unknown(&format!("answer.{k}"), k, &ANSWER_KEYS));
        }
    }
    for k in ANSWER_KEYS {
        if let Some(x) = a.get(k)
            && !x.is_string()
        {
            return Err(wrong_type(&format!("answer.{k}"), "a string", x));
        }
    }
    for k in &ANSWER_KEYS[..4] {
        if !a.contains_key(*k) {
            return Err(required(&format!("answer.{k}"), "a string"));
        }
    }
    let get = |k: &str| a.get(k).and_then(Value::as_str);
    let item_ref = ref_of(get("ref").unwrap_or_default())?;
    let option = option_of(get("option").unwrap_or_default())?;
    let card_hash = hash_of(get("card_hash").unwrap_or_default())?;
    let relay = relay_of("answer.question", get("evidence"), get("question"), None)?;
    Ok(AnswerRule {
        scope,
        item_ref,
        option,
        card_hash,
        relay,
    })
}

/// An item of this flow: a proposal (`P-<id>@<n>`), a decision at a version (`<id>@<12 hex>`),
/// or the `defer:` and `advice:` cards of a proposal version. Other item refs belong to other
/// flows.
fn ref_of(r: &str) -> Result<String, RdyError> {
    if r.len() > MAX_REF_BYTES {
        return Err(exceeds("answer.ref", &format!("{MAX_REF_BYTES} bytes")));
    }
    if r.trim().is_empty() {
        return Err(blank("answer.ref"));
    }
    if !is_item_ref(r) {
        return Err(bad(
            format!("{r} is not an item ref"),
            "pass an item ref as rdy_rule gives it: P-<id>@<n>, <D-id>@<version>, defer:P-<id>@<n> or advice:P-<id>@<n>".to_string(),
        ));
    }
    if r.starts_with("reaffirm:") || !r.contains('@') {
        return Err(other_flow(r));
    }
    Ok(r.to_string())
}

pub(crate) fn other_flow(r: &str) -> RdyError {
    bad(
        format!("item {r} does not belong to a rules-from-chat proposal"),
        "answer it through rdy_answer in its project".to_string(),
    )
}

fn option_of(o: &str) -> Result<String, RdyError> {
    if o.len() > MAX_OPTION_BYTES {
        return Err(exceeds(
            "answer.option",
            &format!("{MAX_OPTION_BYTES} bytes"),
        ));
    }
    if o.trim().is_empty() {
        return Err(blank("answer.option"));
    }
    if o.starts_with(crate::check::verdicts::ACCEPT_PREFIX)
        && crate::check::verdicts::parse_accept_option(o).is_none()
    {
        return Err(bad(
            format!("{TOOL}: \"answer.option\" is not a verdict acceptance option"),
            "pass the option exactly as the card gives it".to_string(),
        ));
    }
    Ok(o.to_string())
}

fn hash_of(h: &str) -> Result<Hash, RdyError> {
    h.parse().map_err(|_| {
        bad(
            format!("{TOOL}: \"answer.card_hash\" must be a full lowercase hash"),
            "pass the card_hash of the card the person saw".to_string(),
        )
    })
}

// ---- replies ----

/// The project a reply names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    pub repo_id: String,
}

/// The rule a reply names: a retired rule keeps its instruction id and its prior text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleInfo {
    pub id: String,
    pub text: String,
    pub ruleset: String,
    pub retired: bool,
}

/// One card of the flow: its item ref, its hash and the whole card.
#[derive(Clone, Debug, PartialEq)]
pub struct CardEntry {
    pub item_ref: String,
    pub card_hash: String,
    pub card: Value,
}

impl CardEntry {
    fn to_json(&self) -> Value {
        json!({"ref": self.item_ref, "card_hash": self.card_hash, "card": self.card})
    }
}

/// What the call reached.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The change landed; `landing` is the existing landing JSON.
    Landed { landing: Value },
    /// The call stopped at the person's card.
    Card {
        view: String,
        proposal_ref: String,
        item_ref: String,
        card_hash: String,
        card: Value,
        cards: Vec<CardEntry>,
    },
    /// The proposal is not ready; `issues` are the existing issues, in order.
    NotReady {
        view: String,
        item_ref: String,
        issues: Vec<Value>,
    },
    /// An answer that did not land the change; `answer` is the existing answer JSON.
    Answered {
        view: String,
        proposal_ref: String,
        item_ref: String,
        answer: Value,
        remaining_cards: Vec<CardEntry>,
    },
}

/// A reply of `rdy_rule`.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub scope: Scope,
    pub project: ProjectInfo,
    pub rule: RuleInfo,
    /// The decision that backs the rule: the new one, or for a remove the revoking one.
    pub decision: String,
    pub outcome: Outcome,
}

impl Reply {
    /// The reply as the tool returns it.
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "scope": self.scope.as_str(),
            "project": {"name": self.project.name, "repo_id": self.project.repo_id},
            "rule": {
                "id": self.rule.id,
                "text": self.rule.text,
                "ruleset": self.rule.ruleset,
                "retired": self.rule.retired,
            },
            "decision": self.decision,
        });
        let m = v.as_object_mut().expect("an object");
        match &self.outcome {
            Outcome::Landed { landing } => {
                m.insert("landing".into(), landing.clone());
            }
            Outcome::Card {
                view,
                proposal_ref,
                item_ref,
                card_hash,
                card,
                cards,
            } => {
                m.insert("state".into(), json!("card"));
                m.insert("view".into(), json!(view));
                m.insert("proposal_ref".into(), json!(proposal_ref));
                m.insert("ref".into(), json!(item_ref));
                m.insert("card_hash".into(), json!(card_hash));
                m.insert("card".into(), card.clone());
                m.insert(
                    "cards".into(),
                    Value::Array(cards.iter().map(CardEntry::to_json).collect()),
                );
            }
            Outcome::NotReady {
                view,
                item_ref,
                issues,
            } => {
                m.insert("state".into(), json!("not_ready"));
                m.insert("view".into(), json!(view));
                m.insert("ref".into(), json!(item_ref));
                m.insert("issues".into(), Value::Array(issues.clone()));
            }
            Outcome::Answered {
                view,
                proposal_ref,
                item_ref,
                answer,
                remaining_cards,
            } => {
                m.insert("state".into(), json!("answered"));
                m.insert("view".into(), json!(view));
                m.insert("proposal_ref".into(), json!(proposal_ref));
                m.insert("ref".into(), json!(item_ref));
                m.insert("answer".into(), answer.clone());
                m.insert(
                    "remaining_cards".into(),
                    Value::Array(remaining_cards.iter().map(CardEntry::to_json).collect()),
                );
            }
        }
        v
    }
}

/// The `corrected` object of a stable-content or shared-decision refusal. The blocks are in
/// ascending order, at most 1024; a longer list is `E_LIMIT`, never a cut list.
pub const MAX_BLOCKERS: usize = 1_024;

/// The `normal_path` correction for `blocks`, or `E_LIMIT` when there are more than
/// [`MAX_BLOCKERS`].
pub fn normal_path(
    scope: Scope,
    project: &ProjectInfo,
    mut blocks: Vec<String>,
) -> Result<Value, RdyError> {
    if blocks.len() > MAX_BLOCKERS {
        return Err(exceeds("blocks", &format!("{MAX_BLOCKERS} ids")));
    }
    blocks.sort();
    Ok(json!({
        "state": "normal_path",
        "scope": scope.as_str(),
        "project": {"name": project.name, "repo_id": project.repo_id},
        "blocks": blocks,
    }))
}
