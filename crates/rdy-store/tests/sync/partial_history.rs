//! r59018 (D-GGTYA, D-4PV25): a project that took a canon state with no history. The take is
//! one durable boundary; the history ref holds the frontier and the exact record count; the
//! closed state link table is the take's only missing-link exception.
use rdy_model::*;
use rdy_store::Store;
use rdy_store::format::FEATURE_PARTIAL_HISTORY;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::SyncHistory;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use std::convert::Infallible;
use std::path::{Path, PathBuf};

fn qa_sync_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/sync")
        .join(name)
}

fn expect(name: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(qa_sync_fixture(name)).unwrap()).unwrap()
}

/// The frozen canon state's repo and root.
fn state_facts() -> (RepoId, Hash) {
    let e = expect("sync5-canon-state.expect.json");
    let repo = e["source"]["repo_id"].as_str().unwrap();
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&repo[2 * i..2 * i + 2], 16).unwrap();
    }
    let root: Hash = e["source"]["root"]["hash"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (RepoId(id), root)
}

/// The frozen canon history's keys: the full history at the state's frontier.
fn history_keys() -> Vec<ObjectKey> {
    let e = expect("sync4-canon-history.expect.json");
    e["stream"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| ObjectKey {
            kind: k["kind"].as_u64().unwrap() as u8,
            hash: k["hash"].as_str().unwrap().parse().unwrap(),
        })
        .collect()
}

// test-hygiene: allow port the fetch target records this URL; nothing connects to it
const URL: &str = "https://127.0.0.1:9/r/demo";

fn target(root: Hash) -> FetchTarget {
    FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::CanonState,
        root,
        view: None,
    }
}

fn history(repo: RepoId, root: Hash, records: u64) -> SyncHistory {
    SyncHistory {
        repo_id: repo,
        url: URL.into(),
        frontier: root,
        complete: false,
        records,
    }
}

fn state_file() -> std::fs::File {
    std::fs::File::open(qa_sync_fixture("sync5-canon-state.bin")).unwrap()
}

/// A new project of the state's repo at `dir`, which took the frozen canon state.
fn partial(dir: &Path) -> Store {
    let (repo, root) = state_facts();
    let mut s = Store::create_fork_project(dir, repo).unwrap();
    s.take_canon_state::<Infallible>(
        &[state_file()],
        &target(root),
        &history(repo, root, history_keys().len() as u64),
        |_, _| Ok(()),
    )
    .unwrap();
    s
}

/// r59018: the take is one sealed batch: the state's records, canon at the root, the sync canon
/// fetched and applied at it, the incomplete history ref and the first use of partial_history
/// land together, and each survives a reopen. Before it the store holds none of them.
#[test]
fn r59018_state_install_is_one_durable_boundary() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("p");
    let (repo, root) = state_facts();
    let mut s = Store::create_fork_project(&dir, repo).unwrap();
    let before = s.log_end();
    assert!(!s.features().has(FEATURE_PARTIAL_HISTORY));
    assert_eq!(s.reader().partial_history().unwrap(), None);
    let records = history_keys().len() as u64;
    s.take_canon_state::<Infallible>(
        &[state_file()],
        &target(root),
        &history(repo, root, records),
        |_, _| Ok(()),
    )
    .unwrap();
    assert!(s.log_end() > before);
    // one sealed batch from the store's end before the take to its end after it
    let mut batches = 0;
    s.reader()
        .for_each_batch_after(before, s.log_end(), &mut |_| {
            batches += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(batches, 1, "the state install is one batch");
    s.close().unwrap();

    let s = Store::open(&dir).unwrap();
    let r = s.reader();
    assert_eq!(r.canon_head().unwrap(), Some(root));
    assert!(s.features().has(FEATURE_PARTIAL_HISTORY));
    let h = r
        .partial_history()
        .unwrap()
        .expect("the history ref is incomplete");
    assert_eq!(h, history(repo, root, records));
    assert_eq!(r.sync_history("origin").unwrap(), Some(h));
    assert_eq!(r.history_frontier().unwrap(), Some(root));
    // the canon walk stops at the frontier, never at its missing prev
    let landings = r.landings().unwrap();
    assert_eq!(landings.len(), 1);
    assert_eq!(landings[0].0, root);
    assert!(landings[0].1.prev.is_some());
    assert!(
        !r.has_kind(kinds::LANDING, &landings[0].1.prev.unwrap())
            .unwrap()
    );
    // the root's map is readable
    assert!(!r.canon_state().unwrap().blocks.is_empty());
    s.verify().unwrap();
    // a dump of the store's batches is refused while the history is incomplete
    let e = r
        .for_each_dump_batch_after(before, s.log_end(), &mut |_| Ok(()))
        .unwrap_err();
    assert!(e.to_string().contains("still arriving"), "{e}");
}

/// r59018: a second canon state, a take into a project with another canon and a history ref
/// that names another frontier are refused, and nothing is written.
#[test]
fn r59018_a_state_take_has_one_root() {
    let d = tempfile::tempdir().unwrap();
    let mut s = partial(&d.path().join("p"));
    let (repo, root) = state_facts();
    let end = s.log_end();
    let mut other = history(repo, root, 3);
    other.frontier = Hash::of(b"another frontier");
    let e = s
        .take_canon_state::<Infallible>(&[state_file()], &target(root), &other, |_, _| Ok(()))
        .unwrap_err();
    assert!(e.to_string().contains("history"), "{e}");
    assert_eq!(s.log_end(), end);
}

/// R1: completion refuses an absent or complete ref, another frontier and another redaction
/// epoch, writing nothing; the exact frontier and epoch complete it, after which no history is
/// partial and the ref survives a reopen as complete.
#[test]
fn r59018_complete_history_refuses_a_changed_frontier_or_epoch() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("p");
    let mut s = partial(&dir);
    let (_, root) = state_facts();
    let epoch = s.redaction_epoch();
    let end = s.log_end();
    let refused = |s: &mut Store, remote: &str, frontier: Hash, epoch: u64| {
        let e = s.complete_history(remote, frontier, epoch).unwrap_err();
        e.to_string()
    };
    assert!(refused(&mut s, "upstream", root, epoch).contains("no history"));
    assert!(refused(&mut s, "origin", Hash::of(b"elsewhere"), epoch).contains("frontier"));
    assert!(refused(&mut s, "origin", root, epoch + 1).contains("epoch"));
    assert_eq!(s.log_end(), end, "a refused completion writes nothing");
    assert!(s.reader().partial_history().unwrap().is_some());

    s.complete_history("origin", root, epoch).unwrap();
    assert_eq!(s.reader().partial_history().unwrap(), None);
    assert!(refused(&mut s, "origin", root, epoch).contains("complete already"));
    s.close().unwrap();
    let s = Store::open(&dir).unwrap();
    let h = s.reader().sync_history("origin").unwrap().unwrap();
    assert!(h.complete);
    assert_eq!(s.reader().partial_history().unwrap(), None);
    assert!(s.features().has(FEATURE_PARTIAL_HISTORY));
}
