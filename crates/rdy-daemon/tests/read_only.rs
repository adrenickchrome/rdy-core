//! Read-only projects through the ops (D-WWAK7): an rdy that does not know a read-only feature
//! of its store serves every read op, `fsck` included, through a direct engine (the CLI's
//! `--no-daemon`) and through the daemon's writer, and changes no byte of the project.
use rdy_daemon::{OpContext, Shared, dispatch};
use rdy_engine::{Engine, Local, Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use rdy_store::format::{FeatureClass, StoreAccess, inject_test_feature};
use rdy_store::kinds;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every path under `dir` with its length and contents (None for a directory).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<(u64, Vec<u8>)>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        if m.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some((m.len(), std::fs::read(&p).unwrap())));
        }
    }
    out
}

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "human:t".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

/// A project with a landed doc and a blob whose first chunk activated the read-only test
/// feature: read-only once the guard drops. Returns its base, its root and the doc's id.
fn read_only_project() -> (tempfile::TempDir, PathBuf, String) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    std::fs::create_dir(&root).unwrap();
    let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let created = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d",
                        "fields": {"title": "Landed doc", "doc_type": "page"}}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap()
        .created;
    let p = r
        .propose(
            &session(Role::Writer),
            "work",
            "Landed doc",
            Some("a summary"),
        )
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    r.store_mut()
        .put_blob(b"uses the read-only feature")
        .unwrap();
    r.seal_open().unwrap();
    r.close().unwrap();
    (base, root, created["$d"].clone())
}

fn cx<'a>(e: &'a dyn Engine, s: &'a Session, cwd: &'a Path) -> OpContext<'a> {
    OpContext {
        engine: e,
        session: s,
        human: "human:t",
        cwd,
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    }
}

/// `fsck` passes, plain and verbose, and reports the project's one landing.
fn fsck_passes(cx: &OpContext<'_>, what: &str) {
    for args in [json!({}), json!({"verbose": true})] {
        let v = dispatch(cx, "fsck", args.clone())
            .unwrap_or_else(|e| panic!("{what} {args}: fsck refused: {e:?}"));
        assert_eq!(v["landings"], 1, "{what} {args}: {v}");
    }
}

/// The read ops every engine serves, each answered.
fn reads(cx: &OpContext<'_>, id: &str, out: &Path, what: &str) {
    let ops: Vec<(&str, Value)> = vec![
        ("status", json!({})),
        ("show", json!({"id": id})),
        ("ls", json!({})),
        ("log", json!({})),
        ("tags", json!({})),
        ("block", json!({"id": id})),
        ("brief", json!({"ids": [id]})),
        ("search", json!({"query": "Landed"})),
        ("inbox", json!({})),
        (
            "dump",
            json!({"out": out.join(format!("{what}.rdydump")).display().to_string()}),
        ),
    ];
    for (op, args) in ops {
        dispatch(cx, op, args.clone())
            .unwrap_or_else(|e| panic!("{what}: {op} {args} refused: {e:?}"));
    }
}

#[test]
fn ok9_read_only_fsck_and_reads_run_through_the_ops() {
    let (_base, root, id) = read_only_project();
    let before = tree(&root);
    let out = tempfile::tempdir().unwrap();
    let s = session(Role::Writer);
    // a direct engine: the CLI's --no-daemon
    let local = Local::open(&root).unwrap();
    assert_eq!(
        local.inspect(|r| Ok(r.store().access())).unwrap(),
        StoreAccess::ReadOnly
    );
    let c = cx(&local, &s, &root);
    fsck_passes(&c, "direct");
    reads(&c, &id, out.path(), "direct");
    local.close().unwrap();
    assert_eq!(tree(&root), before, "direct: no project byte changed");
    // the daemon's writer, with its render and export
    let shared = Shared::new(Rdy::open(&root).unwrap());
    let e = shared.engine();
    let c = cx(&*e, &s, &root);
    fsck_passes(&c, "daemon");
    reads(&c, &id, out.path(), "daemon");
    for (op, args) in [
        (
            "render",
            json!({"out": out.path().join("site").display().to_string()}),
        ),
        (
            "export",
            json!({"out": out.path().join("docs").display().to_string(), "format": "md"}),
        ),
    ] {
        dispatch(&c, op, args.clone())
            .unwrap_or_else(|e| panic!("daemon: {op} {args} refused: {e:?}"));
    }
    // a write still refuses, on the writer's gate
    let e2 = dispatch(&c, "reindex", json!({})).unwrap_err();
    assert_eq!(e2.code, rdy_engine::E_STORE_READ_ONLY, "{e2:?}");
    drop(e);
    shared.shutdown().unwrap();
    assert_eq!(tree(&root), before, "daemon: no project byte changed");
}

/// `fsck {clean_quarantine: true}` by the human on a read-only project refuses with
/// E_STORE_READ_ONLY before the check runs, through a direct engine and through the daemon's
/// writer, and removes no quarantine file: the cleanup changes the source store.
#[test]
fn ok9_read_only_fsck_clean_quarantine_refuses_and_changes_no_byte() {
    let (_base, root, _id) = read_only_project();
    let store = root.join(".rdy/store");
    std::fs::write(store.join("index.redb.damaged-1"), b"a damaged index").unwrap();
    std::fs::write(store.join("segments/0000.seg.torn-1"), b"a torn tail").unwrap();
    let before = tree(&root);
    let s = session(Role::Writer);
    let clean = json!({"clean_quarantine": true});
    let refused = |c: &OpContext<'_>, what: &str| {
        let e = dispatch(c, "fsck", clean.clone()).unwrap_err();
        assert_eq!(e.code, rdy_engine::E_STORE_READ_ONLY, "{what}: {e:?}");
    };
    // a direct engine: the CLI's --no-daemon
    let local = Local::open(&root).unwrap();
    assert_eq!(
        local.inspect(|r| Ok(r.store().access())).unwrap(),
        StoreAccess::ReadOnly
    );
    let c = cx(&local, &s, &root);
    refused(&c, "direct");
    fsck_passes(&c, "direct");
    local.close().unwrap();
    assert_eq!(tree(&root), before, "direct: no project byte changed");
    // the daemon's writer
    let shared = Shared::new(Rdy::open(&root).unwrap());
    let e = shared.engine();
    let c = cx(&*e, &s, &root);
    refused(&c, "daemon");
    fsck_passes(&c, "daemon");
    drop(e);
    shared.shutdown().unwrap();
    assert_eq!(tree(&root), before, "daemon: no project byte changed");
}
