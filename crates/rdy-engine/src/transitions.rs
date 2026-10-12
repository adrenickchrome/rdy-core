//! The moves of the open tension set and of the suspect set between two states (plan M2e2, founder decision C): what each batch logs. A move is logged once, by the batch that made it.
//!
//! Founder decision C is D-Z0EMP (refines D-KTW43; with D-TPZ0W and D-V4NZX). The moves are
//! computed on the writer, from the states it publishes and the frames of its own open batch.
//! Index reads on other threads can see an open batch's frames before its seal (a failed seal
//! or a kill removes them), so no reader off the writer thread derives a move, a refusal or a
//! notification from them: the moves come only from here, inside the batch that made them.
use crate::tensions::set::TensionSet;
use crate::tensions::{SideRef, Tension, TensionSeverity, TensionStatus, tension_id};
use crate::{RdyError, ReadState, Scope, events};
use imbl::ordmap::DiffItem;
use rdy_graph::Suspect;
use rdy_model::Hash;
use rdy_store::transition::{
    Author, Cause, Side, SuspectMove, TensionMove, Transitions, Why, suspect_key,
};
use rdy_store::view::ViewOpKind;
use rdy_store::{StoreReader, kinds};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Whether a batch of `frames` can move the open tensions or the suspects (the closed list):
/// every frame kind except `BLOB_CHUNK`, `BLOB_MANIFEST`, `TAG`, `INTENT`, and a `REF_UPDATE`
/// whose name starts with `inbox/` or `intent/`. A ref update that cannot be read counts as one
/// that moves.
pub(crate) fn can_move(reader: &StoreReader, frames: &[(u8, Hash)]) -> bool {
    frames.iter().any(|(kind, h)| match *kind {
        kinds::BLOB_CHUNK | kinds::BLOB_MANIFEST | kinds::TAG | kinds::INTENT => false,
        kinds::REF_UPDATE => reader
            .get_typed::<kinds::RefUpdate>(kinds::REF_UPDATE, h)
            .map_or(true, |r| {
                !(r.name.starts_with("inbox/") || r.name.starts_with("intent/"))
            }),
        _ => true,
    })
}

/// The moves between `base` and `next`. `cause`: `None` for a write batch of `frames` (each
/// move takes the batch's cause, by the closed rule), or the cause every move takes (a pack
/// reload, a reopen, a recheck).
pub(crate) fn moves(
    base: &ReadState,
    next: &ReadState,
    frames: &[(u8, Hash)],
    cause: Option<Cause>,
) -> Result<Transitions, RdyError> {
    let reader = next.reader();
    let bset = base.tensions()?;
    let nset = next.tensions()?;
    let (resolved, opened) = tension_moves(base, &bset, next, &nset);
    let (gone, new) = suspect_moves(base, next)?;
    if resolved.is_empty() && opened.is_empty() && gone.is_empty() && new.is_empty() {
        return Ok(empty());
    }
    let (rekey_to, rekey_from) = rekeys(&resolved, &opened);
    let batch = match cause {
        Some(c) => why(c, None),
        None => batch_why(reader, frames)?,
    };
    let mut rec = Transitions {
        at_ms: 0,
        resolved: vec![],
        opened: vec![],
        gone: vec![],
        new: vec![],
    };
    for t in &resolved {
        let w = match (cause, nset.get(&t.id).map(|t| next.tension_status(t))) {
            // a write batch's tension that stays in the set and becomes dismissed
            (None, Some(TensionStatus::Dismissed(review))) => {
                let p = reader.review(&review)?.provenance;
                Why {
                    cause: Cause::Dismissed,
                    id: Some(review),
                    actor: Some(p.actor),
                    run: p.run,
                }
            }
            _ => batch.clone(),
        };
        rec.resolved.push(TensionMove {
            tension: t.id,
            kind: t.kind.name().to_string(),
            severity: t.severity.to_string(),
            sides: sides(base, &bset, t)?,
            rekey: rekey_to.get(&t.id).copied(),
            why: w,
        });
    }
    for t in &opened {
        rec.opened.push(TensionMove {
            tension: t.id,
            kind: t.kind.name().to_string(),
            severity: t.severity.to_string(),
            sides: sides(next, &nset, t)?,
            rekey: rekey_from.get(&t.id).copied(),
            why: batch.clone(),
        });
    }
    for s in gone.values() {
        rec.gone.push(suspect_move(base, &bset, s, batch.clone())?);
    }
    for s in new.values() {
        rec.new.push(suspect_move(next, &nset, s, batch.clone())?);
    }
    rec.at_ms = at_ms(reader, next, frames);
    Ok(rec)
}

