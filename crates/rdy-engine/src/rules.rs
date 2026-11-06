//! Rules from chat (FW-MH.rules, D-ZSRBN): one rule change as one normal write, in a private view
//! named by the request, and its proposal.
//!
//! A request becomes a default doc when needed, a direct-ruling decision and an instruction, all
//! in one `WriteRequest`, so no failure can leave a reason with no rule. Targets, decision
//! dependents and stable content are checked first, on canon, before the view exists. The change's
//! message is the request's receipt: a repeated request finds its view by name, checks its owner
//! and its receipt ([`lifecycle`]), and continues from what is durable, never writing the content
//! again. Landing is the next step's: a rule change stops here at its proposal.
use crate::decide::{DecisionInput, decision_op, inherited_links};
use crate::error::*;
use crate::rule_types::{
    AddRule, EditRule, ProjectInfo, RelayWords, RemoveRule, RuleInfo, RuleRequest, RulesetRef,
    Scope as RuleScope, normal_path,
};
use crate::write::{LinkInput, MoveTo, OpInput, WriteRequest};
use crate::{Materialized, Rdy, ReadState, Scope, Session};
use rdy_model::{Block, BlockId, decision};
use rdy_schema::Issue;
use rdy_schema::instruction::{ALL, APPLIES_TO, INSTRUCTION, LOCKED, REQUIRED_FIELD};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

mod lifecycle;

pub use lifecycle::{MAX_RECEIPT_BYTES, RECEIPT_VERSION};

/// The most ops one rule change generates.
pub const MAX_RULE_OPS: usize = 1_024;

/// The projects a rule can land in: the person's meta project, or any other. The caller knows
/// which from the configured path; a session on meta is `Meta` for both scope spellings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Meta,
    Project,
}

impl Destination {
    /// The path, and key, of the destination's default ruleset, and its title.
    fn default_doc(self) -> (&'static str, &'static str) {
        match self {
            Destination::Meta => ("my-rules", "My rules"),
            Destination::Project => ("project-rules", "Project rules"),
        }
    }
}

/// Who asks for a rule change, in the destination project.
#[derive(Clone, Copy, Debug)]
pub struct RuleCaller<'a> {
    /// The caller's own session: it owns the view and writes the change.
    pub session: &'a Session,
    /// The configured human whose words the caller relays.
    pub person: &'a str,
    /// The session reference a request without one reads: `mcp` or `cli`.
    pub surface: &'a str,
    pub destination: Destination,
}

/// The three forms of a request that change a rule.
#[derive(Clone, Copy, Debug)]
pub enum RuleMutation<'a> {
    Add(&'a AddRule),
    Edit(&'a EditRule),
    Remove(&'a RemoveRule),
}

impl<'a> RuleMutation<'a> {
    /// The change `req` asks for; `None` for an answer.
    pub fn of(req: &'a RuleRequest) -> Option<RuleMutation<'a>> {
        match req {
            RuleRequest::Add(r) => Some(RuleMutation::Add(r)),
            RuleRequest::Edit(r) => Some(RuleMutation::Edit(r)),
            RuleRequest::Remove(r) => Some(RuleMutation::Remove(r)),
            RuleRequest::Answer(_) => None,
        }
    }

    fn scope(self) -> RuleScope {
        match self {
            RuleMutation::Add(r) => r.scope,
            RuleMutation::Edit(r) => r.scope,
            RuleMutation::Remove(r) => r.scope,
        }
    }

    fn op(self) -> &'static str {
        match self {
            RuleMutation::Add(_) => "add",
            RuleMutation::Edit(_) => "edit",
            RuleMutation::Remove(_) => "remove",
        }
    }

    fn id(self) -> Option<&'a BlockId> {
        match self {
            RuleMutation::Add(_) => None,
            RuleMutation::Edit(r) => Some(&r.id),
            RuleMutation::Remove(r) => Some(&r.id),
        }
    }

    fn text(self) -> Option<&'a str> {
        match self {
            RuleMutation::Add(r) => Some(&r.text),
            RuleMutation::Edit(r) => Some(&r.text),
            RuleMutation::Remove(_) => None,
        }
    }

    fn why(self) -> &'a str {
        match self {
            RuleMutation::Add(r) => &r.why,
            RuleMutation::Edit(r) => &r.why,
            RuleMutation::Remove(r) => &r.why,
        }
    }

    fn ruleset(self) -> Option<&'a RulesetRef> {
        match self {
            RuleMutation::Add(r) => r.ruleset.as_ref(),
            RuleMutation::Edit(r) => r.ruleset.as_ref(),
            RuleMutation::Remove(_) => None,
        }
    }

    fn applies_to(self) -> Option<&'a [String]> {
        match self {
            RuleMutation::Add(r) => r.applies_to.as_deref(),
            RuleMutation::Edit(r) => r.applies_to.as_deref(),
            RuleMutation::Remove(_) => None,
        }
    }

    fn relay(self) -> &'a RelayWords {
        match self {
            RuleMutation::Add(r) => &r.relay,
            RuleMutation::Edit(r) => &r.relay,
            RuleMutation::Remove(r) => &r.relay,
        }
    }

    /// The statement of the decision this request records. It holds the rule's exact words, but
    /// is not the rule's text: a decision and a rule with one text are a `duplicate_definition`
    /// tension, which would hold every rule back from landing.
    fn statement(self) -> String {
        match self {
            RuleMutation::Add(r) => format!("Rule: {}", r.text),
            RuleMutation::Edit(r) => format!("Rule: {}", r.text),
            RuleMutation::Remove(r) => format!("Retire rule {}: {}", r.id, r.why),
        }
    }
}

