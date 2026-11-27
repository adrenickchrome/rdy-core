//! r59018 (D-GGTYA, D-4PV25): the native store fixture `store/partial-history-1`, the first use
//! of the `partial_history` store feature. It is the store a project holds after it took the
//! frozen canon state `sync5-canon-state` (the state of the frozen `sync4-canon-history` root):
//! one segment, no span map and no derived index. The writer runs once; the pin opens a copy,
//! checks the partial rules, completes it with the frozen history, and checks that a dump, a
//! load and a reindex keep what it holds.
use rdy_engine::{E_HISTORY_PENDING, Rdy, partial_history};
use rdy_model::{Hash, RepoId};
use rdy_qa::foreign_fixture;
use rdy_store::Store;
use rdy_store::format::FEATURE_PARTIAL_HISTORY;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::SyncHistory;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::path::{Path, PathBuf};

/// The blake3 of the fixture's inventory text (`foreign_fixture::inventory_text`).
const INVENTORY_BLAKE3: &str = "dd60c751c132d5719043dce8d7887b96b66c4ca3478323db571227d8e3da8d28";
/// The blake3 of the fixture's expect file.
const EXPECT_BLAKE3: &str = "7f6e967ad0cabbc14be8ff009856cfcbdbef644ca8930aa6725db4c398c9fb06";
// test-hygiene: allow port the fetch target records this URL; nothing connects to it
const URL: &str = "https://127.0.0.1:9/r/demo";

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store/partial-history-1")
}

fn sync_fixture(name: &str) -> PathBuf {
    qa_dir().join("fixtures/sync").join(name)
}

fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The five members the writer creates, in the order it creates them.
fn members() -> Vec<PathBuf> {
    let d = fixture_dir();
    vec![
        d.join("FEATURES"),
        d.join("FORMAT"),
        d.join("lock"),
        d.join("segments/seg-000000.log"),
        d.join("expect.json"),
    ]
}

/// Writes `bytes` to the new file `p`; refuses a file that exists.
fn write_new(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| {
            panic!(
                "the partial history fixture is written once; output exists: {}: {e}",
                p.display()
            )
        });
    std::io::Write::write_all(&mut out, bytes).unwrap();
    out.sync_all().unwrap();
}

/// The state's repo and root, and the frozen history's keys.
fn state_facts() -> (RepoId, Hash, Vec<ObjectKey>) {
    let e = read_json(&sync_fixture("sync5-canon-state.expect.json"));
    let repo = e["source"]["repo_id"].as_str().unwrap().parse().unwrap();
    let root = e["source"]["root"]["hash"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let keys = read_json(&sync_fixture("sync4-canon-history.expect.json"))["stream"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| ObjectKey {
            kind: k["kind"].as_u64().unwrap() as u8,
            hash: k["hash"].as_str().unwrap().parse().unwrap(),
        })
        .collect();
    (repo, root, keys)
}

/// Writes the checked key file of remote origin into the project at `root`.
fn write_keys(root: &Path) {
    let (repo, frontier, keys) = state_facts();
    let path = partial_history::key_file(root, "origin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let value = partial_history::HistoryKeys {
        repo_id: repo,
        frontier,
        keys,
    };
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
}

/// What the opened partial project at `root` holds: its store manifest, log end, refs, tags,
/// canon landings and history progress, and its partial check.
fn partial_facts(root: &Path) -> Value {
    let r = Rdy::open(root).unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    let st = r.read();
    let end = r.store().log_end();
    let manifest = st
        .reader()
        .get_ref(rdy_store::format::REF_STORE_FEATURES)
        .unwrap()
        .map(|v| rdy_store::format::decode_features(&v).unwrap())
        .expect("a manifest");
    let mut refs: Vec<String> = st
        .reader()
        .refs_with_prefix("")
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    refs.sort();
    let tags: Vec<Value> = st
        .versions()
        .unwrap()
        .into_iter()
        .map(|v| json!([v.name, v.landing]))
        .collect();
    let landings: Vec<String> = st
        .landings()
        .unwrap()
        .into_iter()
        .map(|(h, _)| h.to_string())
        .collect();
    let facts = json!({
        "manifest": manifest,
        "log_end": [end.segment, end.offset],
        "refs": refs,
        "tags": tags,
        "landings": landings,
        "history": st.history_progress().unwrap(),
        "fsck": {"landings": report.landings, "history": report.history},
    });
    drop(st);
    r.close().unwrap();
    facts
}

/// The fixture's store copied into a fresh project, with its key file.
fn fixture_project() -> (tempfile::TempDir, PathBuf) {
    let (base, root) = foreign_fixture::project_copy(&fixture_dir());
    write_keys(&root);
    (base, root)
}

#[test]
#[ignore]
fn r59018_write_partial_history_fixture_once() {
    if let Some(p) = members().iter().find(|p| p.exists()) {
        panic!(
            "the partial history fixture is written once; output exists: {}",
            p.display()
        );
    }
    let (repo, frontier, keys) = state_facts();
    let base = tempfile::tempdir().unwrap();
    let store = base.path().join("store");
    let mut s = Store::create_fork_project(&store, repo).unwrap();
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
    let file = std::fs::File::open(sync_fixture("sync5-canon-state.bin")).unwrap();
    s.take_canon_state::<Infallible>(&[file], &target, &history, |_, _| Ok(()))
        .unwrap();
    s.close().unwrap();
    let dir = fixture_dir();
    for f in ["FEATURES", "FORMAT", "lock", "segments/seg-000000.log"] {
        write_new(&dir.join(f), &std::fs::read(store.join(f)).unwrap());
    }
    assert_eq!(
        foreign_fixture::files_under(&store.join("segments")),
        ["seg-000000.log"],
        "one segment"
    );
    let (_b, root) = fixture_project();
    let expect = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "source": {
            "state": "sync5-canon-state.bin",
            "history": "sync4-canon-history.bin",
            "repo_id": repo.to_string(),
            "frontier": frontier.to_string(),
            "records": keys.len(),
        },
        "inventory": foreign_fixture::inventory(&dir),
        "store": partial_facts(&root),
    });
    let bytes = (serde_json::to_string_pretty(&expect).unwrap() + "\n").into_bytes();
    write_new(&dir.join("expect.json"), &bytes);
    eprintln!(
        "INVENTORY_BLAKE3 {}",
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes())
    );
    eprintln!("EXPECT_BLAKE3 {}", Hash::of(&bytes));
}

