//! The swarm's oracles (sg5): what the final state must hold, computed apart from the paths the
//! actors used. The reference tensions are a full rebuild after a reindex, in this process, not
//! the carry the agents saw; the collisions come from the journal and the included sets only,
//! never from a tension (Review Focus 2). Each oracle reads a copy of the project, with no daemon,
//! so a reopen that logs a batch never changes what another oracle reads.
use super::journal::{Entry, Outcome};
use crate::daemon::{human_command, short_tempdir};
use crate::fixtures::output_within;
use rdy_engine::tensions::{TensionStatus, tension_label};
use rdy_engine::{EventKind, Rdy};
use rdy_store::segment::Location;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A command's hang guard, not a speed check.
const CLI_BUDGET: Duration = Duration::from_secs(120);

/// Copies the tree `from` to `to` (which must not exist): directories, files and symbolic
/// links. A socket or another special file is left out: it means nothing without its process.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else if kind.is_file() {
            std::fs::copy(&src, &dst)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        }
    }
    Ok(())
}

/// A copy of the project at `dir`, in a new short temporary directory: the guard that removes
/// it, and its path.
pub fn copy_of(dir: &Path) -> (tempfile::TempDir, PathBuf) {
    let keep = short_tempdir();
    let to = keep.path().join("p");
    copy_tree(dir, &to).unwrap_or_else(|e| panic!("cannot copy {}: {e}", dir.display()));
    (keep, to)
}

/// Opens a copy of `dir` in this process; `f` reads it; the copy is closed and removed.
fn on_copy<T>(dir: &Path, f: impl FnOnce(&mut Rdy) -> T) -> T {
    let (_keep, copy) = copy_of(dir);
    let mut r =
        Rdy::open(&copy).unwrap_or_else(|e| panic!("cannot open a copy of {}: {e}", dir.display()));
    let out = f(&mut r);
    r.close()
        .unwrap_or_else(|e| panic!("cannot close a copy of {}: {e}", dir.display()));
    out
}

/// The reference tensions of the project at `frozen`: in this process, on a copy with no
/// daemon, a reindex and then a full build of the tension set (`tensions_rebuilt`, not the
/// carry the agents saw); the open ones, label → kind. A read-time intent overlap is not in the
/// set.
pub fn reference(frozen: &Path) -> BTreeMap<String, String> {
    on_copy(frozen, |r| {
        r.reindex()
            .unwrap_or_else(|e| panic!("cannot reindex a copy of {}: {e}", frozen.display()));
        let st = r.read();
        let set = st
            .tensions_rebuilt()
            .unwrap_or_else(|e| panic!("cannot build the tensions of {}: {e}", frozen.display()));
        set.iter()
            .filter(|t| !t.read_time && st.tension_status(t) == TensionStatus::Open)
            .map(|t| (tension_label(&t.id), t.kind.name().to_string()))
            .collect()
    })
}

/// The tension moves the log of the project at `frozen` holds, from its start, in log order:
/// each `TensionOpened` and `TensionResolved` event of a replay, with its batch's location. Each
/// wraps the logged `TensionMove`, with its sides' actor, run and change.
pub fn logged_moves(frozen: &Path) -> Vec<(Location, EventKind)> {
    on_copy(frozen, |r| {
        let events = r
            .replay_events(Location {
                segment: 0,
                offset: 0,
            })
            .unwrap_or_else(|e| panic!("cannot replay the log of {}: {e}", frozen.display()));
        events
            .into_iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EventKind::TensionOpened(_) | EventKind::TensionResolved(_)
                )
            })
            .map(|e| (e.seq, e.kind))
            .collect()
    })
}

/// The open views of the project at `frozen` that include each change, by change id: what is
/// still in flight there.
pub fn holders(frozen: &Path) -> BTreeMap<String, BTreeSet<String>> {
    on_copy(frozen, |r| {
        let st = r.read();
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for name in st.view_names() {
            if st.view_head(&name).is_none_or(|h| h.closed) {
                continue;
            }
            for c in st.included(&name).iter() {
                out.entry(c.to_string()).or_default().insert(name.clone());
            }
        }
        out
    })
}