/// Where a prepared request stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedState {
    /// Its proposal is open: `reference` is `P-<id>@<n>`, `version` the version's hash.
    Proposed { reference: String, version: String },
    /// An earlier call's proposal landed: `landing` is the landing's hash.
    Landed { landing: String },
}

/// A rule request after its durable steps: a complete change in its view, proposed or landed.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepared {
    pub scope: RuleScope,
    pub project: ProjectInfo,
    pub rule: RuleInfo,
    /// The decision that backs the rule: the new one, or for a remove the revoking one.
    pub decision: String,
    pub view: String,
    pub state: PreparedState,
    /// The warnings of this call's own write and propose, and of a cleanup that could not write.
    pub warnings: Vec<Issue>,
}

/// A failure a test injects into [`Rdy::rule_prepare`].
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleFault {
    /// Op `i` of the change's write is one the write refuses; past the last op, nothing happens.
    BadOp(usize),
    /// The process stops after the draft: an error, with no cleanup.
    StopAfterDraft,
    /// The propose fails.
    Propose,
    /// The propose fails, and so does closing the view.
    ProposeAndClose,
    /// Closing the view fails.
    Close,
}

thread_local! {
    static FAULT: Cell<Option<RuleFault>> = const { Cell::new(None) };
}

/// Test hook: runs `f` with `fault` injected on this thread. No CLI, MCP, config or environment
/// path reaches it.
#[doc(hidden)]
pub fn with_rule_fault_for_tests<T>(fault: RuleFault, f: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            FAULT.with(|c| c.set(None));
        }
    }
    FAULT.with(|c| c.set(Some(fault)));
    let _clear = Clear;
    f()
}

fn fault() -> Option<RuleFault> {
    FAULT.with(Cell::get)
}

fn injected(step: &str) -> RdyError {
    RdyError::new(
        E_INTERNAL,
        format!("{step} failed, as a test asked"),
        "this only happens in a test",
    )
}

// ---- refusals ----

fn not_found(value: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("rule target {value} was not found"),
        "read the selected project and use an existing instruction or ruleset",
    )
}

fn wrong_type(id: &BlockId, actual: &str, expected: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("rule target {id} is a {actual}, not {expected}"),
        "use an instruction id for id and a doc for ruleset",
    )
}

fn no_whole_reason(id: &BlockId) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("instruction {id} needs one live whole-rule decision"),
        "use the normal decision and instruction workflow",
    )
}

fn too_many_ops() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("rdy_rule: \"ops\" exceeds {MAX_RULE_OPS} ops"),
        format!("use at most {MAX_RULE_OPS} ops for \"ops\""),
    )
}

fn unsuitable_default(path: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("default ruleset {path} is not a draft required ruleset"),
        "name a suitable ruleset, or change this doc through the normal path",
    )
}

// ---- the change ----