/// The moves between the open sets the log folds to (the store's open-move tables) and `next`:
/// what a batch logs when nothing was logged since open. A move that `moves(prev, next, …)`
/// also holds takes its cause (the batch's, or `cause` for a batch that moves nothing) and its
/// re-key; every other move takes `cause`. A resolution read from the tables takes its kind,
/// severity, sides and writers from the record that opened it.
pub(crate) fn reconcile_moves(
    next: &ReadState,
    prev: Option<&ReadState>,
    frames: &[(u8, Hash)],
    cause: Cause,
) -> Result<Transitions, RdyError> {
    let reader = next.reader();
    let table_t: BTreeMap<Hash, Hash> = reader.open_tensions()?.into_iter().collect();
    let table_s: BTreeMap<Hash, Hash> = reader.open_suspects()?.into_iter().collect();
    let nset = next.tensions()?;
    let next_open: BTreeMap<Hash, &Arc<Tension>> = nset
        .iter()
        .filter(|t| is_open(next, t))
        .map(|t| (t.id, t))
        .collect();
    let next_sus = canon_suspects(next)?;
    let opening: Vec<Hash> = next_open
        .keys()
        .filter(|id| !table_t.contains_key(id))
        .copied()
        .collect();
    let resolving: Vec<(Hash, Hash)> = table_t
        .iter()
        .filter(|(id, _)| !next_open.contains_key(id))
        .map(|(id, rec)| (*id, *rec))
        .collect();
    let new_keys: Vec<Hash> = next_sus
        .keys()
        .filter(|k| !table_s.contains_key(k))
        .copied()
        .collect();
    let gone_keys: Vec<(Hash, Hash)> = table_s
        .iter()
        .filter(|(k, _)| !next_sus.contains_key(k))
        .map(|(k, rec)| (*k, *rec))
        .collect();
    if opening.is_empty() && resolving.is_empty() && new_keys.is_empty() && gone_keys.is_empty() {
        return Ok(empty());
    }
    // the moves the batch itself made, with their causes and re-keys
    let made = match prev {
        Some(p) => {
            let batch = if can_move(reader, frames) {
                None
            } else {
                Some(cause)
            };
            Some(moves(p, next, frames, batch)?)
        }
        None => None,
    };
    let made_t = |list: fn(&Transitions) -> &Vec<TensionMove>, id: &Hash| {
        made.as_ref()
            .and_then(|m| list(m).iter().find(|t| t.tension == *id).cloned())
    };
    let made_s = |list: fn(&Transitions) -> &Vec<SuspectMove>, key: &Hash| {
        made.as_ref()
            .and_then(|m| list(m).iter().find(|s| s.key() == *key).cloned())
    };
    let other = why(cause, None);
    let opening_set: BTreeSet<Hash> = opening.iter().copied().collect();
    let resolving_set: BTreeSet<Hash> = resolving.iter().map(|(id, _)| *id).collect();
    let mut records: BTreeMap<Hash, Transitions> = BTreeMap::new();
    let mut record = |h: Hash| -> Result<Transitions, RdyError> {
        if let Some(r) = records.get(&h) {
            return Ok(r.clone());
        }
        let r = reader.transitions(&h)?;
        records.insert(h, r.clone());
        Ok(r)
    };
    let mut rec = empty();
    for (id, opened_by) in &resolving {
        let opened = record(*opened_by)?
            .opened
            .into_iter()
            .find(|t| t.tension == *id)
            .ok_or_else(|| unknown_move(*opened_by, &format!("tension {id}")))?;
        let (why, rekey) = match made_t(|m| &m.resolved, id) {
            Some(m) => (m.why, m.rekey.filter(|r| opening_set.contains(r))),
            None => (other.clone(), None),
        };
        rec.resolved.push(TensionMove {
            rekey,
            why,
            ..opened
        });
    }
    for id in &opening {
        let t = next_open[id];
        let (why, rekey) = match made_t(|m| &m.opened, id) {
            Some(m) => (m.why, m.rekey.filter(|r| resolving_set.contains(r))),
            None => (other.clone(), None),
        };
        rec.opened.push(TensionMove {
            tension: t.id,
            kind: t.kind.name().to_string(),
            severity: t.severity.to_string(),
            sides: sides(next, &nset, t)?,
            rekey,
            why,
        });
    }
    for (key, made_by) in &gone_keys {
        let new = record(*made_by)?
            .new
            .into_iter()
            .find(|s| s.key() == *key)
            .ok_or_else(|| unknown_move(*made_by, &format!("suspect {key}")))?;
        let why = made_s(|m| &m.gone, key).map_or_else(|| other.clone(), |m| m.why);
        rec.gone.push(SuspectMove { why, ..new });
    }
    for key in &new_keys {
        let why = made_s(|m| &m.new, key).map_or_else(|| other.clone(), |m| m.why);
        rec.new
            .push(suspect_move(next, &nset, &next_sus[key], why)?);
    }
    rec.at_ms = at_ms(reader, next, frames);
    Ok(rec)
}

