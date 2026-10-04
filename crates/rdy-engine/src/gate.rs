use crate::error::*;
use crate::write::{OpInput, WriteRequest};
use crate::{rfc3339_now, Materialized, Rdy, Scope, Session};
use rdy_model::{decision, BlockId, Change, Hash, Op, Proposal};
use rdy_schema::{validate_block, Issue, Severity};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

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
        let mut p: Proposal = self.store.get(&pid)?;
        let changes = self.changes_of(&p.changes)?;

        // decisions created or updated by this proposal that are still proposed in the view
        let view = Scope::View(p.view.clone());
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
        let mut accept_ops = vec![];
        let now = rfc3339_now();
        for id in ids {
            if let Some(b) = self.materialized(&view)?.get(&id)
                && decision::status(b) == Some(decision::DecisionStatus::Proposed)
            {
                let mut fields = BTreeMap::new();
                fields.insert("status".to_string(), json!("accepted"));
                fields.insert("decided_at".to_string(), json!(now));
                accept_ops.push(OpInput::Update { id: id.to_string(), base: None, fields, unset: vec![], body: None, links: None });
            }
        }
        let target = if accept_ops.is_empty() {
            pid
        } else {
            let res = self.write(s, WriteRequest { view: p.view.clone(), ops: accept_ops, justification: None, message: format!("accept decisions in {}", p.title) })?;
            p.changes.push(parse_hash(&res.change)?);
            self.store.put(rdy_store::kinds::PROPOSAL, &p)?
        };
        let landing = self.store.land(&target, s.provenance())?;
        self.invalidate();
        Ok(landing.to_string())
    }
}
