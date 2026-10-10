//! eng1: one door for reads and writes, the serve config, pack reload and the open report.
use rdy_engine::engine::{Engine, Local, write_typed};
use rdy_engine::{
    Rdy, ReloadOutcome, Scope, ServeConfig, Session, SessionKind, WriteRequest, serve_config,
};
use rdy_model::Role;
use std::sync::Arc;
use std::time::Duration;

fn assert_send_sync<T: Send + Sync>() {}

fn writer() -> Session {
    Session {
        actor: "human:t".into(),
        run: Some("cli".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn create_doc(r: &mut Rdy, view: &str, name: &str) -> rdy_engine::WriteResult {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}
    ]))
    .unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

#[test]
fn eng1_engine_is_object_safe_and_local_is_send_sync() {
    assert_send_sync::<Local>();
    let (_d, r) = project();
    let e: Arc<dyn Engine> = Arc::new(Local::new(r));
    assert!(e.read().view_names().is_empty());
}

#[test]
fn eng1_a_read_after_a_write_through_local_sees_it() {
    let (_d, r) = project();
    let e = Local::new(r);
    let v = write_typed(&e, |r| Ok(create_doc(r, "work", "a"))).unwrap();
    assert!(v["change"].is_string(), "{v}");
    let st = e.read();
    assert_eq!(st.view_names(), vec!["work".to_string()]);
    assert_eq!(st.included("work").len(), 1);
}

#[test]
fn eng1_a_failed_write_through_local_leaves_the_engine_usable() {
    let (_d, r) = project();
    let e = Local::new(r);
    let err = e
        .write(Box::new(|r| {
            let ops =
                serde_json::from_value(serde_json::json!([{"op": "delete", "id": "b-NOPE1"}]))
                    .unwrap();
            r.write(
                &writer(),
                WriteRequest {
                    view: "work".into(),
                    ops,
                    justification: None,
                    message: String::new(),
                },
            )
            .map(|_| serde_json::Value::Null)
        }))
        .unwrap_err();
    assert_ne!(err.code, rdy_engine::E_INTERNAL);
    write_typed(&e, |r| Ok(create_doc(r, "work", "a"))).unwrap();
    assert_eq!(e.read().included("work").len(), 1);
}

#[test]
fn eng1_open_report_and_root_are_available_through_the_door() {
    let (d, r) = project();
    let e = Local::new(r);
    assert_eq!(e.root(), d.path());
    let rep = e.open_report();
    assert!(!rep.index_rebuilt);
    assert!(rep.quarantined.is_empty());
}

#[test]
fn eng1_serve_config_defaults_and_reads_the_serve_section() {
    let (d, r) = project();
    assert_eq!(r.serve_config(), ServeConfig::DEFAULT);
    assert_eq!(ServeConfig::DEFAULT.start_timeout, Duration::from_secs(30));
    assert_eq!(ServeConfig::DEFAULT.idle_timeout, Duration::from_secs(600));
    drop(r);
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\nserve:\n  start_timeout: 5\n  idle_timeout: 7\n",
    )
    .unwrap();
    let c = serve_config(d.path());
    assert_eq!(c.start_timeout, Duration::from_secs(5));
    assert_eq!(c.idle_timeout, Duration::from_secs(7));
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.serve_config(), c);
    drop(r);
    // a broken config still yields defaults for the client that has to start the daemon
    std::fs::write(d.path().join(".rdy.yaml"), "pack: [\n").unwrap();
    assert_eq!(serve_config(d.path()), ServeConfig::DEFAULT);
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\nserve:\n  idle_timeout: 9\n",
    )
    .unwrap();
    assert_eq!(serve_config(d.path()).idle_timeout, Duration::from_secs(9));
    assert_eq!(
        serve_config(d.path()).start_timeout,
        Duration::from_secs(30)
    );
}

const PACK_A: &str = "name: proj\nversion: '1'\nextends: rdy-standard\n";
const PACK_BAD: &str = "name: proj\nversion: '1'\nextends: no-such-pack\n";