/// The ruleset an add writes to, or a moved edit: an existing doc, or the default doc to create.
enum Target {
    Existing(BlockId),
    NewDefault {
        path: &'static str,
        title: &'static str,
    },
}

/// The rule an edit or a remove changes, with its one reason.
struct Current {
    rule: Block,
    version: String,
    reason: Block,
}

fn is_doc(b: &Block) -> bool {
    b.block_type == rdy_model::tree::DOC
}

/// The doc `r` names in `m`: a doc id, a key or a path of the destination.
fn named_doc(m: &Materialized, r: &RulesetRef) -> Result<BlockId, RdyError> {
    let (text, resolved) = match r {
        RulesetRef::Doc(id) => (id.to_string(), Ok(id.clone())),
        RulesetRef::Path(p) => (p.clone(), crate::keys::resolve_text(m, p)),
    };
    let id = resolved.map_err(|_| not_found(&text))?;
    let b = m.get(&id).ok_or_else(|| not_found(&text))?;
    if !is_doc(b) {
        return Err(wrong_type(&id, &b.block_type, "doc"));
    }
    Ok(id)
}

/// The default doc of `d` in `m` when one stands at its path or holds its key and suits a ruleset;
/// `NewDefault` when none does. A doc that does not suit is never silently changed.
fn default_target(m: &Materialized, d: Destination) -> Result<Target, RdyError> {
    let (path, title) = d.default_doc();
    let found = m.child_named(None, path).ok().or_else(|| {
        m.iter()
            .find(|b| crate::keys::key_of(b).is_some_and(|k| rdy_model::key::fold(k) == path))
            .map(|b| b.id.clone())
    });
    let Some(id) = found else {
        return Ok(Target::NewDefault { path, title });
    };
    let b = m.get(&id).ok_or_else(|| unsuitable_default(path))?;
    let suits = is_doc(b)
        && b.parent.is_none()
        && b.field_str("lifecycle") == Some("draft")
        && b.fields.get(REQUIRED_FIELD) == Some(&rdy_model::FieldValue::Bool(true))
        && b.field_str("key") == Some(path);
    if !suits {
        return Err(unsuitable_default(path));
    }
    Ok(Target::Existing(id))
}

/// The rule `id` in canon, with its one live, whole-rule reason.
fn current_rule(st: &ReadState, m: &Materialized, id: &BlockId) -> Result<Current, RdyError> {
    let rule = m.get(id).ok_or_else(|| not_found(id.as_str()))?;
    if rule.block_type != INSTRUCTION {
        return Err(wrong_type(id, &rule.block_type, INSTRUCTION));
    }
    let reasons = rule
        .links
        .get(decision::DECIDED_BY)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [reason] = reasons else {
        return Err(no_whole_reason(id));
    };
    if reason.part.is_some() {
        return Err(no_whole_reason(id));
    }
    let g = st.graph(&Scope::Canon)?;
    if !matches!(
        g.status(&reason.target),
        Some(rdy_graph::DerivedStatus::Live)
    ) {
        return Err(no_whole_reason(id));
    }
    let reason = m.get(&reason.target).ok_or_else(|| no_whole_reason(id))?;
    let version = m
        .version_of(id)
        .ok_or_else(|| not_found(id.as_str()))?
        .to_string();
    Ok(Current {
        rule: rule.clone(),
        version,
        reason: reason.clone(),
    })
}

/// The blocks other than `rule` that rest on decision `reason`, in canon and in every other open
/// view: a `decided_by`, `implements`, `depends_on` or `conditional_on` link to it, or its own
/// governing `affects` link. Containment and a mention in prose are not dependence.
pub(crate) fn dependents(
    st: &ReadState,
    reason: &BlockId,
    rule: &BlockId,
    own_view: &str,
) -> Result<BTreeSet<String>, RdyError> {
    const RESTS_ON: [&str; 4] = [
        decision::DECIDED_BY,
        decision::IMPLEMENTS,
        decision::DEPENDS_ON,
        decision::CONDITIONAL_ON,
    ];
    let mut scopes = vec![Scope::Canon];
    scopes.extend(
        st.views_with_changes()
            .into_iter()
            .filter(|(v, _)| v != own_view)
            .map(|(v, _)| Scope::View(v)),
    );
    let mut out = BTreeSet::new();
    for scope in &scopes {
        let g = st.graph(scope)?;
        for e in g.incoming(reason) {
            if e.from != *rule
                && matches!(&e.kind, rdy_graph::EdgeKind::Link(n) if RESTS_ON.contains(&n.as_str()))
            {
                out.insert(e.from.to_string());
            }
        }
        for e in g.outgoing(reason) {
            if e.to != *rule
                && matches!(&e.kind, rdy_graph::EdgeKind::Link(n) if n == decision::AFFECTS)
            {
                out.insert(e.to.to_string());
            }
        }
    }
    Ok(out)
}