/// The number of moves in `rec`.
pub(crate) fn count(rec: &Transitions) -> usize {
    rec.resolved.len() + rec.opened.len() + rec.gone.len() + rec.new.len()
}

fn empty() -> Transitions {
    Transitions {
        at_ms: 0,
        resolved: vec![],
        opened: vec![],
        gone: vec![],
        new: vec![],
    }
}

/// A record in the open-move tables that does not hold the move the tables say it made.
fn unknown_move(record: Hash, what: &str) -> RdyError {
    rdy_store::StoreError::Decode(format!(
        "transition record {record} does not hold the move of {what} that the index names; run rdy reindex"
    ))
    .into()
}

fn why(cause: Cause, id: Option<Hash>) -> Why {
    Why {
        cause,
        id,
        actor: None,
        run: None,
    }
}

/// Whether `t` is in the open tension set of `st`: event-derived and not dismissed.
fn is_open(st: &ReadState, t: &Tension) -> bool {
    !t.read_time && st.tension_status(t) == TensionStatus::Open
}

/// The tensions that `next` opened and the ones it resolved against `base`, each by id: the
/// candidates of `TensionSet::diff`, plus every id in both sets whose tension differs (a pack
/// reload that changes a type's tracking fields keeps the id and moves the vkey, so a
/// dismissal bound to the old vkey lapses), plus every tension whose vkey's dismissal differs.
fn tension_moves(
    base: &ReadState,
    bset: &TensionSet,
    next: &ReadState,
    nset: &TensionSet,
) -> (Vec<Arc<Tension>>, Vec<Arc<Tension>>) {
    let mut ids: BTreeSet<Hash> = bset
        .changed(nset)
        .into_iter()
        .filter_map(|(b, n)| b.or(n).map(|t| t.id))
        .collect();
    if !Arc::ptr_eq(&base.dismissals, &next.dismissals) {
        let vkeys: BTreeSet<Hash> = base
            .dismissals
            .diff(&next.dismissals)
            .map(|item| match item {
                DiffItem::Add(k, _) | DiffItem::Remove(k, _) => *k,
                DiffItem::Update { new: (k, _), .. } => *k,
            })
            .collect();
        if !vkeys.is_empty() {
            ids.extend(
                bset.iter()
                    .chain(nset.iter())
                    .filter(|t| vkeys.contains(&t.vkey))
                    .map(|t| t.id),
            );
        }
    }
    let mut out_resolved = vec![];
    let mut out_opened = vec![];
    for id in ids {
        let b = bset.get(&id).filter(|t| is_open(base, t));
        let n = nset.get(&id).filter(|t| is_open(next, t));
        match (b, n) {
            (Some(t), None) => out_resolved.push(Arc::clone(t)),
            (None, Some(t)) => out_opened.push(Arc::clone(t)),
            _ => {}
        }
    }
    (out_resolved, out_opened)
}

