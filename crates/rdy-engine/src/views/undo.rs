//! Undo and close (undo1): `rdy undo` appends an `Undo` with the ops its inverse needs, and
//! `rdy view close` closes views.
use super::{
    Action, ConflictInfo, check_view_op, conflict_infos, dependents, dependents_refusal,
    missing_deps_rooted, no_view, strings,
};
use crate::error::*;
use crate::read_state::ReadState;
use crate::version::answer_write_error;
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{Answer, Hash};
use rdy_store::view::{ResolvedOp, ViewOp, ViewOpKind};
use rdy_store::{StoreError, StoreReader};
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

/// What the landed rule says of an undo's effect.
enum Landed {
    /// The undo may go ahead; for a re-include, the missing deps it adds (step 5 writes them).
    Clear(Vec<Hash>),
    Refused(RdyError),
}

/// The changes a re-include of `cs` puts back: those `included` lacks, each once, in order.
fn put_back_of(cs: &[Hash], included: &BTreeSet<Hash>) -> Vec<Hash> {
    let mut seen = BTreeSet::new();
    cs.iter()
        .filter(|c| !included.contains(c) && seen.insert(**c))
        .copied()
        .collect()
}

/// The landed rule: an undo never brings back or removes landed work. `Refused` when `effect`
/// would re-include a landed change or a landed dep its re-include adds, or would exclude
/// changes that all landed and none of which the view still includes. The deps come from one
/// closure over the whole put-back set, so canon's landing chain is walked at most once.
fn landed_refusal(
    st: &ReadState,
    view: &str,
    short: &str,
    effect: &ResolvedOp,
    included: &BTreeSet<Hash>,
) -> Result<Landed, RdyError> {
    let reader = st.reader();
    match effect {
        ResolvedOp::Include(cs) => {
            for c in cs {
                if let Some(l) = reader.landing_of(c)? {
                    return Ok(Landed::Refused(undo_landing(format!(
                        "op {short} would re-include {}, which landed in {}",
                        c.short(),
                        l.short()
                    ))));
                }
            }
            // the deps the re-include adds (the closure step 5 writes), each with the
            // put-back change whose closure reached it
            let put_back = put_back_of(cs, included);
            let rooted = missing_deps_rooted(st, st.effective_base(view), included, &put_back)?;
            for (d, c) in &rooted {
                if let Some(l) = reader.landing_of(d)? {
                    return Ok(Landed::Refused(RdyError::new(
                        E_UNDO_LANDING,
                        format!(
                            "op {short} would re-include {}, a dep of {}, which landed in {}",
                            d.short(),
                            c.short(),
                            l.short()
                        ),
                        format!(
                            "include the change directly instead: rdy view include {view} {c} (MCP: rdy_view include); an include adds the landed dep"
                        ),
                    )));
                }
            }
            Ok(Landed::Clear(rooted.into_iter().map(|(d, _)| d).collect()))
        }
        ResolvedOp::Exclude(cs) if !cs.is_empty() => {
            if cs.iter().any(|c| included.contains(c)) {
                return Ok(Landed::Clear(vec![]));
            }
            let mut first = None;
            for c in cs {
                match reader.landing_of(c)? {
                    Some(l) => {
                        first.get_or_insert(l);
                    }
                    None => return Ok(Landed::Clear(vec![])),
                }
            }
            Ok(match first {
                Some(l) => Landed::Refused(undo_landing(format!(
                    "op {short}'s change(s) landed in {}",
                    l.short()
                ))),
                None => Landed::Clear(vec![]),
            })
        }
        _ => Ok(Landed::Clear(vec![])),
    }
}

