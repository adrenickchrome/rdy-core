//! Undo and close (undo1): `rdy undo` appends an `Undo` with the ops its inverse needs, and
//! `rdy view close` closes views.
use super::{
    Action, ConflictInfo, check_view_op, conflict_infos, dependents, dependents_refusal,
    missing_deps, no_view, strings,
};
use crate::error::*;
use crate::read_state::ReadState;
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::Hash;
use rdy_store::view::{ResolvedOp, ViewOp, ViewOpKind};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};

/// What `rdy undo` did: the `Undo` op it wrote, the op it undid, and that op's inverse on the
/// view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UndoResult {
    pub view: String,
    /// The new `Undo` op.
    pub op: String,
    /// The op it undid.
    pub undone: String,
    /// The undone op's kind (`commit`, `include`, `exclude`, `close`, `own`, `create`, `fork`,
    /// `undo`).
    pub kind: String,
    pub effect: String,
    /// The changes the undo put back in the view, with the deps they need.
    pub included: Vec<String>,
    /// The changes the undo took out of the view.
    pub excluded: Vec<String>,
    /// The included changes that depended on them, excluded too (`cascade`).
    pub cascaded: Vec<String>,
    pub closed: bool,
    pub reopened: bool,
    /// Whether the reopened view was rebased onto canon's head.
    pub rebased: bool,
    pub conflicts: Vec<ConflictInfo>,
}

/// A view `rdy view close` closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CloseResult {
    pub view: String,
    pub reason: String,
}

/// An op kind in snake case, as undo and `view show` report it.
pub(crate) fn kind_name(k: &ViewOpKind) -> &'static str {
    match k {
        ViewOpKind::Create { .. } => "create",
        ViewOpKind::Fork { .. } => "fork",
        ViewOpKind::Commit { .. } => "commit",
        ViewOpKind::Include { .. } => "include",
        ViewOpKind::Exclude { .. } => "exclude",
        ViewOpKind::Rebase { .. } => "rebase",
        ViewOpKind::Undo { .. } => "undo",
        ViewOpKind::Close { .. } => "close",
        ViewOpKind::Own { .. } => "own",
    }
}

/// The net undone set of a view's op chain (newest first): each undone op mapped to the live
/// undo of it. An op is undone when its newest undo is live, that is, not itself undone. Newest
/// first, every undo of an undo is decided before the undo it targets, so after a redo the
/// redone op is live again. One rule for `undo` and `view show`.
pub(crate) fn net_undone(chain: &[(Hash, ViewOp)]) -> HashMap<Hash, Hash> {
    let mut undone: HashMap<Hash, Hash> = HashMap::new();
    let mut decided: HashSet<Hash> = HashSet::new();
    for (h, op) in chain {
        if let ViewOpKind::Undo { target } = op.op
            && decided.insert(target)
            && !undone.contains_key(h)
        {
            undone.insert(target, *h);
        }
    }
    undone
}

fn undo_landing(message: String) -> RdyError {
    RdyError::new(
        E_UNDO_LANDING,
        message,
        "a landing is undone by a revoking decision (rdy_revoke)",
    )
}

/// The landed rule: an undo never brings back or removes landed work. `Some` refusal when
/// `effect` would re-include a landed change, or would exclude changes that all landed and none
/// of which the view still includes.
fn landed_refusal(
    st: &ReadState,
    short: &str,
    effect: &ResolvedOp,
    included: &BTreeSet<Hash>,
) -> Result<Option<RdyError>, RdyError> {
    let reader = st.reader();
    match effect {
        ResolvedOp::Include(cs) => {
            for c in cs {
                if let Some(l) = reader.landing_of(c)? {
                    return Ok(Some(undo_landing(format!(
                        "op {short} would re-include {}, which landed in {}",
                        c.short(),
                        l.short()
                    ))));
                }
            }
            Ok(None)
        }
        ResolvedOp::Exclude(cs) if !cs.is_empty() => {
            if cs.iter().any(|c| included.contains(c)) {
                return Ok(None);
            }
            let mut first = None;
            for c in cs {
                match reader.landing_of(c)? {
                    Some(l) => {
                        first.get_or_insert(l);
                    }
                    None => return Ok(None),
                }
            }
            Ok(first
                .map(|l| undo_landing(format!("op {short}'s change(s) landed in {}", l.short()))))
        }
        _ => Ok(None),
    }
}