/// The edit collisions the journal shows (D-84QSN): pairs of acknowledged `collide` changes on
/// one block from one base, by two different agents, whose two views were never members of one
/// alternative set (journal `alt_create` entries), and whose changes are both still in an open
/// view's included set in the project at `frozen`. Each pair is sorted. It reads only the journal
/// and those included sets, never a tension.
pub fn collisions(journal: &[Entry], frozen: &Path) -> BTreeSet<(String, String)> {
    let sets: Vec<BTreeSet<&str>> = journal
        .iter()
        .filter(|e| e.act == "alt_create" && e.outcome == Outcome::Ok)
        .map(|e| {
            e.view
                .iter()
                .chain(e.block.iter())
                .map(String::as_str)
                .collect()
        })
        .collect();
    let one_set = |a: &str, b: &str| sets.iter().any(|s| s.contains(a) && s.contains(b));
    let held = holders(frozen);
    let collides: Vec<(&Entry, &str, &str, &str, &str)> = journal
        .iter()
        .filter(|e| e.act == "collide" && e.outcome == Outcome::Ok)
        .filter_map(|e| {
            Some((
                e,
                e.change.as_deref()?,
                e.block.as_deref()?,
                e.base.as_deref()?,
                e.view.as_deref()?,
            ))
        })
        .filter(|(_, c, ..)| held.contains_key(*c))
        .collect();
    let mut out = BTreeSet::new();
    for (i, (x, cx, bx, basex, vx)) in collides.iter().enumerate() {
        for (y, cy, by, basey, vy) in &collides[i + 1..] {
            if x.actor != y.actor && bx == by && basex == basey && !one_set(vx, vy) {
                let (a, b) = if cx < cy { (cx, cy) } else { (cy, cx) };
                out.insert((a.to_string(), b.to_string()));
            }
        }
    }
    out
}

/// The keys (kind, sorted blocks) of the open, blocking, all-canon tensions of an `rdy_tensions`
/// reply.
fn canon_blocking(reply: &Value) -> BTreeSet<(String, String)> {
    reply["tensions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["status"] == "open" && t["severity"] == "blocking")
        .filter(|t| {
            t["sides"].as_array().is_some_and(|s| {
                s.iter()
                    .all(|s| s["change"].is_null() && s["intent"].is_null())
            })
        })
        .map(|t| {
            let mut blocks: Vec<&str> = t["blocks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            blocks.sort_unstable();
            (
                t["kind"].as_str().unwrap_or("?").to_string(),
                blocks.join(", "),
            )
        })
        .collect()
}

/// The tensions a landing put into canon (ledger O2, the simulation form of invariant 6): those
/// open, blocking and all-canon in `after` and absent from `before`, keyed by kind and sorted
/// blocks. `before` and `after` are the relay's `rdy_tensions {scope: "canon", severity:
/// "blocking"}` replies around the landing. Each is `"{kind} on {blocks}"`.
pub fn new_blocking_canon(before: &Value, after: &Value) -> Vec<String> {
    let was = canon_blocking(before);
    canon_blocking(after)
        .difference(&was)
        .map(|(kind, blocks)| format!("{kind} on {blocks}"))
        .collect()
}

/// `rdy --json fsck` on a copy of the project at `dir`, in the command's own process (no
/// daemon), as the human `env` names: whether it succeeded, and its JSON.
pub fn fsck(dir: &Path, env: &[(String, String)]) -> (bool, Value) {
    let (_keep, copy) = copy_of(dir);
    let mut cmd = human_command(&copy, env, &["--json", "fsck"]);
    cmd.env("RDY_NO_DAEMON", "1");
    let out = output_within(&mut cmd, CLI_BUDGET).unwrap_or_else(|e| panic!("rdy fsck hung: {e}"));
    let ok = out.status.success();
    let text = String::from_utf8_lossy(if ok { &out.stdout } else { &out.stderr }).into_owned();
    let v = serde_json::from_str(text.trim()).unwrap_or(Value::String(text));
    (ok, v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(kind: &str, severity: &str, blocks: &[&str], change: bool) -> Value {
        let side = if change {
            json!({"change": "ab12"})
        } else {
            json!({"landing": "cd34"})
        };
        json!({"kind": kind, "severity": severity, "status": "open", "blocks": blocks,
            "sides": [side, {"landing": "ef56"}]})
    }

    #[test]
    fn sg5_new_blocking_canon_keys_by_kind_and_sorted_blocks() {
        let before = json!({"tensions": [row("fork", "blocking", &["c-2", "c-1"], false)]});
        let after = json!({"tensions": [
            row("fork", "blocking", &["c-1", "c-2"], false),
            row("duplicate_definition", "blocking", &["t-2", "t-1"], false),
            row("value_disagreement", "advisory", &["c-3"], false),
            row("edit_collision", "blocking", &["c-4"], true),
        ]});
        assert_eq!(
            new_blocking_canon(&before, &after),
            vec!["duplicate_definition on t-1, t-2".to_string()]
        );
        assert!(new_blocking_canon(&after, &before).is_empty());
    }
}
