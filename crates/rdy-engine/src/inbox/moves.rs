//! Inbox entries from the logged tension and suspect moves (plan M2e2, founder decision C,
//! D-Z0EMP; ledger F3, F4, D-TPZ0W): each batch logs how it moved the open tensions and the
//! suspects, and the inbox turns the moves that concern a session into entries. No state diff,
//! no seen set, no cap.
use super::Entry;
use super::inflight::RunInFlight;
use rdy_model::Hash;
use rdy_store::inbox::InboxSub;
use rdy_store::segment::Location;
use rdy_store::transition::{Author, Cause, SuspectMove, TensionMove, Why};

/// One logged move of a batch, as Task 6 reads it from the batch's events.
#[derive(Clone, Copy, Debug)]
pub enum MoveRef<'a> {
    Opened(&'a TensionMove),
    Resolved(&'a TensionMove),
    New(&'a SuspectMove),
    Gone(&'a SuspectMove),
}

/// What made a move, as an entry line says it (one arm per cause, closed).
pub fn why_text(w: &Why) -> String {
    let id12 = || w.id.map(|h| h.short()).unwrap_or_default();
    match w.cause {
        Cause::Committed => format!("a side changed (change {})", id12()),
        Cause::Landed => format!("landing {}", id12()),
        Cause::Dismissed => format!("dismissed by {}", w.actor.as_deref().unwrap_or_default()),
        Cause::ViewChanged => format!("a view changed (op {})", id12()),
        Cause::Reviewed => format!("a review (review {})", id12()),
        Cause::AltSet => format!("an alternative set changed (version {})", id12()),
        Cause::Written => "a write".to_string(),
        Cause::PackReload => "the pack was reloaded".to_string(),
        Cause::Reopened => "the pack or rdy changed while the project was closed".to_string(),
        Cause::Recheck => "a clean republish found it".to_string(),
    }
}

/// The entries one batch's logged moves give the session (plan M2e2, founder decision C; F3,
/// F4): tensions on its in-flight changes, suspects on its blocks, dismissals. Every move is read:
/// no cap, no seen set.
///
/// `moves` is every move of the batch, in record order, and `first` is the raw index of
/// `moves[0]` in its batch (the batch's other events come first, numbered as `batch_items`
/// numbers them). Each entry comes with the raw index of the last move it consumes, ascending. A
/// re-key fold (a resolution and an opening that stay on the session's in-flight changes, shown
/// as one "replaces" entry) consumes both moves and reports the later one. A read that resumes a
/// batch at raw index `skip` still passes every move (a fold reads its partner) and keeps the
/// entries whose index is `skip` or more, so a cut never splits a fold.
pub fn transition_entries(
    sub: &InboxSub,
    infl: &RunInFlight,
    moves: &[MoveRef<'_>],
    first: usize,
    seq: Location,
) -> Vec<(usize, Entry)> {
    let mine: Vec<Vec<Hash>> = moves
        .iter()
        .map(|mv| match mv {
            MoveRef::Opened(m) | MoveRef::Resolved(m) => own_changes(sub, m),
            MoveRef::New(_) | MoveRef::Gone(_) => vec![],
        })
        .collect();
    let fold = folds(moves, &mine);
    let mut out = Vec::new();
    for (i, mv) in moves.iter().enumerate() {
        let made = match (*mv, fold[i]) {
            // a fold: the later of its two moves gives the one entry
            (_, Some(j)) if j > i => None,
            (MoveRef::Opened(n), Some(j)) => Some(opened(n, resolved_at(moves, j), &mine[i])),
            (MoveRef::Resolved(o), Some(j)) => match moves[j] {
                MoveRef::Opened(n) => Some(opened(n, Some(o), &mine[j])),
                _ => None,
            },
            (MoveRef::Opened(n), None) if !mine[i].is_empty() => Some(opened(n, None, &mine[i])),
            (MoveRef::Resolved(m), None) if !mine[i].is_empty() => Some(resolved(m)),
            (MoveRef::Gone(m), _) if suspect_concerns(sub, infl, m) => Some((
                "suspect_gone",
                vec![m.block.to_string(), m.target.to_string()],
                format!(
                    "{} is no longer suspect: {} {}: {}",
                    m.block,
                    m.target,
                    m.reason,
                    why_text(&m.why)
                ),
            )),
            (MoveRef::New(m), _) if suspect_concerns(sub, infl, m) => Some((
                "suspect_new",
                vec![m.block.to_string(), m.target.to_string()],
                format!(
                    "{} is suspect: {} {}: {}",
                    m.block,
                    m.target,
                    m.reason,
                    why_text(&m.why)
                ),
            )),
            _ => None,
        };
        if let Some((event, ids, line)) = made {
            out.push((
                first + i,
                Entry {
                    event,
                    ids,
                    line,
                    seq,
                },
            ));
        }
    }
    out
}

/// One entry before its `seq`: event, ids, line.
type Made = (&'static str, Vec<String>, String);

/// The session's in-flight changes among the sides of `m`, sorted and distinct: a side matches
/// when it holds a change and its actor and run are the session's (a canon side never matches).
/// Empty when the move does not concern the session.
fn own_changes(sub: &InboxSub, m: &TensionMove) -> Vec<Hash> {
    let mut out: Vec<Hash> = m
        .sides
        .iter()
        .filter(|s| s.actor.as_deref() == Some(sub.actor.as_str()) && s.run == sub.run)
        .filter_map(|s| s.change)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A suspect move concerns the session when the session is among its writers at the move, or
/// when the session's in-flight changes write its block now.
fn suspect_concerns(sub: &InboxSub, infl: &RunInFlight, m: &SuspectMove) -> bool {
    let me = Author {
        actor: sub.actor.clone(),
        run: sub.run.clone(),
    };
    m.writers.contains(&me) || infl.blocks.contains(&m.block)
}

/// For each move, the index of its fold partner: a resolution (not a dismissal) and an opening of
/// this batch that re-key onto each other, both on the session's in-flight changes.
fn folds(moves: &[MoveRef<'_>], mine: &[Vec<Hash>]) -> Vec<Option<usize>> {
    let mut out = vec![None; moves.len()];
    for (i, mv) in moves.iter().enumerate() {
        let MoveRef::Resolved(o) = *mv else { continue };
        if o.why.cause == Cause::Dismissed || mine[i].is_empty() {
            continue;
        }
        let Some(next) = o.rekey else { continue };
        let found = moves.iter().enumerate().position(|(j, mv)| {
            matches!(*mv, MoveRef::Opened(n)
                if n.tension == next && n.rekey == Some(o.tension) && !mine[j].is_empty())
        });
        if let Some(j) = found
            && out[j].is_none()
        {
            out[i] = Some(j);
            out[j] = Some(i);
        }
    }
    out
}

fn resolved_at<'a>(moves: &[MoveRef<'a>], j: usize) -> Option<&'a TensionMove> {
    match moves[j] {
        MoveRef::Resolved(o) => Some(o),
        _ => None,
    }
}

/// The `tension_resolved` or `tension_dismissed` entry of a resolution on the session's in-flight
/// changes that no opening of the batch folds.
fn resolved(m: &TensionMove) -> Made {
    let mut ids = vec![m.tension.to_string()];
    ids.extend(m.why.id.map(|h| h.to_string()));
    let t12 = m.tension.short();
    if m.why.cause == Cause::Dismissed {
        return (
            "tension_dismissed",
            ids,
            format!("tension {t12} {}", why_text(&m.why)),
        );
    }
    (
        "tension_resolved",
        ids,
        format!(
            "tension {t12} no longer involves your in-flight changes: {}",
            why_text(&m.why)
        ),
    )
}

/// The `tension_opened` entry of opening `n` on the session's in-flight changes `changes` (not
/// empty, sorted); `replaced` is the resolution it folds, if any.
fn opened(n: &TensionMove, replaced: Option<&TensionMove>, changes: &[Hash]) -> Made {
    let t12 = n.tension.short();
    let c12 = changes.first().map(Hash::short).unwrap_or_default();
    let what = match replaced {
        Some(o) => format!("replaces {} on your change", o.tension.short()),
        None => "opened on your change".to_string(),
    };
    let mut ids = vec![n.tension.to_string()];
    ids.extend(changes.iter().map(Hash::to_string));
    (
        "tension_opened",
        ids,
        format!(
            "tension {t12} {what} {c12} ({}, {}): {}",
            n.kind,
            n.severity,
            why_text(&n.why)
        ),
    )
}
