//! r59018 (D-GGTYA, D-4PV25): a project whose history is still arriving. It opens at its canon
//! state for read, brief, write, propose and land at once; every call that needs records behind
//! the frontier refuses with E_HISTORY_PENDING, and every read that omits history says so once
//! with W_HISTORY_PENDING.
use rdy_engine::*;
use rdy_model::*;
use rdy_store::Store;
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

/// The frozen canon history's keys, in its export order.
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

/// A project at `root` that took the frozen canon state: an initialized project whose store is
/// replaced by a new store of the state's repo, the state taken into it, and the history's
/// checked key file written.
fn partial_project(root: &Path) {
    let (repo, frontier) = state_facts();
    Rdy::init(root, "rdy-standard").unwrap().close().unwrap();
    let store = root.join(".rdy").join(rdy_store::layout::STORE_DIR);
    std::fs::remove_dir_all(&store).unwrap();
    let mut s = Store::create_fork_project(&store, repo).unwrap();
    let keys = history_keys();
    let history = SyncHistory {
        repo_id: repo,
        url: URL.into(),
        frontier,
        complete: false,
        records: keys.len() as u64,
    };
    let target = FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::CanonState,
        root: frontier,
        view: None,
    };
    let file = std::fs::File::open(qa_sync_fixture("sync5-canon-state.bin")).unwrap();
    s.take_canon_state::<Infallible>(&[file], &target, &history, |_, _| Ok(()))
        .unwrap();
    s.close().unwrap();
    write_keys(root, repo, frontier, keys);
}

/// Writes the history's checked key file of remote origin.
fn write_keys(root: &Path, repo: RepoId, frontier: Hash, keys: Vec<ObjectKey>) {
    let path = partial_history::key_file(root, "origin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let value = partial_history::HistoryKeys {
        repo_id: repo,
        frontier,
        keys,
    };
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn writer() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "r59018".into(),
        },
    )
}

const BLOCK: &str = "d-JQKR9";

fn pending(e: &RdyError, what: &str) {
    assert_eq!(e.code, E_HISTORY_PENDING, "{what}: {e:?}");
    assert_eq!(
        e.message, "the project's history is still arriving (4 of 19 records)",
        "{what}"
    );
    assert_eq!(
        e.fix, "wait for the clone to finish, or run rdy pull to resume it",
        "{what}"
    );
}

/// r59018: every surface that needs records behind the frontier refuses with the one
/// E_HISTORY_PENDING, before it creates or writes anything.
#[test]
fn r59018_pending_refusals_cover_all_surfaces() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let mut r = Rdy::open(&root).unwrap();
    let st = r.read();
    pending(&st.require_complete_history().unwrap_err(), "the guard");
    // whole and incremental dump, before any byte
    let out = d.path().join("dump.rdy");
    pending(&st.dump(&out).unwrap_err(), "dump");
    assert!(!out.exists());
    // local clone, fork and fork-at, before the destination is made
    for (what, fork, at) in [("clone", false, None), ("fork", true, None)] {
        let dir = d.path().join(what);
        let e = clone::clone_at(clone::CloneSource::State(&st), &dir, fork, at).unwrap_err();
        pending(&e, what);
        assert!(!dir.exists(), "{what}");
    }
    let tags = st.versions().unwrap();
    let held = tags.first().expect("the state holds its tag").name.clone();
    let dir = d.path().join("fork-at");
    let e = clone::clone_at(clone::CloneSource::State(&st), &dir, true, Some(&held)).unwrap_err();
    pending(&e, "fork-at");
    assert!(!dir.exists());
    // a read at a version or landing the state does not hold
    pending(&st.resolve_scope("tag:0.0.1").unwrap_err(), "old tag");
    pending(
        &st.resolve_scope("landing:0000000000000000").unwrap_err(),
        "old landing",
    );
    // the held frontier tag stays readable
    let at = st.resolve_scope(&format!("tag:{held}")).unwrap();
    assert!(st.context(&at, &BLOCK.parse().unwrap()).is_ok());
    drop(st);
    // an explicit id that may name a block deleted behind the frontier
    let end = r.store().log_end();
    let e = write(
        &mut r,
        "w",
        serde_json::json!([{"op": "create", "type": "prose", "id": "p-AAAAA", "parent": BLOCK, "body": "Back."}]),
    )
    .unwrap_err();
    pending(&e, "explicit id");
    assert_eq!(r.store().log_end(), end, "a refusal writes nothing");
}

