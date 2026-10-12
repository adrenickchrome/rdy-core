//! Spec §15.3 invariant 5, the reindex audit: a reindex and a reopen reproduce the derived state
//! (tension ids and statuses, conflict ids, every inbox) and the logged moves (D-0TE0F,
//! D-Z0EMP). The `logged` rule: the open tensions the log folds to are the derived open set, and
//! no move is left unlogged. Read-time tensions (an intent side) are outside every rule here.
use crate::invariants::Violation;
use rdy_engine::tensions::{TensionStatus, tension_label};
use rdy_engine::{FsckReport, Rdy, ReadState, Scope};
use rdy_store::segment::Location;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The page size `derived` reads an inbox with: large enough that only `MAX_ENTRIES` cuts a
/// page, small enough that a page's token sum cannot overflow.
const PAGE_TOKENS: usize = 1 << 40;

/// The derived state a reindex must reproduce, and the logged moves it must not add to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Derived {
    /// Every tension of `tensions()` by label (`tn-<12 hex>`): `open` or `dismissed`.
    pub tensions: BTreeMap<String, String>,
    /// The conflict ids of each open view.
    pub conflicts: BTreeMap<String, BTreeSet<String>>,
    /// Each subscription's entries from its persisted cursor to the end of the log, by
    /// `actor/run`.
    pub inboxes: BTreeMap<String, Vec<serde_json::Value>>,
    /// The ids of the store's open-tension table, as labels.
    pub logged_tensions: BTreeSet<String>,
    /// The keys of the store's open-suspect table.
    pub logged_suspects: BTreeSet<String>,
    /// The `TRANSITION` records in the log.
    pub transitions: usize,
    /// The events a replay from the log's start gives.
    pub events: usize,
}

fn viol(rule: &'static str, detail: String) -> Violation {
    Violation {
        rule,
        block: None,
        detail,
    }
}

/// The derived state of `r`, by pure reads: it never subscribes, acks or writes.
pub fn derived(r: &Rdy) -> Result<Derived, String> {
    let st = r.read();
    let reader = st.reader();
    let set = st
        .tensions()
        .map_err(|e| format!("cannot read tensions: {e}"))?;
    let tensions = set
        .iter()
        .map(|t| {
            let status = match st.tension_status(t) {
                TensionStatus::Open => "open",
                TensionStatus::Dismissed(_) => "dismissed",
            };
            (tension_label(&t.id), status.to_string())
        })
        .collect();
    let mut conflicts = BTreeMap::new();
    for name in st.view_names() {
        if st.view_head(&name).is_none_or(|h| h.closed) {
            continue;
        }
        let cs = st
            .conflicts(&Scope::View(name.clone()))
            .map_err(|e| format!("cannot read the conflicts of view {name}: {e}"))?;
        conflicts.insert(name, cs.iter().map(|c| c.id.to_string()).collect());
    }
    let mut inboxes = BTreeMap::new();
    for sub in reader
        .inboxes()
        .map_err(|e| format!("cannot read the inboxes: {e}"))?
    {
        let key = format!("{}/{}", sub.actor, sub.run.as_deref().unwrap_or(""));
        let mut entries = vec![];
        let mut at = sub.cursor;
        loop {
            let page = st
                .inbox_page(&sub, Some(at), PAGE_TOKENS)
                .map_err(|e| format!("cannot read the inbox of {key}: {e}"))?;
            for e in &page.entries {
                entries.push(
                    serde_json::to_value(e)
                        .map_err(|e| format!("cannot encode an entry of {key}: {e}"))?,
                );
            }
            if !page.more {
                break;
            }
            if page.next == at {
                return Err(format!(
                    "the inbox of {key} does not advance at {}",
                    page.cursor
                ));
            }
            at = page.next;
        }
        inboxes.insert(key, entries);
    }
    let logged_tensions = reader
        .open_tensions()
        .map_err(|e| format!("cannot read the open-tension table: {e}"))?
        .iter()
        .map(|(id, _)| tension_label(id))
        .collect();
    let logged_suspects = reader
        .open_suspects()
        .map_err(|e| format!("cannot read the open-suspect table: {e}"))?
        .iter()
        .map(|(k, _)| k.to_string())
        .collect();
    let transitions = reader
        .check_objects()
        .map_err(|e| format!("cannot check the store: {e}"))?
        .transitions;
    let events = r
        .replay_events(Location {
            segment: 0,
            offset: 0,
        })
        .map_err(|e| format!("cannot replay the log: {e}"))?
        .len();
    Ok(Derived {
        tensions,
        conflicts,
        inboxes,
        logged_tensions,
        logged_suspects,
        transitions,
        events,
    })
}

fn changed(what: &str, n: usize, m: usize) -> Violation {
    viol(
        "reindex",
        format!("reindex changed {what}: {n} only before, {m} only after"),
    )
}

/// How many items of `a` are not in `b`, and of `b` not in `a`.
fn set_diff<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> (usize, usize) {
    (a.difference(b).count(), b.difference(a).count())
}

