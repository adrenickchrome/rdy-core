//! Read-only projects (D-WWAK7): an rdy that does not know a read-only feature of its store
//! reads the project and writes nothing to its store. Engine writes refuse with
//! E_STORE_READ_ONLY before their closures run; publication logs no transition record; a
//! session's setup and inbox reads persist no subscription, and an explicit ack refuses.
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::{
    E_STORE_READ_ONLY, Engine, Local, Rdy, Scope, SearchQuery, Session, SessionKind, WriteRequest,
};
use rdy_model::{BlockId, Role};
use rdy_store::format::{FeatureClass, StoreAccess, inject_test_feature};
use rdy_store::kinds;
use serde_json::json;
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
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, view: &str, title: &str) -> BTreeMap<String, String> {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "id": "$d",
                    "fields": {"title": title, "doc_type": "page"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
    .created
}

/// A project with a landed doc, an open view with a second doc, and a blob whose first chunk
/// activated the read-only test feature: read-only once the guard drops.
struct Project {
    _base: tempfile::TempDir,
    root: PathBuf,
    landed: BlockId,
    drafted: BlockId,
}

fn read_only_project() -> Project {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    std::fs::create_dir(&root).unwrap();
    let _known = inject_test_feature(FeatureClass::ReadOnlyCompatible, kinds::BLOB_CHUNK);
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let landed = write(&mut r, "work", "Landed doc")["$d"].parse().unwrap();
    let p = r
        .propose(
            &session(Role::Writer),
            "work",
            "Landed doc",
            Some("a summary"),
        )
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    let drafted = write(&mut r, "draft", "Drafted doc")["$d"].parse().unwrap();
    r.store_mut()
        .put_blob(b"uses the read-only feature")
        .unwrap();
    r.seal_open().unwrap();
    assert!(
        r.store().features().has(rdy_store::format::TEST_FEATURE),
        "the log uses the feature"
    );
    r.close().unwrap();
    Project {
        _base: base,
        root,
        landed,
        drafted,
    }
}

fn assert_read_only(e: &rdy_engine::RdyError) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_STORE_READ_ONLY,
            "store feature test_feature permits reads only",
            "open the project with rdy 99.0.0 or later to write"
        ),
        "{e:?}"
    );
}

#[test]
fn ok9_read_only_reads_use_one_snapshot() {
    let p = read_only_project();
    let before = tree(&p.root);
    let out = tempfile::tempdir().unwrap();
    let r = Rdy::open(&p.root).unwrap();
    assert_eq!(r.store().access(), StoreAccess::ReadOnly);
    let st = r.read();
    // status, show and read: canon and the open view
    let canon = st.materialized(&Scope::Canon).unwrap();
    assert!(canon.get(&p.landed).is_some());
    assert!(canon.get(&p.drafted).is_none());
    let draft = st.materialized(&Scope::View("draft".into())).unwrap();
    assert!(draft.get(&p.drafted).is_some());
    // search and history
    let found = st
        .search(
            &Scope::Canon,
            &SearchQuery {
                text: "Landed".into(),
                regex: false,
                ignore_case: false,
                filters: Default::default(),
            },
            None,
            None,
        )
        .unwrap();
    assert!(found.total > 0, "{found:?}");
    st.history(&Scope::Canon, &p.landed, 10).unwrap();
    // fsck and dump: the dump is written outside the source store
    let report = r.fsck().unwrap();
    assert_eq!(report.landings, 1);
    st.dump(&out.path().join("p.rdydump")).unwrap();
    // one snapshot: every state read is the same, and the store is held for its readers
    assert_eq!(r.read().seq(), st.seq());
    let private = st.reader().private_index_dir_for_tests().unwrap();
    r.close().unwrap();
    // a captured state outlives its handle: it reads the same snapshot, and keeps the lock
    assert!(
        st.materialized(&Scope::Canon)
            .unwrap()
            .get(&p.landed)
            .is_some()
    );
    assert_eq!(Rdy::open(&p.root).err().unwrap().code, rdy_engine::E_LOCKED);
    assert!(private.exists());
    drop(canon);
    drop(draft);
    drop(st);
    assert!(
        !private.exists(),
        "the last reader removed the private index"
    );
    assert_eq!(tree(&p.root), before, "no project byte changed");
    // the next open reads the same project
    let r = Rdy::open(&p.root).unwrap();
    assert!(
        r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .get(&p.landed)
            .is_some()
    );
}

#[test]
fn ok9_every_mutation_entry_refuses_before_append() {
    let p = read_only_project();
    let before = tree(&p.root);
    let mut r = Rdy::open(&p.root).unwrap();
    let end = r.store().log_end();
    // the handle's gate, and each engine write behind it
    assert_read_only(&r.ensure_writable().unwrap_err());
    let e = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "draft".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "fields": {"title": "New", "doc_type": "page"}}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap_err();
    assert_read_only(&e);
    assert_read_only(
        &r.propose(&session(Role::Writer), "draft", "Drafted", Some("s"))
            .unwrap_err(),
    );
    assert_read_only(
        &r.tag(&session(Role::Approver), "v1.0.0", None, None)
            .unwrap_err(),
    );
    assert_read_only(&r.reindex().unwrap_err());
    // publication: the reopened reconcile logs no transition record
    assert_eq!(r.reconcile_transitions().unwrap(), 0);
    assert_eq!(r.store().log_end(), end, "nothing appended");
    r.close().unwrap();
    // Local: a write closure never runs
    let local = Local::open(&p.root).unwrap();
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = std::sync::Arc::clone(&ran);
    let e = local
        .write(Box::new(move |_| {
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }))
        .unwrap_err();
    assert_read_only(&e);
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "the closure ran"
    );
    local.close().unwrap();
    assert_eq!(tree(&p.root), before, "no project byte changed");
}

#[test]
fn ok9_read_only_session_does_not_write_acknowledgments() {
    let p = read_only_project();
    let before = tree(&p.root);
    let local = Local::open(&p.root).unwrap();
    let s = session(Role::Writer);
    // session setup: the subscription the session would have, nothing written
    let sub = subscribe(&local, &s, None).unwrap();
    assert_eq!(sub.actor, s.actor);
    assert_eq!(sub.cursor.at, local.read().seq());
    assert!(
        local
            .read()
            .reader()
            .inbox(&s.actor, None)
            .unwrap()
            .is_none()
    );
    // an inbox read with no persisted subscription: the empty started page
    let v = run_inbox(&local, &s, InboxRequest::default()).unwrap();
    assert_eq!(v["entries"], json!([]));
    assert_eq!(v["started"], json!(true));
    assert_eq!(v["more"], json!(false));
    // an explicit ack refuses, persisted subscription or not
    let e = run_inbox(
        &local,
        &s,
        InboxRequest {
            ack: true,
            ..InboxRequest::default()
        },
    )
    .unwrap_err();
    assert_read_only(&e);
    assert!(
        local
            .read()
            .reader()
            .inbox(&s.actor, None)
            .unwrap()
            .is_none()
    );
    local.close().unwrap();
    assert_eq!(tree(&p.root), before, "no project byte changed");
}
