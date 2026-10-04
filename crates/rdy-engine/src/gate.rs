use crate::error::*;
use crate::{rfc3339_now, Materialized, Rdy, Scope, Session};
use rdy_model::{decision, BlockId, Change, FieldValue, Hash, Op, Proposal};
use rdy_schema::{validate_block, Issue, Severity};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize)]
pub struct ProposalInfo {
    pub id: String,
    pub title: String,
    pub changes: usize,
    pub warnings: Vec<Issue>,
}

fn parse_hash(s: &str) -> Result<Hash, RdyError> {
    s.parse().map_err(|_| RdyError::new(E_BAD_INPUT, format!("{s:?} is not a proposal id"), "pass the id returned by rdy_propose"))
}

impl Rdy {
    fn changes_of(&self, ids: &[Hash]) -> Result<Vec<Change>, RdyError> {
        ids.iter().map(|h| self.store.get::<Change>(h).map_err(RdyError::from)).collect()
    }

    /// Validates the canon that would result from landing `changes`.
    fn prospective_issues(&mut self, changes: &[Change]) -> Result<Vec<Issue>, RdyError> {
        // replay onto canon to surface E_STALE_BASE / E_FROZEN early
        let mut snap = self.store.canon_state()?;
        for c in changes {
            for op in &c.ops {
                rdy_store::apply_op(&self.store, &mut snap, op)?;
            }
        }
        let mut next: Materialized = self.materialized(&Scope::Canon)?.clone();
        let mut touched = BTreeSet::new();
        let mut deleted = BTreeSet::new();
        for c in changes {
            next.apply(&c.ops);
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
        let mut issues = vec![];
        for id in &touched {
            let Some(b) = next.get(id) else { continue };
            let parent = b.parent.as_ref().and_then(|p| next.get(p));
            issues.extend(validate_block(&self.pack, b, parent));
        }
        if !deleted.is_empty() {
            for b in next.blocks.values() {
                let refs: BTreeSet<BlockId> = b.ref_tokens().iter().filter_map(|t| BlockId::parse(t).ok()).chain(b.links.values().flatten().map(|l| l.target.clone())).collect();
                for gone in refs.intersection(&deleted) {
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
        Ok(issues)
    }

    pub fn propose(&mut self, s: &Session, view: &str, title: &str) -> Result<ProposalInfo, RdyError> {
        let v = self.store.view(view)?;
        if v.changes.is_empty() {
            return Err(RdyError::new(E_BAD_INPUT, format!("view {view:?} has no changes to propose"), "write to the view first"));
        }
        let changes = self.changes_of(&v.changes)?;
        let issues = self.prospective_issues(&changes)?;
        if let Some(first) = issues.iter().find(|i| i.severity == Severity::Error) {
            let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
            e.issues = issues;
            return Err(e);
        }
        let id = self.store.propose(view, title, s.provenance())?;
        Ok(ProposalInfo { id: id.to_string(), title: title.into(), changes: v.changes.len(), warnings: issues })
    }

    pub fn land(&mut self, s: &Session, proposal: &str) -> Result<String, RdyError> {
        if !s.role.can_land() {
            return Err(RdyError::new(E_PERMISSION, format!("role {:?} cannot land proposals", s.role), "ask an approver to land it"));
        }
        let pid = parse_hash(proposal)?;
        let p: Proposal = self.store.get(&pid)?;
        let changes = self.changes_of(&p.changes)?;

        // 1. prospective canon: canon + the proposal's own changes
        let mut prospective: Materialized = self.materialized(&Scope::Canon)?.clone();
        for c in &changes {
            prospective.apply(&c.ops);
        }
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
            let Some(b) = prospective.get(&id) else { continue };
            if decision::status(b) != Some(decision::DecisionStatus::Proposed) {
                continue;
            }
            // 2. the view must not have moved on from the proposal's version
            let in_view = self.materialized(&view)?.get(&id).map(|v| v.version_hash());
            if in_view != Some(b.version_hash()) {
                stale.push(RdyError::new(E_STALE_BASE, format!("decision {id} changed in view {:?} after the proposal was made", p.view), format!("the view has later edits to {id}; propose again to include them")));
                continue;
            }
            let mut acc = b.clone();
            acc.fields.insert("status".into(), FieldValue::Str("accepted".into()));
            acc.fields.insert("decided_at".into(), FieldValue::Str(now.clone()));
            accept_ops.push(Op::Update { base: b.version_hash(), block: acc });
        }
        if let Some(e) = stale.into_iter().next() {
            return Err(e);
        }

        // 3. dry run: validate the final change list before writing anything
        let accept = (!accept_ops.is_empty()).then(|| Change {
            view: p.view.clone(),
            ops: accept_ops,
            deps: vec![],
            provenance: s.provenance(),
            justification: None,
            message: format!("accept decisions in {}", p.title),
        });
        let mut all = changes;
        all.extend(accept.clone());
        let issues = self.prospective_issues(&all)?;
        if let Some(first) = issues.iter().find(|i| i.severity == Severity::Error) {
            let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
            e.issues = issues;
            return Err(e);
        }

        // 4. write the acceptance change, then land
        let result = (|| -> Result<_, RdyError> {
            let target = match accept {
                None => pid,
                Some(change) => {
                    let mut p = p;
                    let id = self.store.commit_to_view(&p.view, change)?;
                    p.changes.push(id);
                    self.store.put(rdy_store::kinds::PROPOSAL, &p)?
                }
            };
            Ok(self.store.land(&target, s.provenance())?)
        })();
        // the view may have changed even if a later step failed: never leave caches stale
        self.invalidate();
        let landing = result?;
        Ok(landing.to_string())
    }
}
