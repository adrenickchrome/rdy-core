use crate::error::*;
use crate::{Delta, Materialized, Rdy, ReadState, Scope, Session, rfc3339_now};
use rdy_model::{Block, BlockId, Change, ChangeLinks, FieldValue, Hash, Op, Proposal, decision};
use rdy_schema::{Issue, Severity, validate_block};
use rdy_store::LandRequest;
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize)]
pub struct ProposalInfo {
    pub id: String,
    pub title: String,
    pub changes: usize,
    pub warnings: Vec<Issue>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LandInfo {
    pub landing: String,
    pub warnings: Vec<Issue>,
}

fn parse_hash(s: &str) -> Result<Hash, RdyError> {
    s.parse().map_err(|_| {
        RdyError::new(
            E_BAD_INPUT,
            format!("{s:?} is not a proposal id"),
            "pass the id returned by rdy_propose",
        )
    })
}

/// Every block id `b` points at: inline references and link targets.
fn targets(b: &Block) -> BTreeSet<BlockId> {
    b.ref_tokens()
        .iter()
        .filter_map(|t| BlockId::parse(t).ok())
        .chain(b.links.values().flatten().map(|l| l.target.clone()))
        .collect()
}

fn changes_of(st: &ReadState, ids: &[Hash]) -> Result<Vec<Change>, RdyError> {
    ids.iter()
        .map(|h| st.reader().change(h).map_err(RdyError::from))
        .collect()
}

/// The landing that already carried any of `p`'s changes into canon: one index lookup per
/// change (lidx1).
fn landed_as(st: &ReadState, p: &Proposal) -> Result<Option<Hash>, RdyError> {
    for c in &p.changes {
        if let Some(l) = st.reader().landing_of(c)? {
            return Ok(Some(l));
        }
    }
    Ok(None)
}

/// The ops of the changes at `applied` (positions in `changes`), in applied order.
fn applied_ops(changes: &[Change], applied: &[usize]) -> Vec<Op> {
    applied
        .iter()
        .flat_map(|&i| changes[i].ops.iter().cloned())
        .collect()
}

impl Rdy {
    /// Validates the canon that would result from landing `changes` on `st`'s canon; returns the
    /// issues and each applied change's position in `changes`, in applied order.
    fn prospective_issues(
        &self,
        st: &ReadState,
        changes: &[Change],
    ) -> Result<(Vec<Issue>, Vec<usize>), RdyError> {
        // merge onto the state's canon to surface E_STALE_BASE / E_ALREADY_EXISTS / E_FROZEN
        // early
        let reader = st.reader();
        let (_, applied) = reader.merge_checked(&reader.state_of(st.canon_head())?, changes)?;
        let next: Materialized = st
            .materialized(&Scope::Canon)?
            .with(&applied_ops(changes, &applied));
        let mut touched = BTreeSet::new();
        let mut deleted = BTreeSet::new();
        for c in applied.iter().map(|&i| &changes[i]) {
            for op in &c.ops {
                match op {
                    Op::Delete { id, .. } => {
                        deleted.insert(id.clone());
                        touched.remove(id);
                    }
                    other => {
                        touched.insert(other.block_id().clone());
                    }
                }
            }
        }
        // an id deleted and then re-created in this proposal exists in the prospective canon:
        // it is not deleted
        deleted.retain(|id| next.get(id).is_none());
        let mut issues = vec![];
        // two views can each give a different block the same key: index next's keys once, and
        // only when a touched block has a key
        let keys = touched
            .iter()
            .any(|id| next.get(id).and_then(crate::keys::key_of).is_some())
            .then(|| crate::keys::Keys::of(next.iter()));
        for id in &touched {
            let Some(found) = next.get(id) else { continue };
            if let (Some(keys), Some(k)) = (&keys, crate::keys::key_of(found)) {
                for (_, other) in keys.holders(k) {
                    // a pair whose holders are both touched is reported once, lower id first
                    if other == id || (other < id && touched.contains(other)) {
                        continue;
                    }
                    let fix = if touched.contains(other) {
                        format!("change the key of {id} or {other} in this view")
                    } else {
                        format!("change the key of {id} in this view")
                    };
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: E_KEY_CLASH,
                        block: Some(id.clone()),
                        message: format!("{id} and {other} would both have key {k:?} in canon"),
                        fix,
                    });
                }
            }
            // a doc from M1 has no stored name; readers give it one, so the gate doesn't require it
            let named;
            let b =
                if found.block_type == rdy_model::tree::DOC && !found.fields.contains_key("name") {
                    let mut c = found.clone();
                    let n = rdy_model::tree::default_name(c.field_str("title"), &c.id);
                    c.fields.insert("name".into(), FieldValue::Str(n));
                    named = c;
                    &named
                } else {
                    found
                };
            let parent = b.parent.as_ref().and_then(|p| next.get(p));
            issues.extend(validate_block(st.pack(), b, parent));
            // two views can each be fine alone and clash or loop once both are in canon
            let mut walk = b.parent.clone();
            for _ in 0..=next.len() {
                let Some(p) = walk else { break };
                if &p == id {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_PARENT_CYCLE",
                        block: Some(id.clone()),
                        message: format!(
                            "{id} would be inside itself: its parents loop back to it"
                        ),
                        fix: format!(
                            "move {id} (or one of its parents) to a place outside its own subtree"
                        ),
                    });
                    break;
                }
                walk = next.get(&p).and_then(|x| x.parent.clone());
            }
            if rdy_model::tree::is_tree_type(&b.block_type) {
                let mut depth = next.folder_depth(id);
                if b.block_type == rdy_model::tree::FOLDER {
                    depth += next.folder_height(id);
                }
                if depth > rdy_model::tree::MAX_FOLDER_DEPTH {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_TOO_DEEP",
                        block: Some(id.clone()),
                        message: format!(
                            "{id} would put folders {depth} deep (at most {} allowed)",
                            rdy_model::tree::MAX_FOLDER_DEPTH
                        ),
                        fix: format!(
                            "keep folders at most {} deep: move {id} higher in this view",
                            rdy_model::tree::MAX_FOLDER_DEPTH
                        ),
                    });
                }
            }
            // the derived names under each parent are cached in `next`, so this is a lookup
            if rdy_model::tree::is_tree_type(&b.block_type)
                && let Some(n) = next.name_of(id)
            {
                for other in next.named(b.parent.as_ref(), &n.to_ascii_lowercase()) {
                    if &other != id {
                        issues.push(Issue {
                            severity: Severity::Error,
                            code: "E_PATH_CLASH",
                            block: Some(id.clone()),
                            message: format!("{id} and {other} would both be named {n:?} in the same folder (ignoring case)"),
                            fix: format!("rename {id} in this view"),
                        });
                    }
                }
            }
            // canon may have moved since the view was written: every target must still exist
            for target in targets(b) {
                if next.get(&target).is_none() && !deleted.contains(&target) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_DANGLING_REF",
                        block: Some(b.id.clone()),
                        message: format!("{} references {target}, which does not exist in canon", b.id),
                        fix: format!("remove the reference to {target} from {}, or restore {target} in the same view", b.id),
                    });
                }
            }
        }
        if !deleted.is_empty() {
            for b in next.iter() {
                if let Some(parent) = b.parent.as_ref().filter(|p| deleted.contains(*p)) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_MISSING_PARENT",
                        block: Some(b.id.clone()),
                        message: format!(
                            "{} has parent {parent}, which this proposal deletes",
                            b.id
                        ),
                        fix: format!(
                            "move or delete {} in the same view, or don't delete {parent}",
                            b.id
                        ),
                    });
                }
                for gone in targets(b).intersection(&deleted) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_DANGLING_REF",
                        block: Some(b.id.clone()),
                        message: format!("{} still references {gone}, which this proposal deletes", b.id),
                        fix: format!("remove the reference to {gone} from {} in the same view, or don't delete it", b.id),
                    });
                }
            }
        }
        Ok((issues, applied))
    }

    /// Warns when the proposal places blocks (creates or moves) under a parent whose siblings, as
    /// the view's base saw them, have different order keys in canon now: any landed re-key of a
    /// sibling since then counts, whether a rebalance or an ordinary move of one block. The
    /// placed blocks' keys were chosen against the old keys.
    fn rekeyed_warnings(
        &self,
        st: &ReadState,
        view_base: Option<Hash>,
        changes: &[Change],
    ) -> Result<Vec<Issue>, RdyError> {
        let mut parents: BTreeSet<Option<BlockId>> = BTreeSet::new();
        for c in changes {
            for op in &c.ops {
                match op {
                    Op::Create { block } => {
                        parents.insert(block.parent.clone());
                    }
                    Op::Update { base, block } => {
                        let old = st.reader().get_block(base)?;
                        if old.parent != block.parent || old.order != block.order {
                            parents.insert(block.parent.clone());
                        }
                    }
                    Op::Delete { .. } => {}
                }
            }
        }
        if parents.is_empty() {
            return Ok(vec![]);
        }
        let base = match view_base {
            Some(l) => st.reader().snapshot_at(&l)?,
            None => rdy_model::StateSnapshot::default(),
        };
        // canon's children that the base also had, at a different version: (parent, id, base, order)
        let canon = st.materialized(&Scope::Canon)?;
        let mut changed = vec![];
        for p in &parents {
            for sib in canon.children(p.as_ref()) {
                if let Some(h) = base.blocks.get(&sib.id)
                    && *h != sib.version_hash()
                {
                    changed.push((p.clone(), sib.id.clone(), *h, sib.order.clone()));
                }
            }
        }
        let mut out = vec![];
        for p in parents {
            let mut rekeyed = None;
            for (_, id, h, order) in changed.iter().filter(|c| c.0 == p) {
                if st.reader().get_block(h)?.order != *order {
                    rekeyed = Some(id.clone());
                    break;
                }
            }
            if let Some(sib) = rekeyed {
                let at = p
                    .as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "the top level".into());
                out.push(Issue {
                    severity: Severity::Warning,
                    code: "W_SIBLINGS_REKEYED",
                    block: p.clone(),
                    message: format!(
                        "the children of {at} were re-keyed in canon since this view was based on it (e.g. {sib}): check the order of the blocks this proposal places there"
                    ),
                    fix: format!(
                        "after landing, read the children of {at} and move any block this proposal placed out of order"
                    ),
                });
            }
        }
        Ok(out)
    }

    pub fn propose(
        &mut self,
        s: &Session,
        view: &str,
        title: &str,
    ) -> Result<ProposalInfo, RdyError> {
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot propose", s.role),
                "start the session as a writer",
            ));
        }
        let st = self.read();
        let included = st.included(view);
        if included.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("view {view:?} has no changes to propose"),
                "write to the view first",
            ));
        }
        let changes = changes_of(&st, &included)?;
        let (mut issues, _) = self.prospective_issues(&st, &changes)?;
        if let Some(first) = issues.iter().find(|i| i.severity == Severity::Error) {
            let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
            e.issues = issues;
            return Err(e);
        }
        // a view with no head floats on canon
        let view_base = st.view_head(view).map_or(st.canon_head(), |h| h.base);
        issues.extend(self.rekeyed_warnings(&st, view_base, &changes)?);
        let id = self.store.propose(view, title, s.provenance())?;
        self.publish(Delta::Seq)?;
        Ok(ProposalInfo {
            id: id.to_string(),
            title: title.into(),
            changes: included.len(),
            warnings: issues,
        })
    }

    /// Lands a proposal; returns the landing id.
    pub fn land(&mut self, s: &Session, proposal: &str) -> Result<String, RdyError> {
        self.land_report(s, proposal).map(|l| l.landing)
    }

    /// Lands a proposal; returns the landing id and the gate's warnings.
    pub fn land_report(&mut self, s: &Session, proposal: &str) -> Result<LandInfo, RdyError> {
        if !s.role.can_land() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot land proposals", s.role),
                "ask an approver to land it",
            ));
        }
        let pid = parse_hash(proposal)?;
        if !self.store.has_kind(rdy_store::kinds::PROPOSAL, &pid)? {
            if self.store.has(&pid)? {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("{proposal} is not a proposal"),
                    "pass the id returned by rdy_propose",
                ));
            }
            return Err(rdy_store::StoreError::NotFound(pid).into());
        }
        let st = self.read();
        let p = st.reader().proposal(&pid)?;
        // land1: a landed change is already in canon; replaying it would only fail obscurely
        if let Some(landing) = landed_as(&st, &p)? {
            return Err(RdyError::new(
                E_ALREADY_LANDED,
                format!(
                    "proposal {} already landed as landing {landing}",
                    pid.short()
                ),
                format!(
                    "nothing to do: see it in rdy log; to land later work from view {:?}, propose the view again",
                    p.view
                ),
            ));
        }
        let changes = changes_of(&st, &p.changes)?;

        // 1. prospective canon: canon + the proposal's own changes
        let ops: Vec<Op> = changes.iter().flat_map(|c| c.ops.iter().cloned()).collect();
        let prospective = st.materialized(&Scope::Canon)?.with(&ops);
        let mut ids = BTreeSet::new();
        for c in &changes {
            for op in &c.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op
                    && block.block_type == decision::DECISION
                {
                    ids.insert(block.id.clone());
                }
            }
        }
        let now = rfc3339_now();
        let mut accept_ops = vec![];
        let mut stale = vec![];
        let view = Scope::View(p.view.clone());
        for id in ids {
            let Some(b) = prospective.get(&id) else {
                continue;
            };
            if decision::status(b) != Some(decision::DecisionStatus::Proposed) {
                continue;
            }
            // 2. the view must not have moved on from the proposal's version
            let in_view = st.materialized(&view)?.get(&id).map(|v| v.version_hash());
            if in_view != Some(b.version_hash()) {
                stale.push(RdyError::new(
                    E_STALE_BASE,
                    format!(
                        "decision {id} changed in view {:?} after the proposal was made",
                        p.view
                    ),
                    format!("the view has later edits to {id}; propose again to include them"),
                ));
                continue;
            }
            let mut acc = b.clone();
            acc.fields
                .insert("status".into(), FieldValue::Str("accepted".into()));
            acc.fields
                .insert("decided_at".into(), FieldValue::Str(now.clone()));
            accept_ops.push(Op::Update {
                base: b.version_hash(),
                block: acc,
            });
        }
        if let Some(e) = stale.into_iter().next() {
            return Err(e);
        }

        // 3. dry run: validate the final change list before writing anything. The acceptance
        // carries the deps the store gives it when it commits it to the view (the view's own
        // producers of its bases), so the merge orders it after the changes it accepts.
        let accept = if accept_ops.is_empty() {
            None
        } else {
            let mut change = Change {
                view: p.view.clone(),
                ops: accept_ops,
                deps: vec![],
                provenance: s.provenance(),
                justification: None,
                message: format!("accept decisions in {}", p.title),
                links: ChangeLinks::default(),
            };
            change.deps = self.store.change_deps(&change, Some(&p.view))?;
            Some(change)
        };
        let mut all = changes;
        all.extend(accept.clone());
        let (mut issues, order) = self.prospective_issues(&st, &all)?;
        if let Some(first) = issues.iter().find(|i| i.severity == Severity::Error) {
            let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
            e.issues = issues;
            return Err(e);
        }
        // a view with no head floats on canon
        let view_base = st.view_head(&p.view).map_or(st.canon_head(), |h| h.base);
        issues.extend(self.rekeyed_warnings(&st, view_base, &all)?);
        let landed_ops = applied_ops(&all, &order);

        // 4. land in one write (atom1): the acceptance change, the final proposal version, the
        // snapshot, the landing, the view's rebase and the refs. A failed landing writes nothing,
        // so the published state stays as it is.
        let out = self.store.land_atomic(LandRequest {
            proposal: pid,
            accept,
            approver: s.provenance(),
            chosen: None,
            relay: None,
            evidence: None,
        })?;
        // 5. publish canon incrementally: the previous canon with the landed ops (snap1)
        if let Err(e) = self.publish(Delta::Landed {
            landing: out.landing,
            view: p.view.clone(),
            ops: landed_ops,
            rebased: out.rebased.iter().map(|r| r.view.clone()).collect(),
        }) {
            // the landing is written: never leave the published state behind it
            let _ = self.publish(Delta::Full);
            return Err(e);
        }
        Ok(LandInfo {
            landing: out.landing.to_string(),
            warnings: issues,
        })
    }
}