/// The differences between two `Derived` values: rule `reindex`, one violation per differing
/// field (per view for conflicts, per subscription for inboxes).
pub fn compare(before: &Derived, after: &Derived) -> Vec<Violation> {
    let mut out = vec![];
    // a status that differs counts on both sides
    let pairs = |d: &Derived| -> BTreeSet<(String, String)> {
        d.tensions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    let (n, m) = set_diff(&pairs(before), &pairs(after));
    if n + m > 0 {
        out.push(changed("tension ids", n, m));
    }
    let empty = BTreeSet::new();
    let views: BTreeSet<&String> = before
        .conflicts
        .keys()
        .chain(after.conflicts.keys())
        .collect();
    for v in views {
        let (n, m) = set_diff(
            before.conflicts.get(v).unwrap_or(&empty),
            after.conflicts.get(v).unwrap_or(&empty),
        );
        if n + m > 0 {
            out.push(changed(&format!("conflicts of view {v}"), n, m));
        }
    }
    let none = vec![];
    let subs: BTreeSet<&String> = before.inboxes.keys().chain(after.inboxes.keys()).collect();
    for s in subs {
        let a = before.inboxes.get(s).unwrap_or(&none);
        let b = after.inboxes.get(s).unwrap_or(&none);
        // entries in order: a differing position counts on both sides
        let (mut n, mut m) = (0, 0);
        for i in 0..a.len().max(b.len()) {
            if a.get(i) != b.get(i) {
                n += usize::from(i < a.len());
                m += usize::from(i < b.len());
            }
        }
        if n + m > 0 {
            out.push(changed(&format!("the inbox of {s}"), n, m));
        }
    }
    let (n, m) = set_diff(&before.logged_tensions, &after.logged_tensions);
    if n + m > 0 {
        out.push(changed("the logged open tensions", n, m));
    }
    let (n, m) = set_diff(&before.logged_suspects, &after.logged_suspects);
    if n + m > 0 {
        out.push(changed("the logged open suspects", n, m));
    }
    if before.transitions != after.transitions {
        out.push(changed(
            "the logged moves",
            before.transitions,
            after.transitions,
        ));
    }
    if before.events != after.events {
        out.push(changed("the replayed events", before.events, after.events));
    }
    out
}

/// Rule `logged`: the store's open-tension table holds exactly the open tensions of
/// `tensions()`, and no tension or suspect move is left unlogged.
pub fn logged(r: &Rdy) -> Vec<Violation> {
    match r.fsck() {
        Ok(f) => logged_in(&r.read(), &f),
        Err(e) => vec![viol("logged", format!("fsck failed: {e}"))],
    }
}

/// The `logged` rule on the snapshot `st`, with `fsck` the report of the same handle.
pub(crate) fn logged_in(st: &ReadState, fsck: &FsckReport) -> Vec<Violation> {
    let mut out = vec![];
    match (st.tensions(), st.reader().open_tensions()) {
        (Ok(set), Ok(table)) => {
            let derived: BTreeSet<String> = set
                .iter()
                .filter(|t| !t.read_time && st.tension_status(t) == TensionStatus::Open)
                .map(|t| tension_label(&t.id))
                .collect();
            let logged: BTreeSet<String> = table.iter().map(|(id, _)| tension_label(id)).collect();
            let (n, m) = set_diff(&logged, &derived);
            if n + m > 0 {
                out.push(viol(
                    "logged",
                    format!(
                        "the logged open tensions differ from the derived ones: {n} only logged, {m} only derived"
                    ),
                ));
            }
        }
        (Err(e), _) => out.push(viol("logged", format!("cannot read tensions: {e}"))),
        (_, Err(e)) => out.push(viol(
            "logged",
            format!("cannot read the open-tension table: {e}"),
        )),
    }
    if fsck.unlogged_moves != 0 {
        out.push(viol(
            "logged",
            format!(
                "{} tension or suspect moves are not logged",
                fsck.unlogged_moves
            ),
        ));
    }
    out
}

/// Opens the project at `dir`, reads its derived state, reindexes, and compares; then closes,
/// reopens, compares with the first read, and applies the `logged` rule. The reindex must log
/// nothing: a reindex that logs a batch found a move the log missed.
pub fn reindex_reproduces(dir: &Path) -> Result<(), Vec<Violation>> {
    let fail = |what: &str, e: String| vec![viol("reindex", format!("{what}: {e}"))];
    let mut r = Rdy::open(dir).map_err(|e| fail("cannot open the project", e.to_string()))?;
    let first = derived(&r).map_err(|e| fail("before the reindex", e))?;
    r.reindex()
        .map_err(|e| fail("cannot reindex", e.to_string()))?;
    let second = derived(&r).map_err(|e| fail("after the reindex", e))?;
    let mut out = compare(&first, &second);
    if let Err(e) = r.close() {
        out.extend(fail("cannot close the project", e.to_string()));
        return Err(out);
    }
    let r = Rdy::open(dir).map_err(|e| fail("cannot reopen the project", e.to_string()))?;
    match derived(&r) {
        Ok(third) => out.extend(compare(&first, &third)),
        Err(e) => out.extend(fail("after the reopen", e)),
    }
    out.extend(logged(&r));
    if let Err(e) = r.close() {
        out.extend(fail("cannot close the project", e.to_string()));
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}
