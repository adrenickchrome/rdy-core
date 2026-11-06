//! The life of one rule request's view: its receipt, its name, and how a later call finds it.
//!
//! A request's identity and exact words hash, length-prefixed, to the view name `rule-<digest>`,
//! and repeat, as a versioned JSON receipt, in the message of the change. The name is an ordinary
//! view name: it gives no authority. A later call checks the view's owner, its chain of ops, the
//! receipt's bytes and the change's shape before it reuses anything; whatever else the view holds
//! is `E_STALE_BASE`. The lookup reads this view's own chain and its proposal lineage, so its cost
//! does not grow with the project's history. Nothing here rewrites the log.
use super::{RuleCaller, RuleMutation, kept_words_parts, selector_list};
use crate::error::*;
use crate::gate::{latest_version, open_proposal_of_view, ref_hash, version_number};
use crate::rule_types::{
    AddRule, EditRule, RelayWords, RemoveRule, RuleInfo, RuleRequest, RulesetRef,
    Scope as RuleScope,
};
use crate::{ProposalRef, ReadState, Scope, Session};
use rdy_model::{Block, BlockId, Change, FieldValue, Hash, Op, decision};
use rdy_schema::instruction::{APPLIES_TO, INSTRUCTION};
use rdy_store::view::{ViewHead, ViewOpKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;

/// The receipt's version, its first field.
pub const RECEIPT_VERSION: u32 = 1;
/// The most bytes of a receipt's JSON.
pub const MAX_RECEIPT_BYTES: usize = 2_097_152;

/// What a request's digest starts with: names this encoding and its version.
const DIGEST_DOMAIN: &[u8] = b"rdy-rule-request\0v1\0";
/// How the changes the gate adds to a view start their messages: a propose's key change, and a
/// landing's acceptance change (`acceptance_message`).
const GATE_CHANGE_PREFIXES: [&str; 4] = [
    "new keys rdy derived",
    "accept decisions",
    "give new keys",
    "give new order keys",
];

/// A request's identity and words, in the receipt's field order. It never names the project the
/// request came from, and the scope spelling is no part of it: both spellings on meta are one
/// request.
#[derive(Serialize)]
struct Fields<'a> {
    rdy_rule_receipt: u32,
    repo_id: &'a str,
    actor: &'a str,
    run: Option<&'a str>,
    session_ref: &'a str,
    op: &'a str,
    id: &'a str,
    ruleset: &'a str,
    applies_to: Option<&'a [String]>,
    text: &'a str,
    why: &'a str,
    evidence: &'a str,
    question: Option<&'a str>,
}