fn project_of(st: &ReadState) -> Result<ProjectInfo, RdyError> {
    Ok(ProjectInfo {
        name: st.project_name(),
        repo_id: st.repo_id()?.to_string(),
    })
}

fn shared_refusal(
    st: &ReadState,
    m: RuleMutation<'_>,
    reason: &BlockId,
    blocks: BTreeSet<String>,
) -> Result<RdyError, RdyError> {
    let corrected = normal_path(m.scope(), &project_of(st)?, blocks.into_iter().collect())?;
    Ok(RdyError::new(
        E_BAD_INPUT,
        format!("decision {reason} also governs other blocks"),
        "brief the returned blocks and use the normal decision workflow",
    )
    .with_corrected(corrected))
}

fn stable_refusal(
    st: &ReadState,
    m: RuleMutation<'_>,
    blocks: BTreeSet<String>,
) -> Result<RdyError, RdyError> {
    let corrected = normal_path(m.scope(), &project_of(st)?, blocks.into_iter().collect())?;
    Ok(RdyError::new(
        crate::brief::E_BRIEF_REQUIRED,
        "rdy_rule cannot change stable content",
        "brief the returned blocks and use the normal write, propose and answer path",
    )
    .with_corrected(corrected))
}

fn link(target: impl ToString) -> LinkInput {
    LinkInput {
        target: target.to_string(),
        part: None,
    }
}

/// The decision of request `m`: a direct ruling on the person's relayed words, under the
/// destination's decision home.
fn rule_decision(c: &RuleCaller<'_>, m: RuleMutation<'_>) -> DecisionInput {
    let relay = m.relay();
    let s = c.session;
    let run = s
        .run
        .as_deref()
        .map(|r| format!(" (run {r})"))
        .unwrap_or_default();
    let mut body = String::new();
    if let Some(q) = &relay.question {
        body.push_str(&format!("Question: {q}\n\n"));
    }
    body.push_str(&format!(
        "Session: {}",
        relay.session_ref.as_deref().unwrap_or(c.surface)
    ));
    let mut fields =
        BTreeMap::from([("built".to_string(), json!(rdy_schema::as_built::BUILT_NONE))]);
    if matches!(m, RuleMutation::Remove(_)) {
        // a retirement applies itself: no content carries it (cc16)
        fields.insert("needs_content".to_string(), json!(false));
    }
    DecisionInput {
        statement: m.statement(),
        why: Some(m.why().to_string()),
        authority_basis: Some("direct_ruling".into()),
        attested_by: Some(format!(
            "{}, relayed by {}{run}; evidence: {}",
            c.person, s.actor, relay.evidence
        )),
        reversibility: Some("two_way".into()),
        body,
        fields,
        ..Default::default()
    }
}