/// Whether `text` could name an op: 7 to 64 hex characters.
fn is_op_prefix(text: &str) -> bool {
    (7..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// mfb3: the ops one write wrote in a row with `chain[i]` (newest first), when it is a `Commit`
/// or an `Undo`: the commits of one `commit_many` (an import's changes, one per author) and the
/// undos of one undo of them. An undo takes them as one: the members not yet undone (`chain[i]`
/// first, then newest first) and their joint effect, oldest change first. Any other op, an op
/// written alone, or members whose effects do not join is `chain[i]` alone with its own effect.
fn write_group(
    reader: &StoreReader,
    view: &str,
    chain: &[(Hash, ViewOp)],
    i: usize,
    undone: &HashMap<Hash, Hash>,
) -> Result<(Vec<Hash>, ResolvedOp), RdyError> {
    let target = chain[i].0;
    let alone = || -> Result<(Vec<Hash>, ResolvedOp), RdyError> {
        Ok((vec![target], reader.undo_effect(view, &target)?))
    };
    let kind = |k: &ViewOpKind| match k {
        ViewOpKind::Commit { .. } => Some(true),
        ViewOpKind::Undo { .. } => Some(false),
        _ => None,
    };
    let Some(mine) = kind(&chain[i].1.op) else {
        return alone();
    };
    let (mut newest, mut oldest) = (i, i);
    while newest > 0
        && kind(&chain[newest - 1].1.op) == Some(mine)
        && reader.view_ops_in_a_row(&chain[newest].0, &chain[newest - 1].0)?
    {
        newest -= 1;
    }
    while oldest + 1 < chain.len()
        && kind(&chain[oldest + 1].1.op) == Some(mine)
        && reader.view_ops_in_a_row(&chain[oldest + 1].0, &chain[oldest].0)?
    {
        oldest += 1;
    }
    if newest == oldest {
        return alone();
    }
    let mut members = vec![target];
    members.extend(
        chain[newest..=oldest]
            .iter()
            .map(|(h, _)| *h)
            .filter(|h| *h != target && !undone.contains_key(h)),
    );
    let (mut excluded, mut included) = (vec![], vec![]);
    for (h, _) in chain[newest..=oldest].iter().rev() {
        if !members.contains(h) {
            continue;
        }
        match reader.undo_effect(view, h)? {
            ResolvedOp::Exclude(cs) => excluded.extend(cs),
            ResolvedOp::Include(cs) => included.extend(cs),
            _ => return alone(),
        }
    }
    let effect = match (excluded.is_empty(), included.is_empty()) {
        (_, true) => ResolvedOp::Exclude(excluded),
        (true, false) => ResolvedOp::Include(included),
        (false, false) => return alone(),
    };
    Ok((members, effect))
}

impl Rdy {
    /// undo1: undoes op `target` of `view`'s chain (by id or unique prefix), or without a target
    /// the session's newest undoable op not yet undone, by appending an `Undo` with the ops its
    /// inverse needs: the excluded changes' dependents (`cascade`, else `E_UNDO_BLOCKED`), a
    /// re-include's missing deps, or a rebase onto canon when it reopens a view. A rebase, and
    /// an undo that would bring back or remove landed work, is `E_UNDO_LANDING`. The commits one
    /// write wrote in a row (an import's changes, one per author) are undone as one, and so are
    /// the undos of one such undo (`write_group`): one `Undo` per member, the target's first.
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
        let (members, target_op, effect, deps) = match target {
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
                    .enumerate()
                    .filter(|(_, (h, _))| h.to_string().starts_with(&prefix));
                let (at, h, op) = match (found.next(), found.next()) {
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
                    (Some((at, (h, op))), None) => (at, *h, op.clone()),
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
                let (members, effect) = write_group(reader, view, &chain, at, &undone)?;
                let deps = match landed_refusal(&st, view, &h.short(), &effect, &included)? {
                    Landed::Refused(e) => return Err(e),
                    Landed::Clear(deps) => deps,
                };
                // a closed view takes no undo but the one of its close
                if head.closed && effect != ResolvedOp::Reopen {
                    return Err(StoreError::ViewClosed(view.into()).into());
                }
                (members, op, effect, deps)
            }
            None => {
                let mut pick = None;
                for (i, (h, op)) in chain.iter().enumerate() {
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
                    let (members, effect) = write_group(reader, view, &chain, i, &undone)?;
                    if head.closed && effect != ResolvedOp::Reopen {
                        continue;
                    }
                    if let Landed::Clear(deps) =
                        landed_refusal(&st, view, &h.short(), &effect, &included)?
                    {
                        pick = Some((members, op.clone(), effect, deps));
                        break;
                    }
                }
                if pick.is_none() && head.closed {
                    return Err(StoreError::ViewClosed(view.into()).into());
                }
                pick.ok_or_else(|| {
                    let by = crate::identity::actor_run_text(&s.actor, s.run.as_deref());
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
        // one undo per member of the target's write group, the target's first
        let target = members[0];
        let mut ops: Vec<ViewOpKind> = members
            .iter()
            .map(|t| ViewOpKind::Undo { target: *t })
            .collect();
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
                // the deps `landed_refusal` found for this put-back set
                put_back = put_back_of(cs, &included);
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
                    let merged = reader.merge_onto(Some(ch), &reader.state_of(Some(ch))?, &rest)?;
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
        let reason = self.closable(s, views, reason)?;
        // one write and one publish per view
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

    /// `view_close` for a person's relayed act (O14, D-DGCH7): the same rules, run as `s` (the
    /// person), and every close and the answer `a` in one write. The answer's `targets` are the
    /// close ops. Returns the closed views and the answer's id.
    pub(crate) fn view_close_answered(
        &mut self,
        s: &Session,
        views: &[String],
        reason: &str,
        a: Answer,
    ) -> Result<(Vec<CloseResult>, Hash), RdyError> {
        let reason = self.closable(s, views, reason)?;
        let appends: Vec<(String, Vec<ViewOpKind>)> = views
            .iter()
            .map(|v| {
                (
                    v.clone(),
                    vec![ViewOpKind::Close {
                        reason: reason.to_string(),
                    }],
                )
            })
            .collect();
        let (_, answer) = self
            .store
            .view_append_answered(&appends, &s.provenance(), a)
            .map_err(answer_write_error)?;
        self.publish(Delta::Coord {
            closed: views.to_vec(),
        })?;
        Ok((
            views
                .iter()
                .map(|v| CloseResult {
                    view: v.clone(),
                    reason: reason.to_string(),
                })
                .collect(),
            answer,
        ))
    }

    /// The rules of a close, for every name, before any write: at least one name, the session
    /// may close each view, and each view is open (a name given twice is closed once the first
    /// is, so it is already closed). Returns the reason to record.
    fn closable<'a>(
        &self,
        s: &Session,
        views: &[String],
        reason: &'a str,
    ) -> Result<&'a str, RdyError> {
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
        Ok(reason)
    }
}