#[test]
fn eng1_reload_pack_is_unchanged_reloaded_or_invalid_and_keeps_the_old_pack_on_invalid() {
    let (d, mut r) = project();
    let fp0 = r.pack_fingerprint().clone();
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Unchanged));
    // a project pack appears and the config points at it
    std::fs::write(d.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: proj\n").unwrap();
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    assert_eq!(r.pack().name, "proj");
    assert_eq!(
        r.read().pack().name,
        "proj",
        "the published state carries the new pack"
    );
    assert_ne!(r.pack_fingerprint(), &fp0);
    let fp1 = r.pack_fingerprint().clone();
    // an invalid edit keeps the old pack and the old fingerprint, so the fix is seen as a change
    std::fs::write(d.path().join(".rdy/packs/proj.yaml"), PACK_BAD).unwrap();
    let out = r.reload_pack().unwrap();
    let ReloadOutcome::Invalid { error: e, .. } = out else {
        panic!("{out:?}")
    };
    assert!(e.message.contains("no-such-pack"), "{e}");
    assert_eq!(r.pack().name, "proj");
    assert_eq!(r.pack_fingerprint(), &fp1);
    assert!(
        matches!(r.reload_pack().unwrap(), ReloadOutcome::Invalid { .. }),
        "still invalid, still reported"
    );
    std::fs::write(d.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
    // same bytes as before the bad edit: unchanged against the kept fingerprint
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Unchanged));
    assert!(r.read().materialized(&Scope::Canon).is_ok());
}

#[test]
fn eng1_an_invalid_reload_carries_the_fingerprint_of_the_bytes_it_evaluated() {
    let (d, mut r) = project();
    let loaded = r.pack_fingerprint().clone();
    std::fs::write(d.path().join(".rdy/packs/proj.yaml"), PACK_BAD).unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: proj\n").unwrap();
    let on_disk = rdy_engine::pack_fingerprint(d.path()).unwrap();
    let out = r.reload_pack().unwrap();
    let ReloadOutcome::Invalid { error, fingerprint } = out else {
        panic!("{out:?}")
    };
    assert!(error.message.contains("no-such-pack"), "{error}");
    assert_eq!(fingerprint.hash, on_disk.hash, "the bytes the reload read");
    assert_eq!(
        r.pack_fingerprint(),
        &loaded,
        "the loaded pack's fingerprint stays"
    );
}

#[test]
fn eng1_the_fingerprint_hashes_contents_not_only_mtimes() {
    let (d, r) = project();
    let fp0 = rdy_engine::pack_fingerprint(d.path()).unwrap();
    let cfg = d.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, &text).unwrap();
    let fp1 = rdy_engine::pack_fingerprint(d.path()).unwrap();
    assert_eq!(
        fp0.hash, fp1.hash,
        "a rewrite of the same bytes keeps the hash"
    );
    std::fs::write(&cfg, format!("{text}# c\n")).unwrap();
    assert_ne!(
        fp0.hash,
        rdy_engine::pack_fingerprint(d.path()).unwrap().hash
    );
    drop(r);
}

#[test]
fn eng1_a_write_that_panics_leaves_local_republishing_the_store_on_the_next_lock() {
    use rdy_model::{Block, BlockId, Change, ChangeLinks, FieldValue, Op};
    let (_d, r) = project();
    let e = Local::new(r);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = e.write(Box::new(|r| {
            // a store write outside the engine's publish, then a panic before the republish:
            // the read state is left behind the store, as a write that panics midway leaves it
            let mut fields = std::collections::BTreeMap::new();
            fields.insert("title".to_string(), FieldValue::Str("Half".into()));
            fields.insert("doc_type".to_string(), FieldValue::Str("glossary".into()));
            let block = Block {
                id: BlockId::parse("d-AAAAB").unwrap(),
                block_type: "doc".into(),
                parent: None,
                order: "V".into(),
                fields,
                body: String::new(),
                links: Default::default(),
                pins: Default::default(),
            };
            r.store_mut()
                .commit_to_view(
                    "work",
                    Change {
                        view: "work".into(),
                        ops: vec![Op::Create { block }],
                        deps: vec![],
                        provenance: writer().provenance(),
                        justification: None,
                        message: "half".into(),
                        links: ChangeLinks::default(),
                    },
                )
                .unwrap();
            panic!("a write panics after touching the store");
        }));
    }));
    assert!(panicked.is_err());
    assert_eq!(
        e.read().view_names(),
        vec!["work".to_string()],
        "the poisoned lock is recovered with an invalidate, so the read sees the store"
    );
    // the poison is cleared: later writes go through as usual
    write_typed(&e, |r| Ok(create_doc(r, "work", "b"))).unwrap();
    assert_eq!(e.read().included("work").len(), 2);
    e.close().unwrap();
}