impl Rdy {
    /// The checks and the ops of request `m` on canon: targets, the rule's one reason, the
    /// reason's other dependents, then stable content. Nothing is written.
    fn rule_plan(
        &self,
        c: &RuleCaller<'_>,
        m: RuleMutation<'_>,
        view: &str,
    ) -> Result<Vec<OpInput>, RdyError> {
        let st = self.read();
        let canon = st.materialized(&Scope::Canon)?;
        let target = match m {
            RuleMutation::Add(_) => Some(match m.ruleset() {
                Some(r) => Target::Existing(named_doc(&canon, r)?),
                None => default_target(&canon, c.destination)?,
            }),
            RuleMutation::Edit(_) | RuleMutation::Remove(_) => match m.ruleset() {
                Some(r) => Some(Target::Existing(named_doc(&canon, r)?)),
                None => None,
            },
        };
        let current = match m.id() {
            Some(id) => Some(current_rule(&st, &canon, id)?),
            None => None,
        };
        if let Some(cur) = &current {
            let others = dependents(&st, &cur.reason.id, &cur.rule.id, view)?;
            if !others.is_empty() {
                return Err(shared_refusal(&st, m, &cur.reason.id, others)?);
            }
        }
        let d = rule_decision(c, m);
        let mut extra = vec![];
        if let Some(cur) = &current {
            if matches!(m, RuleMutation::Edit(_)) {
                let (_, inherited) = inherited_links(&st, &Scope::Canon, &cur.reason, &d)?;
                extra = inherited;
                extra.push((decision::SUPERSEDES.to_string(), link(&cur.reason.id)));
            } else {
                extra.push((decision::REVOKES.to_string(), link(&cur.reason.id)));
            }
        }
        let mut ops: Vec<OpInput> = vec![];
        let parent = match &target {
            Some(Target::NewDefault { path, title }) => {
                let doc_type = crate::paths::default_doc_type(&self.pack)?;
                let mut values = vec![json!({
                    "op": "create", "type": rdy_model::tree::DOC, "id": "$doc",
                    "fields": {
                        "name": path, "key": path, "title": title, "doc_type": doc_type,
                        "lifecycle": "draft", (REQUIRED_FIELD): true,
                    },
                })];
                values.extend(crate::paths::skeleton_ops(&self.pack, &doc_type, "$doc"));
                if values.len() + 2 > MAX_RULE_OPS {
                    return Err(too_many_ops());
                }
                ops = serde_json::from_value(Value::Array(values)).map_err(|e| {
                    RdyError::new(
                        E_INTERNAL,
                        format!("the default ruleset ops do not parse: {e}"),
                        "report this as a bug in rdy",
                    )
                })?;
                Some("$doc".to_string())
            }
            Some(Target::Existing(id)) => Some(id.to_string()),
            None => None,
        };
        ops.push(decision_op(&d, extra));
        let reason_link =
            BTreeMap::from([(decision::DECIDED_BY.to_string(), vec![link("$decision")])]);
        match m {
            RuleMutation::Add(r) => {
                let selectors = match &r.applies_to {
                    Some(list) => json!(list),
                    None => json!([ALL]),
                };
                ops.push(OpInput::Create {
                    block_type: INSTRUCTION.to_string(),
                    id: Some("$rule".to_string()),
                    parent,
                    after: None,
                    before: None,
                    fields: BTreeMap::from([
                        (APPLIES_TO.to_string(), selectors),
                        (LOCKED.to_string(), json!(false)),
                    ]),
                    body: r.text.clone(),
                    links: reason_link,
                });
            }
            RuleMutation::Edit(r) => {
                let cur = current.as_ref().expect("an edit has a rule");
                // the rule's other links stay; its reason is replaced
                let mut links = reason_link;
                for (name, list) in &cur.rule.links {
                    if name != decision::DECIDED_BY {
                        links.insert(
                            name.clone(),
                            list.iter()
                                .map(|l| LinkInput {
                                    target: l.target.to_string(),
                                    part: l.part.clone(),
                                })
                                .collect(),
                        );
                    }
                }
                let mut fields = BTreeMap::new();
                if let Some(list) = &r.applies_to {
                    fields.insert(APPLIES_TO.to_string(), json!(list));
                }
                let here = canon.doc_of(&cur.rule.id).map(|b| b.id.to_string());
                let move_to = parent.filter(|p| Some(p) != here.as_ref()).map(|p| MoveTo {
                    parent: Some(p),
                    after: None,
                    before: None,
                });
                ops.push(OpInput::Update {
                    id: cur.rule.id.to_string(),
                    base: Some(cur.version.clone()),
                    fields,
                    unset: vec![],
                    body: Some(r.text.clone()),
                    links: Some(links),
                    block_type: None,
                    move_to,
                });
            }
            RuleMutation::Remove(_) => {
                let cur = current.as_ref().expect("a remove has a rule");
                ops.push(OpInput::Delete {
                    id: cur.rule.id.to_string(),
                    base: Some(cur.version.clone()),
                });
            }
        }
        // every block the write can touch: a rule is appended, so no sibling takes a new key
        let mut stable: BTreeSet<String> = crate::brief::stable_subjects(&self.pack, &canon, &ops)
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        let mut check = |id: Option<&BlockId>| {
            if let Some(id) = id
                && crate::brief::effective_stable(&canon, Some(id))
            {
                stable.insert(id.to_string());
            }
        };
        if let Some(cur) = &current {
            check(Some(&cur.rule.id));
            check(cur.rule.parent.as_ref());
            check(Some(&cur.reason.id));
        }
        if let Some(Target::Existing(id)) = &target {
            check(Some(id));
        }
        let home = st.decision_parent(view, None)?;
        if let Some(home) = home {
            let id = crate::keys::resolve_text(&canon, &home).ok();
            check(id.as_ref());
        }
        if !stable.is_empty() {
            return Err(stable_refusal(&st, m, stable)?);
        }
        Ok(ops)
    }