/// r59018: the fixture opens, refuses a dump and an older rdy, and passes the partial check;
/// the frozen history completes it, after which the full check passes and the ref is set
/// complete; a dump, its load and a reindex keep its tags and landings; its released digests
/// stay fixed.
#[test]
fn r59018_first_use_fixture_and_completed_dump() {
    let dir = fixture_dir();
    let want = read_json(&dir.join("expect.json"));
    assert_eq!(
        foreign_fixture::files_under(&dir),
        [
            "FEATURES",
            "FORMAT",
            "expect.json",
            "lock",
            "segments/seg-000000.log"
        ],
        "one segment, no span map and no derived index"
    );
    assert_eq!(want["inventory"], foreign_fixture::inventory(&dir));
    assert_eq!(
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes()).to_string(),
        INVENTORY_BLAKE3
    );
    assert_eq!(
        foreign_fixture::digest_of(&dir.join("expect.json")).1,
        EXPECT_BLAKE3
    );
    let written = std::fs::read_to_string(dir.join("expect.json")).unwrap();
    for bad in ["/Users", "/home/", "/tmp", "/var/", "\\\\", ":\\"] {
        assert!(!written.contains(bad), "the expect file names {bad}");
    }
    let features = want["store"]["manifest"]["features"].as_array().unwrap();
    assert!(
        features
            .iter()
            .any(|f| f["name"] == FEATURE_PARTIAL_HISTORY),
        "the manifest holds partial_history"
    );

    // it opens with the partial rules, as recorded
    let (_b, root) = fixture_project();
    assert_eq!(partial_facts(&root), want["store"]);
    {
        let r = Rdy::open(&root).unwrap();
        let out = root.join("p.rdydump");
        let e = r.read().dump(&out).unwrap_err();
        assert_eq!(e.code, E_HISTORY_PENDING, "{e:?}");
        assert!(!out.exists());
        assert_eq!(r.fsck_full().unwrap_err().code, E_HISTORY_PENDING);
    }
    // an older rdy, which does not know the feature, refuses the store
    {
        let (_o, older) = fixture_project();
        let _old = rdy_store::format::older_reader_for_tests(&[FEATURE_PARTIAL_HISTORY]);
        let e = Rdy::open(&older).err().expect("an older rdy refuses it");
        assert_eq!(e.code, "E_STORE_FORMAT", "{e:?}");
        assert!(e.message.contains(FEATURE_PARTIAL_HISTORY), "{e:?}");
    }

    // the frozen history completes it: the full check, then the ref is set complete
    let (_, frontier, _) = state_facts();
    let store_dir = root.join(".rdy").join(rdy_store::layout::STORE_DIR);
    {
        let mut s = Store::open(&store_dir).unwrap();
        let target = FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: StreamPurpose::CanonHistory,
            root: frontier,
            view: None,
        };
        let file = std::fs::File::open(sync_fixture("sync4-canon-history.bin")).unwrap();
        s.take_fetched::<Infallible>(&[file], &target, |_, _| Ok(()))
            .unwrap();
        s.close().unwrap();
    }
    let r = Rdy::open(&root).unwrap();
    let full = r.fsck_full().unwrap();
    assert!(!full.history.unwrap().complete);
    let epoch = r.store().redaction_epoch();
    drop(r);
    {
        let mut s = Store::open(&store_dir).unwrap();
        s.complete_history("origin", frontier, epoch).unwrap();
        s.close().unwrap();
    }
    let mut r = Rdy::open(&root).unwrap();
    let done = r.fsck().unwrap();
    assert!(done.history.unwrap().complete);
    r.fsck_full().unwrap();
    let held = |r: &Rdy| -> Value {
        let st = r.read();
        let tags: Vec<Value> = st
            .versions()
            .unwrap()
            .into_iter()
            .map(|v| json!([v.name, v.landing]))
            .collect();
        let landings: Vec<String> = st
            .landings()
            .unwrap()
            .into_iter()
            .map(|(h, _)| h.to_string())
            .collect();
        json!({"tags": tags, "landings": landings})
    };
    let before = held(&r);
    assert_eq!(before["landings"].as_array().unwrap().len(), 3, "{before}");
    assert_eq!(before["tags"], want["store"]["tags"], "{before}");
    // a reindex keeps them
    r.reindex().unwrap();
    assert_eq!(held(&r), before);
    // a dump and its load keep them
    let scratch = tempfile::tempdir().unwrap();
    let dump = scratch.path().join("done.rdydump");
    r.read().dump(&dump).unwrap();
    r.close().unwrap();
    let loaded = scratch.path().join("loaded");
    rdy_engine::load::load(&dump, &loaded).unwrap();
    let l = Rdy::open(&loaded).unwrap();
    l.fsck().unwrap();
    assert_eq!(held(&l), before);
    assert!(l.read().history_warning().unwrap().is_none());
}