/// A writer that only counts.
struct Counter(usize);

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn digest_of(f: &Fields<'_>) -> String {
    let mut h = Sha256::new();
    h.update(DIGEST_DOMAIN);
    let text = |h: &mut Sha256, t: &str| {
        h.update((t.len() as u64).to_le_bytes());
        h.update(t.as_bytes());
    };
    for t in [f.repo_id, f.actor] {
        text(&mut h, t);
    }
    h.update([u8::from(f.run.is_some())]);
    text(&mut h, f.run.unwrap_or_default());
    for t in [f.session_ref, f.op, f.id, f.ruleset] {
        text(&mut h, t);
    }
    h.update([u8::from(f.applies_to.is_some())]);
    let list = f.applies_to.unwrap_or_default();
    h.update((list.len() as u64).to_le_bytes());
    for t in list {
        text(&mut h, t);
    }
    for t in [f.text, f.why, f.evidence] {
        text(&mut h, t);
    }
    h.update([u8::from(f.question.is_some())]);
    text(&mut h, f.question.unwrap_or_default());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A request's receipt: the bytes a change's message holds, and the digest that names its view.
pub(super) struct Receipt {
    json: String,
    digest: String,
}

impl Receipt {
    /// The receipt of `m` from `c` in the project `repo_id`. An omitted default ruleset stays an
    /// empty value: the name never depends on what canon holds. The size is measured before the
    /// JSON is allocated.
    pub(super) fn of(
        repo_id: &str,
        c: &RuleCaller<'_>,
        m: RuleMutation<'_>,
    ) -> Result<Receipt, RdyError> {
        let id = m.id().map(ToString::to_string).unwrap_or_default();
        let ruleset = match m.ruleset() {
            Some(RulesetRef::Doc(d)) => d.to_string(),
            Some(RulesetRef::Path(p)) => p.clone(),
            None => String::new(),
        };
        let relay = m.relay();
        let fields = Fields {
            rdy_rule_receipt: RECEIPT_VERSION,
            repo_id,
            actor: &c.session.actor,
            run: c.session.run.as_deref(),
            session_ref: relay.session_ref.as_deref().unwrap_or(c.surface),
            op: m.op(),
            id: &id,
            ruleset: &ruleset,
            applies_to: m.applies_to(),
            text: m.text().unwrap_or_default(),
            why: m.why(),
            evidence: &relay.evidence,
            question: relay.question.as_deref(),
        };
        let encode = |e: serde_json::Error| {
            RdyError::new(
                E_INTERNAL,
                format!("a rule receipt does not encode: {e}"),
                "report this as a bug in rdy",
            )
        };
        let mut size = Counter(0);
        serde_json::to_writer(&mut size, &fields).map_err(encode)?;
        if size.0 > MAX_RECEIPT_BYTES {
            return Err(RdyError::new(
                E_LIMIT,
                format!("rdy_rule: \"receipt\" exceeds {MAX_RECEIPT_BYTES} bytes"),
                format!("use at most {MAX_RECEIPT_BYTES} bytes for \"receipt\""),
            ));
        }
        Ok(Receipt {
            json: serde_json::to_string(&fields).map_err(encode)?,
            digest: digest_of(&fields),
        })
    }

    pub(super) fn json(&self) -> &str {
        &self.json
    }

    pub(super) fn view_name(&self) -> String {
        format!("rule-{}", self.digest)
    }
}

/// What a request's view holds.
pub(super) enum Found {
    /// No view has the name.
    Nothing,
    /// The whole change, with no proposal: a call stopped between the steps.
    Drafted(Box<Change>),
    /// The whole change, and its open proposal.
    Proposed {
        content: Box<Change>,
        proposal: ProposalRef,
    },
    /// The whole change, landed.
    Landed { content: Box<Change>, landing: Hash },
}

pub(super) fn changed(view: &str) -> RdyError {
    RdyError::new(
        E_STALE_BASE,
        format!("rule view {view} changed outside this request"),
        "read the view and continue through the normal workflow",
    )
}

/// Every commit of `view`'s chain, oldest first, or `None` when the chain holds an op that this
/// flow never makes. A rebase is a landing's, and a close this flow's or a person's.
fn commits(st: &ReadState, head: &ViewHead) -> Result<Option<Vec<Hash>>, RdyError> {
    let mut out = vec![];
    let mut cur = Some(head.head);
    while let Some(h) = cur {
        let op = st.reader().view_op(&h)?;
        match op.op {
            ViewOpKind::Commit { change } => out.push(change),
            ViewOpKind::Create { .. } | ViewOpKind::Rebase { .. } | ViewOpKind::Close { .. } => {}
            _ => return Ok(None),
        }
        cur = op.prev;
    }
    out.reverse();
    Ok(Some(out))
}

fn is_gate_change(c: &Change) -> bool {
    GATE_CHANGE_PREFIXES
        .iter()
        .any(|p| c.message.starts_with(p))
        && c.ops.iter().all(|o| matches!(o, Op::Update { .. }))
}

/// Whether `written` is the statement request `m` writes in `content`. An edit that keeps the
/// words reads that from the version its update replaces, and its selectors from its update. Its
/// doc was read from canon at the write and a later move can change it, so only its form is read.
fn statement_fits(st: &ReadState, content: &Change, m: RuleMutation<'_>, written: &str) -> bool {
    let RuleMutation::Edit(r) = m else {
        return written == m.statement();
    };
    let update = content.ops.iter().find_map(|o| match o {
        Op::Update { block, base } => Some((block, base)),
        _ => None,
    });
    let Some((rule, base)) = update else {
        return false;
    };
    let Ok(old) = st.reader().get_block(base) else {
        return false;
    };
    if old.body != r.text {
        return written == m.statement();
    }
    let (head, tail) = kept_words_parts(&r.text, &selector_list(rule), &r.why);
    written
        .strip_prefix(head.as_str())
        .and_then(|rest| rest.strip_suffix(tail.as_str()))
        .is_some_and(|doc| doc.is_empty() || BlockId::parse(doc).is_ok())
}

/// Whether `content`'s ops are exactly what request `m` writes.
fn holds_request(st: &ReadState, content: &Change, m: RuleMutation<'_>) -> bool {
    let (mut reason, mut rule) = (None, None);
    for op in &content.ops {
        match op {
            Op::Create { block } if block.block_type == decision::DECISION => {
                let fits = block
                    .field_str("statement")
                    .is_some_and(|written| statement_fits(st, content, m, written));
                if reason.is_some() || !fits {
                    return false;
                }
                reason = Some(&block.id);
            }
            Op::Create { block } if block.block_type == INSTRUCTION => {
                let RuleMutation::Add(r) = m else {
                    return false;
                };
                if rule.is_some() || block.body != r.text {
                    return false;
                }
                rule = Some(block);
            }
            Op::Create { block } if block.block_type == "section" || block.block_type == "doc" => {
                // only a first use writes a doc and its skeleton
                if !matches!(m, RuleMutation::Add(_)) || m.ruleset().is_some() {
                    return false;
                }
            }
            Op::Update { block, .. } => {
                let RuleMutation::Edit(r) = m else {
                    return false;
                };
                if rule.is_some() || block.id != r.id || block.body != r.text {
                    return false;
                }
                rule = Some(block);
            }
            Op::Delete { id, .. } => {
                let RuleMutation::Remove(r) = m else {
                    return false;
                };
                if *id != r.id {
                    return false;
                }
                return reason.is_some();
            }
            Op::Create { .. } => return false,
        }
    }
    let (Some(reason), Some(rule)) = (reason, rule) else {
        return false;
    };
    rule.links
        .get(decision::DECIDED_BY)
        .is_some_and(|l| matches!(l.as_slice(), [one] if one.target == *reason))
}

/// What a change's message holds, read back.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFields {
    rdy_rule_receipt: u32,
    repo_id: String,
    actor: String,
    run: Option<String>,
    session_ref: String,
    op: String,
    id: String,
    ruleset: String,
    applies_to: Option<Vec<String>>,
    text: String,
    why: String,
    evidence: String,
    question: Option<String>,
}