/// The re-keys among `resolved` and `opened` (both by id): first, per vkey, the resolved and
/// the opened tensions paired in id order; then a resolved tension and an opened one with the
/// same items under the other severity (an acceptance). Returns (resolved -> opened,
/// opened -> resolved).
fn rekeys(
    resolved: &[Arc<Tension>],
    opened: &[Arc<Tension>],
) -> (BTreeMap<Hash, Hash>, BTreeMap<Hash, Hash>) {
    let mut by_vkey: BTreeMap<Hash, Vec<Hash>> = BTreeMap::new();
    for t in opened {
        by_vkey.entry(t.vkey).or_default().push(t.id);
    }
    let mut to: BTreeMap<Hash, Hash> = BTreeMap::new();
    let mut from: BTreeMap<Hash, Hash> = BTreeMap::new();
    let mut taken: BTreeMap<Hash, usize> = BTreeMap::new();
    for t in resolved {
        let n = taken.entry(t.vkey).or_default();
        if let Some(o) = by_vkey.get(&t.vkey).and_then(|ids| ids.get(*n)) {
            to.insert(t.id, *o);
            from.insert(*o, t.id);
            *n += 1;
        }
    }
    let opened_ids: BTreeSet<Hash> = opened.iter().map(|t| t.id).collect();
    for t in resolved {
        if to.contains_key(&t.id) {
            continue;
        }
        let other = match t.severity {
            TensionSeverity::Blocking => TensionSeverity::Advisory,
            TensionSeverity::Advisory => TensionSeverity::Blocking,
        };
        let o = tension_id(t.kind, other, &t.involved);
        if opened_ids.contains(&o) && !from.contains_key(&o) {
            to.insert(t.id, o);
            from.insert(o, t.id);
        }
    }
    (to, from)
}

/// The suspects `next` made gone and new against `base`, each by key. The suspects are
/// canon's (task-15 ruling: in canon, a stale citation is a suspect; an in-flight citer of a
/// changed premise is a premise shift tension), so a batch that cannot change canon moves
/// none: same canon head, same pack. With the same pack, only the blocks whose suspects can
/// differ are compared (`Graph::suspect_candidates`); a new pack can change the
/// presentational trait or the tracking fields without changing a block, so then the whole
/// sets are compared.
#[allow(clippy::type_complexity)]
fn suspect_moves(
    base: &ReadState,
    next: &ReadState,
) -> Result<(BTreeMap<Hash, Suspect>, BTreeMap<Hash, Suspect>), RdyError> {
    let mut gone = BTreeMap::new();
    let mut new = BTreeMap::new();
    let same_pack = Arc::ptr_eq(&base.pack, &next.pack);
    if same_pack && base.canon_head == next.canon_head {
        return Ok((gone, new));
    }
    if !same_pack {
        let mut after = canon_suspects(next)?;
        for (k, s) in canon_suspects(base)? {
            if after.remove(&k).is_none() {
                gone.insert(k, s);
            }
        }
        return Ok((gone, after));
    }
    let bg = base.graph(&Scope::Canon)?;
    let ng = next.graph(&Scope::Canon)?;
    if Arc::ptr_eq(&bg, &ng) {
        return Ok((gone, new));
    }
    let candidates = ng.suspect_candidates(&bg);
    if candidates.is_empty() {
        return Ok((gone, new));
    }
    let bm = base.materialized(&Scope::Canon)?;
    let nm = next.materialized(&Scope::Canon)?;
    for b in &candidates {
        let before = keyed(base, &bm, bg.suspects_of(b));
        let mut after = keyed(next, &nm, ng.suspects_of(b));
        for (k, s) in before {
            if after.remove(&k).is_none() {
                gone.insert(k, s);
            }
        }
        new.extend(after);
    }
    Ok((gone, new))
}

/// The open suspects of `st`: canon's, without those of a presentational source, by key.
fn canon_suspects(st: &ReadState) -> Result<BTreeMap<Hash, Suspect>, RdyError> {
    Ok(st
        .suspects(&Scope::Canon)?
        .into_iter()
        .map(|s| (suspect_key(&s.block, &s.target, &s.reason), s))
        .collect())
}

