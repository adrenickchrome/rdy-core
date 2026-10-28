//! cf1 (FW-CF1, D-X4GMH): a conflict on a block that a landing froze, an accepted or rejected
//! decision, is resolved by taking the landed side. The view's block then reads the version the
//! landing froze, which equals canon, so the decision does not change. The op commits one
//! change that writes that version unchanged; the store makes it depend on every side, as any
//! resolving write does (res1).
//! The change briefs the block, in the view, or in canon when the view holds no version of it,
//! and records that briefing (D-ME2HD).
use super::{Action, check_view_op, commit_claim, no_view, strings};
use crate::brief::Coverage;
use crate::error::*;
use crate::write::{check_landed_block, conflict_has_frozen, frozen_in_conflict, landed_side};
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{AltChoice, Change, ChangeLinks, Op};
use serde::Serialize;

/// What `rdy view resolve --landed` did: the conflict it resolved on `block` in `view`, the
/// landed `version` the view now reads, and the change it committed with that change's deps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolveResult {
    pub view: String,
    pub block: String,
    pub conflict: String,
    pub version: String,
    pub change: String,
    pub deps: Vec<String>,
}

impl Rdy {
    /// cf1: resolves the conflict on `block` (an id, key or path) in `view` by taking its landed
    /// side (`landed_side`). Refused when the block has no conflict in the view (`E_BAD_INPUT`),
    /// when nothing in the conflict is frozen (`E_BAD_INPUT`: a write resolves it), and when
    /// something is frozen but no side landed it (`E_FROZEN`, as an edit is refused).
    pub fn view_resolve_landed(
        &mut self,
        s: &Session,
        view: &str,
        block: &str,
    ) -> Result<ResolveResult, RdyError> {
        let st = self.read();
        check_view_op(&st, s, view, Action::Commit)?;
        if !st.has_view(view) {
            return Err(no_view(view));
        }
        let scope = Scope::View(view.to_string());
        let id = st.resolve_id(&scope, block)?;
        let conflicts = st.conflicts(&scope)?;
        let Some(c) = conflicts.iter().find(|c| c.block == id) else {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{id} has no conflict in view {view}"),
                format!("rdy view show {view} lists its conflicts (MCP: rdy_view show)"),
            ));
        };
        let (cid, conflict) = (c.id.short(), c.id.to_string());
        if !conflict_has_frozen(c, &self.store)? {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "conflict {cid} on {id} holds no accepted or rejected decision, so it has no landed side to take"
                ),
                format!(
                    "resolve it with a write of {id} from the version you want: pass base=<its base or a side's version> (rdy_context shows them)"
                ),
            ));
        }
        let included = st.included(view);
        let Some(landed) = landed_side(c, &self.store)? else {
            // frozen, but no side landed it: the refusal an edit gets, with its ways out
            frozen_in_conflict(view, &id, c, &self.store, &included)?;
            return Err(RdyError::new(
                E_INTERNAL,
                format!("conflict {cid} on {id} is frozen but was not refused"),
                "report this as a bug in rdy",
            ));
        };
        let mut links = ChangeLinks::default();
        // alts1: a change written in a member view of an open set records the set
        if let Some((set, _, _)) = st.alt_of_view(view)? {
            links.alternative = Some(AltChoice {
                set,
                member: view.to_string(),
            });
        }
        let claim = commit_claim(&st, s, view)?;
        let m = st.materialized(&scope)?;
        let block = self.store.get_block(&landed)?;
        // a landed side a redaction removed reads as its stub: refused as removed content, before
        // its missing fields are validated
        if let Some(request) = block.redaction() {
            return Err(rdy_store::StoreError::Redacted { request }.into());
        }
        check_landed_block(&self.pack, &m, &block)?;
        // D-ME2HD (O3): the change briefs the block it resolves and records that briefing, as
        // rdy's own writes do (`assign_keys`, the daemon's repin). A conflict with no base (a
        // replayed create of a block canon holds) leaves the view with no version of the block:
        // then canon is briefed, which holds the landed side (`landed_side`), the version the
        // change writes
        let brief_scope = if m.get(&id).is_some() {
            scope.clone()
        } else {
            Scope::Canon
        };
        let briefing = st.brief(
            &brief_scope,
            &Coverage {
                ids: vec![id.to_string()],
                path: None,
            },
            None,
        )?;
        briefing.register(&self.briefs);
        links.briefing = Some(briefing.record.token);
        let ops = vec![Op::Update {
            base: landed,
            block,
        }];
        let change = Change {
            view: view.to_string(),
            ops: ops.clone(),
            deps: vec![],
            provenance: s.provenance(),
            justification: None,
            message: format!("take the landed side of conflict {cid} on {id}"),
            links,
            read_identity: None,
        };
        let conflicts = conflicts.as_ref().clone();
        drop(st);
        let ch = self.store.commit_to_view_resolving(
            view,
            change,
            &|b| m.version_of(b),
            &conflicts,
            claim,
        )?;
        self.publish(Delta::Commit {
            view: view.to_string(),
            change: ch,
            ops,
        })?;
        let deps = self.read().reader().change(&ch)?.deps;
        Ok(ResolveResult {
            view: view.to_string(),
            block: id.to_string(),
            conflict,
            version: landed.to_string(),
            change: ch.to_string(),
            deps: strings(&deps),
        })
    }
}