impl StoredFields {
    fn fields(&self) -> Fields<'_> {
        Fields {
            rdy_rule_receipt: self.rdy_rule_receipt,
            repo_id: &self.repo_id,
            actor: &self.actor,
            run: self.run.as_deref(),
            session_ref: &self.session_ref,
            op: &self.op,
            id: &self.id,
            ruleset: &self.ruleset,
            applies_to: self.applies_to.as_deref(),
            text: &self.text,
            why: &self.why,
            evidence: &self.evidence,
            question: self.question.as_deref(),
        }
    }

    /// The request that wrote this receipt. Its scope spelling is no part of the receipt, and no
    /// shape check reads it.
    fn request(&self) -> Option<RuleRequest> {
        let relay = RelayWords {
            evidence: self.evidence.clone(),
            question: self.question.clone(),
            session_ref: Some(self.session_ref.clone()),
        };
        let scope = RuleScope::Project;
        let ruleset = (!self.ruleset.is_empty()).then(|| match BlockId::parse(&self.ruleset) {
            Ok(id) => RulesetRef::Doc(id),
            Err(_) => RulesetRef::Path(self.ruleset.clone()),
        });
        Some(match self.op.as_str() {
            "add" => RuleRequest::Add(AddRule {
                scope,
                text: self.text.clone(),
                why: self.why.clone(),
                ruleset,
                applies_to: self.applies_to.clone(),
                relay,
            }),
            "edit" => RuleRequest::Edit(EditRule {
                scope,
                id: BlockId::parse(&self.id).ok()?,
                text: self.text.clone(),
                why: self.why.clone(),
                ruleset,
                applies_to: self.applies_to.clone(),
                relay,
            }),
            "remove" => RuleRequest::Remove(RemoveRule {
                scope,
                id: BlockId::parse(&self.id).ok()?,
                why: self.why.clone(),
                relay,
            }),
            _ => return None,
        })
    }
}