/// `list` (suspects of `st`'s canon, materialized as `m`) without those of a presentational
/// source, by key.
fn keyed(st: &ReadState, m: &crate::Materialized, list: Vec<Suspect>) -> BTreeMap<Hash, Suspect> {
    list.into_iter()
        .filter(|s| !st.is_presentational_source(m, s))
        .map(|s| (suspect_key(&s.block, &s.target, &s.reason), s))
        .collect()
}

/// A suspect move of `s` in `st` (the state that holds it): its writers are the authors of
/// the live in-flight writes on its block there, sorted and distinct.
fn suspect_move(
    st: &ReadState,
    set: &TensionSet,
    s: &Suspect,
    why: Why,
) -> Result<SuspectMove, RdyError> {
    let mut writers = BTreeSet::new();
    for w in set.writes().all_on(&s.block) {
        if w.live {
            let (actor, run) = author(st, set, &w.change)?;
            writers.insert(Author { actor, run });
        }
    }
    Ok(SuspectMove {
        block: s.block.clone(),
        target: s.target.clone(),
        reason: s.reason.clone(),
        writers: writers.into_iter().collect(),
        why,
    })
}

/// The actor and run of in-flight change `c`: its facts in `set`, else its stored provenance.
fn author(
    st: &ReadState,
    set: &TensionSet,
    c: &Hash,
) -> Result<(String, Option<String>), RdyError> {
    Ok(match set.writes().change(c) {
        Some(f) => (f.actor.clone(), f.run.clone()),
        None => {
            let p = st.reader().change(c)?.provenance;
            (p.actor, p.run)
        }
    })
}

/// The sides of `t` as a move records them, with the authors `st` (the state that holds it)
/// gives its changes.
fn sides(st: &ReadState, set: &TensionSet, t: &Tension) -> Result<Vec<Side>, RdyError> {
    t.sides
        .iter()
        .map(|s| {
            Ok(match s {
                SideRef::Change(c) => {
                    let (actor, run) = author(st, set, c)?;
                    Side {
                        change: Some(*c),
                        actor: Some(actor),
                        run,
                        landing: None,
                    }
                }
                SideRef::Canon { landing } => Side {
                    change: None,
                    actor: None,
                    run: None,
                    landing: *landing,
                },
                // an intent side is read-time only, never in the open set
                SideRef::Intent { .. } => Side {
                    change: None,
                    actor: None,
                    run: None,
                    landing: None,
                },
            })
        })
        .collect()
}

/// What a write batch of `frames` did, as its moves' cause (closed, first match wins): a
/// landing; a commit; a review; an alternative set version; another view op; else written.
fn batch_why(reader: &StoreReader, frames: &[(u8, Hash)]) -> Result<Why, RdyError> {
    let first = |kind: u8| frames.iter().find(|(k, _)| *k == kind).map(|(_, h)| *h);
    if let Some(h) = first(kinds::LANDING) {
        return Ok(why(Cause::Landed, Some(h)));
    }
    for (_, h) in frames.iter().filter(|(k, _)| *k == kinds::VIEW_OP) {
        if let ViewOpKind::Commit { change } = reader.view_op(h)?.op {
            return Ok(why(Cause::Committed, Some(change)));
        }
    }
    if let Some(h) = first(kinds::REVIEW) {
        return Ok(why(Cause::Reviewed, Some(h)));
    }
    if let Some(h) = first(kinds::ALT_SET) {
        return Ok(why(Cause::AltSet, Some(h)));
    }
    if let Some(h) = first(kinds::VIEW_OP) {
        return Ok(why(Cause::ViewChanged, Some(h)));
    }
    Ok(why(Cause::Written, None))
}

/// The `at_ms` of the last event the batch's other frames give, else the clock.
fn at_ms(reader: &StoreReader, next: &ReadState, frames: &[(u8, Hash)]) -> i64 {
    events::events_for_batch(reader, next.seq(), frames)
        .ok()
        .and_then(|e| e.last().map(|e| e.at_ms))
        .unwrap_or_else(crate::now_ms)
}
