//! mht9 (D-6M0AF): MCP over HTTP stays out of the adapter. An adapter holder and a captured
//! snapshot are the in-process API of another program. Opening one and reading through it
//! creates no bearer token, no run directory, no daemon file and no site listener, and looks up
//! no person home. The adapter's dependency closure holds no daemon, no MCP crate, no server stack
//! (`ureq`, the hub client's HTTP client, is not one), so its direction stays store-ward.

use rdy_adapter::{ReadRequest, SearchRequest, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The variable that makes this binary's child test act, and names the project it opens.
const CHILD_PROJECT: &str = "RDY_QA_MHT9_ADAPTER_PROJECT";
const CHILD: &str = "mht9_adapter_child";

/// A landed project with a doc, closed again.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let session = Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    };
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": "The ledger keeps balances."}
    ]);
    let w = r
        .write(
            &session,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "mht9".into(),
            },
        )
        .unwrap();
    std::fs::write(dir.path().join("DOC_ID"), &w.created["$doc"]).unwrap();
    let p = r
        .propose(&session, "work", "T", Some("test summary"))
        .unwrap();
    let approver = Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: SessionKind::Human,
    };
    r.land(&approver, &p.id).unwrap();
    r.close().unwrap();
    dir
}

/// Every file and directory below `root`, as paths relative to it.
fn tree(root: &Path) -> BTreeSet<PathBuf> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeSet<PathBuf>) {
        for e in std::fs::read_dir(at).unwrap() {
            let p = e.unwrap().path();
            out.insert(p.strip_prefix(root).unwrap().to_path_buf());
            if p.is_dir() {
                walk(root, &p, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

/// Opens the project through the adapter, captures a snapshot, reads and searches through it,
/// and closes the holder. The work another program does in its own process.
fn use_adapter(project: &Path) {
    let doc = std::fs::read_to_string(project.join("DOC_ID")).unwrap();
    let handle = StoreHandle::open(project).unwrap();
    let snap = handle.snapshot().unwrap();
    let read = snap
        .read(ReadRequest {
            id: doc,
            ..Default::default()
        })
        .unwrap();
    assert!(read.to_string().contains("ledger"), "{read}");
    let found: Value = snap
        .search(SearchRequest {
            query: "ledger".into(),
            ignore_case: true,
            ..Default::default()
        })
        .unwrap();
    assert!(found.to_string().contains("ledger"), "{found}");
    // a second snapshot of the open holder is the same work again
    drop(handle.snapshot().unwrap());
    drop(snap);
    handle.close().unwrap();
}

/// The body of the child run: acts only when the parent named a project.
#[test]
fn mht9_adapter_child() {
    let Some(project) = std::env::var_os(CHILD_PROJECT) else {
        return;
    };
    use_adapter(Path::new(&project));
}

/// The variables that place a person's home, config or data, as the platforms read them.
const PERSON_VARS: [&str; 8] = [
    "HOME",
    "USERPROFILE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];

#[test]
fn mht9_adapter_open_and_snapshot_create_no_http_state() {
    let dir = project();
    let before = tree(dir.path());
    for name in &before {
        let text = name.to_string_lossy();
        assert!(
            !text.contains("mcp-token") && !text.contains("daemon.pid") && !text.contains(".sock"),
            "guard: the fixture starts with no run state: {text}"
        );
    }
    assert!(!dir.path().join(".rdy/run").exists());

    // in this process: no run directory, token, pid or socket file appears
    use_adapter(dir.path());
    let after = tree(dir.path());
    for name in after.difference(&before) {
        let text = name.to_string_lossy().to_string();
        assert!(
            !text.contains("run") && !text.contains("token") && !text.contains("pid"),
            "the adapter left run state behind: {text}"
        );
    }
    assert!(!dir.path().join(".rdy/run").exists(), "no run directory");

    // in a child whose every home variable names a place that does not exist: a lookup that
    // creates or reads a person layer would make the place, or fail
    let nowhere = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env(CHILD_PROJECT, dir.path());
    let mut places = Vec::new();
    for var in PERSON_VARS {
        let place = nowhere.path().join(var.to_lowercase());
        cmd.env(var, &place);
        places.push(place);
    }
    cmd.env_remove("HOMEDRIVE").env_remove("RDY_ACTOR");
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("test result: ok. 1 passed"),
        "the child failed: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    for place in &places {
        assert!(!place.exists(), "the adapter made {}", place.display());
    }
    assert!(
        std::fs::read_dir(nowhere.path()).unwrap().next().is_none(),
        "the adapter wrote below the home it was given"
    );
    assert!(
        !dir.path().join(".rdy/run").exists(),
        "the child made no run directory"
    );
}

/// The packages `root` needs, by name, from the workspace lock file.
fn closure(lock: &str, root: &str) -> BTreeSet<String> {
    let mut deps: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut name = String::new();
    let mut in_deps = false;
    for line in lock.lines() {
        if let Some(n) = line.strip_prefix("name = \"") {
            name = n.trim_end_matches('"').to_string();
            deps.entry(name.clone()).or_default();
            in_deps = false;
        } else if line.starts_with("dependencies = [") {
            in_deps = true;
        } else if in_deps && line.trim() == "]" {
            in_deps = false;
        } else if in_deps {
            let dep = line.trim().trim_end_matches(',').trim_matches('"');
            let dep = dep.split(' ').next().unwrap_or(dep);
            deps.get_mut(&name).unwrap().push(dep.to_string());
        }
    }
    let mut seen = BTreeSet::new();
    let mut todo = vec![root.to_string()];
    while let Some(p) = todo.pop() {
        if seen.insert(p.clone()) {
            todo.extend(deps.get(&p).cloned().unwrap_or_default());
        }
    }
    seen
}

#[test]
fn mht9_adapter_gains_no_daemon_or_http_dependency() {
    let manifest = include_str!("../Cargo.toml");
    let lock = include_str!("../../../Cargo.lock");
    let all = closure(lock, "rdy-adapter");
    assert!(
        all.contains("rdy-engine") && all.contains("rdy-store"),
        "guard: {all:?}"
    );
    for banned in [
        "rdy-daemon",
        "rdy-mcp",
        "rdy-cli",
        "rdy-site",
        "rmcp",
        "hyper",
        "hyper-util",
        "tower-service",
        "tokio",
    ] {
        assert!(
            !manifest.contains(&format!("{banned}.workspace"))
                && !manifest.contains(&format!("{banned} =")),
            "the adapter manifest names {banned}"
        );
        assert!(
            !all.contains(banned),
            "the adapter's dependency closure holds {banned}"
        );
    }
}