/// A rule request as its view holds it, every check passed: what an answer resolves its item to.
pub(super) struct StoredRule {
    pub(super) view: String,
    /// The complete change, the first commit of the view.
    pub(super) first: Hash,
    pub(super) content: Change,
    /// The chat session the request came from.
    pub(super) session_ref: String,
}

/// The request whose view is `view`, read back from what the view holds: its name is its receipt's
/// digest, its owner the receipt's actor and run, and its change exactly what that receipt asks
/// for, with only the gate's changes after it. `None` for any other view: it is no rule request.
pub(super) fn stored_rule(st: &ReadState, view: &str) -> Result<Option<StoredRule>, RdyError> {
    let Some(digest) = view.strip_prefix("rule-") else {
        return Ok(None);
    };
    let Some(head) = st.view_head(view) else {
        return Ok(None);
    };
    let Some(chain) = commits(st, head)? else {
        return Ok(None);
    };
    let Some((first, rest)) = chain.split_first() else {
        return Ok(None);
    };
    let content = st.reader().change(first)?;
    let Ok(stored) = serde_json::from_str::<StoredFields>(&content.message) else {
        return Ok(None);
    };
    let fields = stored.fields();
    let repo = st.repo_id()?.to_string();
    let owned = head
        .owner
        .as_ref()
        .is_some_and(|o| o.actor == stored.actor && o.run == stored.run);
    if !owned
        || stored.rdy_rule_receipt != RECEIPT_VERSION
        || stored.repo_id != repo
        || digest_of(&fields) != digest
        || serde_json::to_string(&fields).ok().as_deref() != Some(content.message.as_str())
    {
        return Ok(None);
    }
    let Some(request) = stored.request() else {
        return Ok(None);
    };
    let Some(m) = RuleMutation::of(&request) else {
        return Ok(None);
    };
    if !holds_request(st, &content, m) {
        return Ok(None);
    }
    for h in rest {
        if !is_gate_change(&st.reader().change(h)?) {
            return Ok(None);
        }
    }
    Ok(Some(StoredRule {
        view: view.to_string(),
        first: *first,
        content,
        session_ref: stored.session_ref,
    }))
}

/// The rule request whose complete change writes the decision `id`: a read of each rule view's
/// first change, which an answer on a decision card needs and nothing on the normal path does.
pub(super) fn rule_of_decision(st: &ReadState, id: &str) -> Result<Option<StoredRule>, RdyError> {
    for view in st.view_names() {
        if !view.starts_with("rule-") {
            continue;
        }
        let Some(rule) = stored_rule(st, &view)? else {
            continue;
        };
        let writes = rule.content.ops.iter().any(|o| {
            matches!(o, Op::Create { block } if block.block_type == decision::DECISION && block.id.as_str() == id)
        });
        if writes {
            return Ok(Some(rule));
        }
    }
    Ok(None)
}

/// The decision that `content` writes, and the rule's earlier reason it supersedes or revokes:
/// `None` for an add.
pub(super) fn replaced(content: &Change) -> Option<(BlockId, BlockId, &'static str)> {
    content.ops.iter().find_map(|o| match o {
        Op::Create { block } if block.block_type == decision::DECISION => [
            (decision::SUPERSEDES, "supersedes"),
            (decision::REVOKES, "revokes"),
        ]
        .into_iter()
        .find_map(|(name, word)| {
            let old = block.links.get(name)?.first()?;
            Some((block.id.clone(), old.target.clone(), word))
        }),
        _ => None,
    })
}

/// The rule that `content` edits or retires.
pub(super) fn target_rule(content: &Change) -> Option<&BlockId> {
    content.ops.iter().find_map(|o| match o {
        Op::Update { block, .. } if block.block_type == INSTRUCTION => Some(&block.id),
        Op::Delete { id, .. } => Some(id),
        _ => None,
    })
}