/// r59018: read, write, propose and land work on the partial project; the history reads omit
/// what is behind the frontier and say so once with W_HISTORY_PENDING; the log stops there.
#[test]
fn r59018_read_warnings_do_not_hide_content() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let mut r = Rdy::open(&root).unwrap();
    let id: BlockId = BLOCK.parse().unwrap();
    {
        let st = r.read();
        assert!(st.context(&Scope::Canon, &id).is_ok());
        let w = st.history_warning().unwrap().expect("the warning");
        assert_eq!(w.code, W_HISTORY_PENDING);
        assert_eq!(w.message, "history is still arriving (4 of 19 records)");
        assert_eq!(
            w.fix,
            "wait for the clone to finish, or run rdy pull to resume it"
        );
        // the last change of the root's block is behind the frontier: left out, no error
        assert!(
            st.last_changes(&Scope::Canon, std::slice::from_ref(&id))
                .unwrap()
                .is_empty()
        );
        assert!(
            st.history(&Scope::Canon, &id, 10)
                .unwrap()
                .entries
                .is_empty()
        );
        // the canon walk and the log stop at the frontier
        assert_eq!(st.landings().unwrap().len(), 1);
        assert!(st.log_page(None, None).unwrap().is_empty());
    }
    // a fresh write, its proposal and its landing extend the frontier normally
    write(
        &mut r,
        "w",
        serde_json::json!([{"op": "create", "type": "prose", "id": "$p", "parent": BLOCK, "body": "A new paragraph."}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "w", "w", Some(crate::common::TEST_SUMMARY))
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    let st = r.read();
    assert_eq!(st.landings().unwrap().len(), 2);
    assert_eq!(
        st.log_page(None, None).unwrap().len(),
        1,
        "the local landing is logged"
    );
    assert!(st.history_warning().unwrap().is_some(), "still partial");
    drop(st);
    // the local landing's delta continues the chain from the frontier's map
    let report = r.fsck().unwrap();
    assert_eq!(report.landings, 2);
    assert!(!report.history.unwrap().complete);
}

/// r59018: the progress is exact (listed keys held plus staged trailer records, at most the
/// total) and a read appends nothing to the store.
#[test]
fn r59018_progress_is_exact_and_not_persisted() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let r = Rdy::open(&root).unwrap();
    let end = r.store().log_end();
    let st = r.read();
    let (_, frontier) = state_facts();
    let p = st.history_progress().unwrap().unwrap();
    assert_eq!(
        p,
        partial_history::HistoryProgress {
            frontier,
            complete: false,
            held: 4,
            total: 19,
        }
    );
    // the listed keys the store holds, counted from the frozen expectations
    let held = history_keys()
        .iter()
        .filter(|k| st.reader().has_kind(k.kind, &k.hash).unwrap())
        .count() as u64;
    assert_eq!(held, p.held);
    assert_eq!(r.store().log_end(), end, "a progress read writes nothing");
    drop(st);
    // a damaged key file is refused, not guessed around
    let path = partial_history::key_file(&root, "origin");
    std::fs::write(
        &path,
        br#"{"repo_id":"00","frontier":"00","keys":[],"extra":1}"#,
    )
    .unwrap();
    drop(r);
    let r = Rdy::open(&root).unwrap();
    assert!(r.read().history_progress().is_err());
}

/// R2: two progress reads on one state open the key file once and hash no staged payload; a
/// staged batch counts by its trailer alone.
#[test]
fn r59018_a_read_hashes_no_staged_payload_and_reads_the_key_file_once() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    // a staged batch of the history: the frozen canon history stream
    std::fs::copy(
        qa_sync_fixture("sync4-canon-history.bin"),
        partial_history::history_dir(&root, "origin").join("000001.batch"),
    )
    .unwrap();
    let r = Rdy::open(&root).unwrap();
    let st = r.read();
    let (opens, hashes) = (
        partial_history::key_file_opens_for_tests(),
        partial_history::staged_payload_hashes_for_tests(),
    );
    let first = st.history_progress().unwrap().unwrap();
    let second = st.history_progress().unwrap().unwrap();
    assert!(st.history_warning().unwrap().is_some());
    assert_eq!(first, second);
    assert_eq!(partial_history::key_file_opens_for_tests() - opens, 1);
    assert_eq!(
        partial_history::staged_payload_hashes_for_tests() - hashes,
        0
    );
    // the trailer's 19 records reach the total, never past it
    assert_eq!(first.held, 19);
    assert!(!first.complete);
}

/// r59018: fsck of the partial project hashes every held frame and reports the exact partial
/// history; the full check refuses the missing history; a damaged key file fails it.
#[test]
fn r59018_partial_fsck_is_strict() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let r = Rdy::open(&root).unwrap();
    let report = r.fsck().unwrap();
    assert_eq!(
        report.history,
        Some(FsckHistory {
            complete: false,
            held: 4,
            total: 19,
        })
    );
    assert_eq!(report.history.unwrap().line(), "history: partial (4 of 19)");
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(
        v["history"],
        serde_json::json!({"complete": false, "held": 4, "total": 19})
    );
    pending(&r.fsck_full().unwrap_err(), "full check");
    drop(r);
    // a key file of another frontier fails the check
    let path = partial_history::key_file(&root, "origin");
    let mut keys: partial_history::HistoryKeys =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    keys.frontier = Hash::of(b"elsewhere");
    std::fs::write(&path, serde_json::to_vec(&keys).unwrap()).unwrap();
    let r = Rdy::open(&root).unwrap();
    let e = r.fsck().unwrap_err();
    assert_eq!(e.code, E_BAD_RECORD, "{e:?}");
}