/// Whether `text` could name an op: 7 to 64 hex characters.
fn is_op_prefix(text: &str) -> bool {
    (7..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Rdy {
    /// undo1: undoes op `target` of `view`'s chain (by id or unique prefix), or without a target
    /// the session's newest undoable op not yet undone, by appending an `Undo` with the ops its
    /// inverse needs: the excluded changes' dependents (`cascade`, else `E_UNDO_BLOCKED`), a
    /// re-include's missing deps, or a rebase onto canon when it reopens a view. A rebase, and
    /// an undo that would bring back or remove landed work, is `E_UNDO_LANDING`.
    pub fn undo(
        &mut self,
        s: &Session,
        view: &str,
        target: Option<&str>,
        cascade: bool,
    ) -> Result<UndoResult, RdyError> {
        // 1. the rules
        let st = self.read();
        check_view_op(&st, s, view, Action::Undo)?;
        let head = st.view_head(view).ok_or_else(|| no_view(view))?.clone();
        let reader = st.reader();
        // 2. the chain, newest first; each undone op with the (newest) undo of it
        let mut chain: Vec<(Hash, ViewOp)> = vec![];
        let mut cur = Some(head.head);
        while let Some(h) = cur {
            let op = reader.view_op(&h)?;
            cur = op.prev;
            chain.push((h, op));
        }
        // The net undone set: see `net_undone`.
        let undone = net_undone(&chain);
        let included_list = st.included(view);
        let included: BTreeSet<Hash> = included_list.iter().copied().collect();
        // 3 and 4. the target, its effect, and the rules on it
        let (target, target_op, effect) = match target {
            Some(text) => {
                if !is_op_prefix(text) {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("{:?} is not an op id", super::clip(text)),
                        format!("rdy view show {view} lists its ops"),
                    ));
                }
                let prefix = text.to_ascii_lowercase();
                let mut found = chain
                    .iter()
                    .filter(|(h, _)| h.to_string().starts_with(&prefix));
                let (h, op) = match (found.next(), found.next()) {
                    (None, _) => {
                        return Err(RdyError::new(
                            E_NOT_FOUND,
                            format!("no op {text} on view {view:?}'s chain"),
                            format!("rdy view show {view} lists its ops"),
                        ));
                    }
                    (Some(_), Some(_)) => {
                        return Err(RdyError::new(
                            E_BAD_INPUT,
                            format!("{text} is ambiguous on view {view:?}'s chain"),
                            "use more characters",
                        ));
                    }
                    (Some((h, op)), None) => (*h, op.clone()),
                };
                if let ViewOpKind::Rebase { landing, .. } = &op.op {
                    return Err(undo_landing(format!(
                        "op {} is a rebase from landing {}",
                        h.short(),
                        landing.short()
                    )));
                }
                if let Some(u) = undone.get(&h) {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("op {} is already undone by {}", h.short(), u.short()),
                        format!(
                            "to redo it, undo that undo: rdy undo --view {view} {}",
                            u.short()
                        ),
                    ));
                }
                let effect = reader.undo_effect(view, &h)?;
                if let Some(e) = landed_refusal(&st, &h.short(), &effect, &included)? {
                    return Err(e);
                }
                (h, op, effect)
            }
            None => {
                let mut pick = None;
                for (h, op) in &chain {
                    if op.actor != s.actor
                        || op.run != s.run
                        || !matches!(
                            op.op,
                            ViewOpKind::Commit { .. }
                                | ViewOpKind::Include { .. }
                                | ViewOpKind::Exclude { .. }
                                | ViewOpKind::Close { .. }
                                | ViewOpKind::Own { .. }
                        )
                        || undone.contains_key(h)
                    {
                        continue;
                    }
                    let effect = reader.undo_effect(view, h)?;
                    if landed_refusal(&st, &h.short(), &effect, &included)?.is_none() {
                        pick = Some((*h, op.clone(), effect));
                        break;
                    }
                }
                pick.ok_or_else(|| {
                    let by = match &s.run {
                        Some(run) => format!("{}/{run}", s.actor),
                        None => s.actor.clone(),
                    };
                    RdyError::new(
                        E_NOTHING_TO_UNDO,
                        format!("no undoable op by {by} on view {view:?}"),
                        format!(
                            "name an op: rdy undo --view {view} <op> (rdy view show lists them)"
                        ),
                    )
                })?
            }
        };
        // 5. the ops: the undo, then what its inverse needs
        let mut ops = vec![ViewOpKind::Undo { target }];
        let mut put_back: Vec<Hash> = vec![];
        let mut taken_out: Vec<Hash> = vec![];
        let mut cascaded: Vec<Hash> = vec![];
        let mut rebased = false;
        match &effect {
            ResolvedOp::Exclude(cs) => {
                taken_out = cs
                    .iter()
                    .filter(|c| included.contains(c))
                    .copied()
                    .collect();
                let gone: BTreeSet<Hash> = taken_out.iter().copied().collect();
                let deps = dependents(&st, &included_list, &gone)?;
                if !deps.is_empty() && !cascade {
                    return Err(dependents_refusal(
                        &st,
                        &gone,
                        &deps,
                        E_UNDO_BLOCKED,
                        "what this undo would exclude",
                        "undo with --cascade (MCP: cascade: true) to exclude them too",
                        |d| format!("undo with --cascade to exclude {} too", d.short()),
                    )?);
                }
                if !deps.is_empty() {
                    ops.push(ViewOpKind::Exclude {
                        changes: deps.clone(),
                    });
                    cascaded = deps;
                }
            }
            ResolvedOp::Include(cs) => {
                let mut seen = BTreeSet::new();
                put_back = cs
                    .iter()
                    .filter(|c| !included.contains(c) && seen.insert(**c))
                    .copied()
                    .collect();
                let deps = missing_deps(&st, st.effective_base(view), &included, &put_back)?;
                if !deps.is_empty() {
                    ops.push(ViewOpKind::Include {
                        changes: deps.clone(),
                    });
                    put_back.extend(deps);
                }
            }
            ResolvedOp::Reopen => {
                if !head.pinned
                    && !included_list.is_empty()
                    && let Some(ch) = st.canon_head()
                    && Some(ch) != head.base
                {
                    let mut dropped = vec![];
                    let mut remaining = vec![];
                    for c in included_list.iter() {
                        if reader.landing_of(c)?.is_some() {
                            dropped.push(*c);
                        } else {
                            remaining.push(*c);
                        }
                    }
                    let rest = remaining
                        .iter()
                        .map(|c| reader.change(c))
                        .collect::<Result<Vec<_>, _>>()?;
                    let merged = reader.merge_onto(&reader.state_of(Some(ch))?, &rest)?;
                    ops.push(ViewOpKind::Rebase {
                        landing: ch,
                        dropped,
                        remaining,
                        conflicts: merged.conflicts,
                    });
                    rebased = true;
                }
            }
            ResolvedOp::Close | ResolvedOp::SetOwner(_) | ResolvedOp::Nothing => {}
        }
        let kind = kind_name(&target_op.op);
        let effect_text = match (&target_op.op, &effect) {
            (ViewOpKind::Undo { target: t }, _) => {
                format!("redid {}", kind_name(&reader.view_op(t)?.op))
            }
            (_, ResolvedOp::Exclude(_)) if !taken_out.is_empty() => {
                format!("excluded {} change(s)", taken_out.len() + cascaded.len())
            }
            (_, ResolvedOp::Include(_)) if !put_back.is_empty() => {
                format!("included {} change(s)", put_back.len())
            }
            (_, ResolvedOp::Close) => "closed".to_string(),
            (_, ResolvedOp::Reopen) => "reopened".to_string(),
            (_, ResolvedOp::SetOwner(_)) => "owner restored".to_string(),
            _ => "no change".to_string(),
        };
        // 6. the write, the publish, the result
        let after_undo = ops.len() - 1;
        drop(st);
        let new_head = self.store.view_append(view, ops, &s.provenance())?;
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        let st2 = self.read();
        let mut op = new_head.head;
        for _ in 0..after_undo {
            op = st2.reader().view_op(&op)?.prev.ok_or_else(|| {
                RdyError::new(
                    E_INTERNAL,
                    format!("view {view:?}: the undo's ops end without a prev"),
                    "run rdy fsck",
                )
            })?;
        }
        let conflicts = conflict_infos(&st2, &st2.conflicts(&Scope::View(view.to_string()))?)?;
        Ok(UndoResult {
            view: view.to_string(),
            op: op.to_string(),
            undone: target.to_string(),
            kind: kind.to_string(),
            effect: effect_text,
            included: strings(&put_back),
            excluded: strings(&taken_out),
            cascaded: strings(&cascaded),
            closed: matches!(effect, ResolvedOp::Close),
            reopened: matches!(effect, ResolvedOp::Reopen),
            rebased,
            conflicts,
        })
    }

    /// Closes `views`, each with `reason` ("closed by hand" when empty). Every name is checked
    /// before any write; each view is then closed in its own write, so a store failure on one
    /// leaves the views before it closed, and the error names the view it failed on.
    pub fn view_close(
        &mut self,
        s: &Session,
        views: &[String],
        reason: &str,
    ) -> Result<Vec<CloseResult>, RdyError> {
        if views.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "name at least one view to close",
                "rdy view close <view>...",
            ));
        }
        let reason = if reason.trim().is_empty() {
            "closed by hand"
        } else {
            reason
        };
        // 1. the rules, for every name, before any write (a name given twice is closed once
        //    the first is, so it is already closed)
        let st = self.read();
        let mut seen = BTreeSet::new();
        for v in views {
            check_view_op(&st, s, v, Action::Close)?;
            let h = st.view_head(v).ok_or_else(|| no_view(v))?;
            if h.closed || !seen.insert(v.as_str()) {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("view {v:?} is already closed"),
                    "nothing to do; reopen it with rdy undo if that was the intent",
                ));
            }
        }
        drop(st);
        // 2. one write and one publish per view
        let mut out = vec![];
        for v in views {
            self.store
                .view_append(
                    v,
                    vec![ViewOpKind::Close {
                        reason: reason.to_string(),
                    }],
                    &s.provenance(),
                )
                .map_err(|e| {
                    let mut e = RdyError::from(e);
                    e.message = format!("closing view {v:?}: {}", e.message);
                    e
                })?;
            self.publish(Delta::View { view: v.clone() })?;
            out.push(CloseResult {
                view: v.clone(),
                reason: reason.to_string(),
            });
        }
        Ok(out)
    }
}