/// How far the view of the request with `receipt` got, checked before anything is reused. It
/// comes before any target check, so a repeated remove finds its landing though its rule is gone.
pub(super) fn find(
    st: &ReadState,
    view: &str,
    s: &Session,
    receipt: &Receipt,
    m: RuleMutation<'_>,
) -> Result<Found, RdyError> {
    let Some(head) = st.view_head(view) else {
        return Ok(Found::Nothing);
    };
    let owned = head
        .owner
        .as_ref()
        .is_some_and(|o| o.actor == s.actor && o.run == s.run);
    if !owned {
        return Err(changed(view));
    }
    let Some(chain) = commits(st, head)? else {
        return Err(changed(view));
    };
    let Some((first, rest)) = chain.split_first() else {
        return Err(changed(view));
    };
    let content = st.reader().change(first)?;
    if content.message != receipt.json() || !holds_request(st, &content, m) {
        return Err(changed(view));
    }
    for h in rest {
        if !is_gate_change(&st.reader().change(h)?) {
            return Err(changed(view));
        }
    }
    if let Some(landing) = st.reader().landing_of(first)? {
        return Ok(Found::Landed {
            content: Box::new(content),
            landing,
        });
    }
    if head.closed {
        return Err(rdy_store::StoreError::ViewClosed(view.to_string()).into());
    }
    if !st.included(view).contains(first) {
        return Err(changed(view));
    }
    if open_proposal_of_view(st, view)?.is_none() {
        return Ok(Found::Drafted(Box::new(content)));
    }
    let open = st
        .reader()
        .get_ref(&format!("open/{view}"))?
        .ok_or_else(|| changed(view))?;
    let stable = ref_hash(&open, "an open ref")?;
    let version = latest_version(st, &stable)?;
    Ok(Found::Proposed {
        content: Box::new(content),
        proposal: ProposalRef {
            version,
            stable,
            n: version_number(st, &version)?,
        },
    })
}

/// The rule and the decision that `content` writes, as a reply names them: a retired rule keeps
/// its id and its prior text, read from the version the delete names.
pub(super) fn describe(
    st: &ReadState,
    view: &str,
    content: &Change,
    landed: bool,
) -> Result<(RuleInfo, String), RdyError> {
    let bad = || changed(view);
    let mut reason = None;
    let mut rule: Option<(Block, bool)> = None;
    for op in &content.ops {
        match op {
            Op::Create { block } if block.block_type == decision::DECISION => {
                reason = Some(block.id.to_string());
            }
            Op::Create { block } | Op::Update { block, .. } if block.block_type == INSTRUCTION => {
                rule = Some((block.clone(), false));
            }
            Op::Delete { base, .. } => {
                rule = Some((st.reader().get_block(base)?, true));
            }
            _ => {}
        }
    }
    let (Some(reason), Some((rule, retired))) = (reason, rule) else {
        return Err(bad());
    };
    let scope = if landed {
        Scope::Canon
    } else {
        Scope::View(view.to_string())
    };
    let mat = st.materialized(&scope)?;
    let ruleset = match rule.parent.as_ref() {
        Some(p) => mat
            .doc_of(p)
            .map_or_else(|| p.to_string(), |d| d.id.to_string()),
        None => String::new(),
    };
    Ok((
        RuleInfo {
            id: rule.id.to_string(),
            text: rule.body.clone(),
            ruleset,
            retired,
        },
        reason,
    ))
}

fn selectors_of(b: &Block) -> Option<&FieldValue> {
    b.fields.get(APPLIES_TO)
}

/// The refusal of a landed request whose rule moved after it landed: repeating the request would
/// report a state the rule is no longer in.
pub(super) fn check_not_moved(
    st: &ReadState,
    content: &Change,
    rule: &RuleInfo,
) -> Result<(), RdyError> {
    let id = BlockId::parse(&rule.id)
        .map_err(|e| RdyError::new(E_INTERNAL, e.to_string(), "report this as a bug in rdy"))?;
    let canon = st.materialized(&Scope::Canon)?;
    let now = canon.get(&id);
    let same = match content.ops.iter().find(|o| *o.block_id() == id) {
        Some(Op::Create { block }) | Some(Op::Update { block, .. }) => now.is_some_and(|b| {
            b.body == block.body
                && b.parent == block.parent
                && selectors_of(b) == selectors_of(block)
                && b.links.get(decision::DECIDED_BY) == block.links.get(decision::DECIDED_BY)
        }),
        _ => now.is_none(),
    };
    if same {
        return Ok(());
    }
    Err(RdyError::new(
        E_STALE_BASE,
        format!("rule {id} changed after this request landed"),
        "pass a new session_ref to make this request again",
    ))
}