    /// The name of the view request `m` works in: `rule-<digest>`. It reads no target.
    pub fn rule_view_name(
        &self,
        c: &RuleCaller<'_>,
        m: RuleMutation<'_>,
    ) -> Result<String, RdyError> {
        let repo = self.read().repo_id()?.to_string();
        Ok(lifecycle::Receipt::of(&repo, c, m)?.view_name())
    }

    /// Takes request `m` as far as its proposal, or finds how far an earlier call took it. The
    /// call runs under the destination's writer. A request whose target or safety checks refuse
    /// writes nothing. A complete change whose proposal cannot be made closes its view and keeps
    /// its history. A repeated request reconciles what its view holds: it never writes the
    /// content again, returns the proposal or the landing it finds, and refuses a view that does
    /// not hold exactly this request (`E_STALE_BASE`) or that closed unlanded (`E_VIEW_CLOSED`).
    pub fn rule_prepare(
        &mut self,
        c: &RuleCaller<'_>,
        m: RuleMutation<'_>,
    ) -> Result<Prepared, RdyError> {
        let s = c.session;
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot write", s.role),
                "start the session as a writer",
            ));
        }
        let st = self.read();
        let repo = st.repo_id()?.to_string();
        let receipt = lifecycle::Receipt::of(&repo, c, m)?;
        let view = receipt.view_name();
        let found = lifecycle::find(&st, &view, s, &receipt, m)?;
        drop(st);
        match found {
            lifecycle::Found::Nothing => self.rule_draft(c, m, &receipt, view),
            lifecycle::Found::Drafted(content) => {
                // an interrupted call left the whole change: it is proposed only if still safe
                if let Err(e) = self.rule_plan(c, m, &view) {
                    return Err(self.rule_close_failed(s, &view, FAILED_BEFORE_PROPOSAL, e));
                }
                self.rule_propose(c, m, view, *content, vec![])
            }
            lifecycle::Found::Proposed { content, proposal } => {
                let (rule, decision) = lifecycle::describe(&self.read(), &view, &content, false)?;
                Ok(Prepared {
                    scope: m.scope(),
                    project: project_of(&self.read())?,
                    rule,
                    decision,
                    view,
                    state: PreparedState::Proposed {
                        reference: proposal.to_string(),
                        version: proposal.version.to_string(),
                    },
                    warnings: vec![],
                })
            }
            lifecycle::Found::Landed { content, landing } => {
                self.rule_landed(c, m, view, &content, landing.to_string())
            }
        }
    }

    /// The first call's durable steps: the checks, the one write, the proposal.
    fn rule_draft(
        &mut self,
        c: &RuleCaller<'_>,
        m: RuleMutation<'_>,
        receipt: &lifecycle::Receipt,
        view: String,
    ) -> Result<Prepared, RdyError> {
        let mut ops = self.rule_plan(c, m, &view)?;
        if let Some(RuleFault::BadOp(i)) = fault()
            && i < ops.len()
        {
            ops[i] = OpInput::Delete {
                id: "ai-00000".to_string(),
                base: None,
            };
        }
        let written = self.write(
            c.session,
            WriteRequest {
                view: view.clone(),
                ops,
                justification: None,
                message: receipt.json().to_string(),
            },
        )?;
        if fault() == Some(RuleFault::StopAfterDraft) {
            return Err(injected("the process"));
        }
        let hash = crate::views::parse_change(&written.change)?;
        let content = self.read().reader().change(&hash)?;
        self.rule_propose(c, m, view, content, written.warnings)
    }

    /// Proposes the complete change `content` of `view`. A failure closes the view.
    fn rule_propose(
        &mut self,
        c: &RuleCaller<'_>,
        m: RuleMutation<'_>,
        view: String,
        content: rdy_model::Change,
        mut warnings: Vec<Issue>,
    ) -> Result<Prepared, RdyError> {
        let s = c.session;
        let (rule, decision) = lifecycle::describe(&self.read(), &view, &content, false)?;
        let title = match m {
            RuleMutation::Add(_) => format!("Add rule {}", rule.id),
            RuleMutation::Edit(_) => format!("Edit rule {}", rule.id),
            RuleMutation::Remove(_) => format!("Retire rule {}", rule.id),
        };
        let summary = format!("{title}: {}", rule.text);
        let proposed = match fault() {
            Some(RuleFault::Propose | RuleFault::ProposeAndClose) => Err(injected("the propose")),
            _ => self.propose(s, &view, &title, Some(&summary)),
        };
        let info = match proposed {
            Ok(info) => info,
            Err(e) => return Err(self.rule_close_failed(s, &view, FAILED_BEFORE_PROPOSAL, e)),
        };
        warnings.extend(info.warnings.iter().cloned());
        Ok(Prepared {
            scope: m.scope(),
            project: project_of(&self.read())?,
            rule,
            decision,
            view,
            state: PreparedState::Proposed {
                reference: info.r#ref,
                version: info.id,
            },
            warnings,
        })
    }

    /// A request whose proposal landed in an earlier call: finishes the cleanup and returns the
    /// landing, unless the rule has moved since, when this request is no longer its state.
    fn rule_landed(
        &mut self,
        c: &RuleCaller<'_>,
        m: RuleMutation<'_>,
        view: String,
        content: &rdy_model::Change,
        landing: String,
    ) -> Result<Prepared, RdyError> {
        let mut warnings = vec![];
        if let Some(w) = self.rule_close_landed(c.session, &view) {
            warnings.push(w);
        }
        let st = self.read();
        let (rule, decision) = lifecycle::describe(&st, &view, content, true)?;
        lifecycle::check_not_moved(&st, content, &rule)?;
        Ok(Prepared {
            scope: m.scope(),
            project: project_of(&st)?,
            rule,
            decision,
            view,
            state: PreparedState::Landed { landing },
            warnings,
        })
    }

    /// Closes the owned view `view` through the normal close, with `reason`.
    fn rule_close(&mut self, s: &Session, view: &str, reason: &str) -> Result<(), RdyError> {
        if matches!(fault(), Some(RuleFault::ProposeAndClose | RuleFault::Close)) {
            return Err(injected("the close"));
        }
        self.view_close(s, &[view.to_string()], reason)?;
        Ok(())
    }

    /// Closes `view` after refusal `e`, which it returns unchanged but for a cleanup it could not
    /// finish, named in `corrected`. The view's history stays.
    pub(crate) fn rule_close_failed(
        &mut self,
        s: &Session,
        view: &str,
        reason: &str,
        mut e: RdyError,
    ) -> RdyError {
        if self.rule_close(s, view, reason).is_err() {
            let mut corrected = match e.corrected.take() {
                Some(Value::Object(o)) => o,
                _ => serde_json::Map::new(),
            };
            corrected.insert("view".into(), json!(view));
            corrected.insert("cleanup".into(), json!("pending"));
            e.corrected = Some(Value::Object(corrected));
        }
        e
    }

    /// Closes `view` after its landing. The landing stands whatever happens here: a close that
    /// cannot write is the warning returned, with the underlying code and the fix.
    pub(crate) fn rule_close_landed(&mut self, s: &Session, view: &str) -> Option<Issue> {
        if self.read().view_head(view).is_none_or(|h| h.closed) {
            return None;
        }
        self.rule_close(s, view, LANDED).err().map(|e| Issue {
            severity: rdy_schema::Severity::Warning,
            code: e.code,
            block: None,
            message: format!("the landed view {view} did not close: {}", e.message),
            fix: format!("close the view with rdy view close {view}"),
        })
    }
}

/// The reason a view closes with when its change could not be proposed.
pub(crate) const FAILED_BEFORE_PROPOSAL: &str = "rules_from_chat: failed before proposal";
/// The reason a view closes with once its change landed.
const LANDED: &str = "rules_from_chat: landed";
