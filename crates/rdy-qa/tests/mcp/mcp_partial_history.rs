//! r59018 (D-GGTYA): the tools on a project whose history is still arriving. A read works and
//! carries no refusal; rdy_log stops at the frontier with one W_HISTORY_PENDING; rdy_dump is
//! refused with the one E_HISTORY_PENDING text; rdy_schema lists both codes.
use rdy_engine::{Rdy, partial_history};
use rdy_model::{Hash, RepoId};
use rdy_qa::daemon::Served;
use rdy_qa::mcp::McpClient;
use rdy_store::Store;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::SyncHistory;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::path::{Path, PathBuf};

fn qa_sync_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/sync")
        .join(name)
}

fn expect(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(qa_sync_fixture(name)).unwrap()).unwrap()
}

// test-hygiene: allow port the fetch target records this URL; nothing connects to it
const URL: &str = "https://127.0.0.1:9/r/demo";

/// Makes the project at `root` one that took the frozen canon state: its store replaced by a
/// new store of the state's repo, the state taken into it, and the history's key file written.
fn partial_seed(root: &Path) {
    let e = expect("sync5-canon-state.expect.json");
    let repo: RepoId = e["source"]["repo_id"].as_str().unwrap().parse().unwrap();
    let frontier: Hash = e["source"]["root"]["hash"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let keys: Vec<ObjectKey> = expect("sync4-canon-history.expect.json")["stream"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| ObjectKey {
            kind: k["kind"].as_u64().unwrap() as u8,
            hash: k["hash"].as_str().unwrap().parse().unwrap(),
        })
        .collect();
    drop(Rdy::init(root, "rdy-standard").unwrap());
    let store = root.join(".rdy").join(rdy_store::layout::STORE_DIR);
    std::fs::remove_dir_all(&store).unwrap();
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
    let file = std::fs::File::open(qa_sync_fixture("sync5-canon-state.bin")).unwrap();
    s.take_canon_state::<Infallible>(&[file], &target, &history, |_, _| Ok(()))
        .unwrap();
    s.close().unwrap();
    let path = partial_history::key_file(root, "origin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let value = partial_history::HistoryKeys {
        repo_id: repo,
        frontier,
        keys,
    };
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
}

const PENDING: &str = "the project's history is still arriving (4 of 19 records)";
const WARNING: &str = "history is still arriving (4 of 19 records)";
const FIX: &str = "wait for the clone to finish, or run rdy pull to resume it";

#[test]
fn r59018_tools_on_a_partial_project() {
    let s = Served::seeded(partial_seed);
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let read = c.call("rdy_read", json!({"id": "d-JQKR9"}));
    assert_eq!(read.code(), None, "{read:?}");

    let log = c.call("rdy_log", json!({})).ok();
    assert_eq!(log["items"], json!([]), "{log}");
    assert_eq!(log["warnings"].as_array().unwrap().len(), 1, "{log}");
    assert_eq!(log["warnings"][0]["code"], "W_HISTORY_PENDING", "{log}");
    assert_eq!(log["warnings"][0]["message"], WARNING, "{log}");
    assert_eq!(log["warnings"][0]["fix"], FIX, "{log}");

    let dump = c.call("rdy_dump", json!({"out": "p.rdydump"}));
    assert_eq!(dump.code(), Some("E_HISTORY_PENDING"), "{dump:?}");
    assert!(format!("{dump:?}").contains(PENDING), "{dump:?}");
    assert!(!s.dir.path().join("p.rdydump").exists());
}
