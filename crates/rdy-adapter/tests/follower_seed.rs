//! A seeded follower through the adapter (D-AA4FD: "A follower is a store whose log is a
//! byte-identical prefix of its source's."): it reads as its source reads, by project or holder
//! scope rules, it reopens with no batch, and a log ahead of its mark opens for recovery only.
use rdy_adapter::maintenance::{self, ChainEnd};
use rdy_adapter::{Follower, RepoId, StoreHandle};
use rdy_engine::follower::FollowerCore;
use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest, stream_export, stream_take};
use rdy_model::{Hash, Role};
use rdy_store::take_batch::UploadTarget;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn land_tag(r: &mut Rdy, n: usize, name: &str) {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": "A", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("body {n}")}
    ]);
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    r.tag(&session(Role::Approver), name, None, None).unwrap();
}

fn segment_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}

/// A project with one tag; its full dump; a follower seeded from it, not open.
struct Seeded {
    base: tempfile::TempDir,
    source: PathBuf,
    follower: PathBuf,
    end: ChainEnd,
}

fn seeded() -> Seeded {
    let base = tempfile::tempdir().unwrap();
    let source = base.path().join("p");
    let mut r = Rdy::init(&source, "rdy-standard").unwrap();
    land_tag(&mut r, 0, "v1.0.0");
    let dump = base.path().join("p.rdydump");
    r.read().dump(&dump).unwrap();
    r.close().unwrap();
    let follower = base.path().join("f");
    let end = maintenance::seed_follower(&dump, &follower).unwrap();
    Seeded {
        base,
        source,
        follower,
        end,
    }
}

fn triple(e: &rdy_adapter::RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

#[test]
fn mh6_project_and_holder_snapshots_keep_their_scope_rules() {
    // a project: identity, canon, tags and landing facts equal the source's
    let s = seeded();
    let src = StoreHandle::open(&s.source).unwrap();
    let want = src.snapshot().unwrap();
    let f = Follower::open(&s.follower).unwrap();
    assert_eq!(f.end().unwrap(), s.end.end);
    let got = f.snapshot().unwrap();
    assert_eq!(got.identity(), want.identity());
    assert_eq!(got.canon(), want.canon());
    assert_eq!(got.tags().unwrap(), want.tags().unwrap());
    assert_eq!(got.canon_chain().unwrap(), want.canon_chain().unwrap());
    assert_eq!(
        got.tag_history("1.0.0").unwrap(),
        want.tag_history("1.0.0").unwrap()
    );
    drop((got, f));
    // a project follower reads canon and views as a project does
    let core = FollowerCore::open(&s.follower).unwrap();
    let state = core.read().unwrap();
    assert!(state.materialized(&Scope::Canon).is_ok());
    assert!(state.materialized(&Scope::View("work".into())).is_ok());
    core.fsck().unwrap();
    drop((state, core));
    src.close().unwrap();

    // a holder: tag and landing reads only
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    land_tag(&mut r, 0, "v1.0.0");
    let repo: RepoId = r.store().repo_id().unwrap().unwrap();
    let (_, t, _) = r.store().tags().unwrap().remove(0);
    let landing: Hash = r.store().read_tag(&t).unwrap().landing;
    let mut batch = Vec::new();
    stream_export::write(&r.read(), t, &BTreeSet::new(), &mut batch).unwrap();
    r.close().unwrap();
    let file = d.path().join("v1.rdysync");
    std::fs::write(&file, &batch).unwrap();
    let hdir = d.path().join("h");
    let mut h = Rdy::create_holder(&hdir, repo).unwrap();
    stream_take::take(
        &mut h,
        &[std::fs::File::open(&file).unwrap()],
        UploadTarget {
            tag: "v1.0.0".into(),
            landing,
        },
    )
    .unwrap();
    let dump = d.path().join("h.rdydump");
    h.read().dump(&dump).unwrap();
    h.close().unwrap();
    let hf = d.path().join("hf");
    maintenance::seed_follower(&dump, &hf).unwrap();
    // the holder's mark lists no file, and no project file is written
    assert!(!hf.join(".rdy.yaml").exists());
    let mark = rdy_store::follower::read_mark(&hf.join(".rdy/store"))
        .unwrap()
        .unwrap();
    assert!(mark.files.is_empty());
    let src = StoreHandle::open_holder(&hdir).unwrap();
    let want = src.snapshot().unwrap();
    let f = Follower::open(&hf).unwrap();
    let got = f.snapshot().unwrap();
    assert_eq!(got.identity(), want.identity());
    assert_eq!(got.tags().unwrap(), want.tags().unwrap());
    assert_eq!(
        got.tag_history("1.0.0").unwrap(),
        want.tag_history("1.0.0").unwrap()
    );
    assert_eq!(
        got.landing_facts(landing).unwrap(),
        want.landing_facts(landing).unwrap()
    );
    drop((got, f));
    let core = FollowerCore::open(&hf).unwrap();
    let state = core.read().unwrap();
    let tag = state.resolve_scope("tag:1.0.0").unwrap();
    assert!(state.materialized(&tag).is_ok());
    assert!(state.materialized(&Scope::At(landing)).is_ok());
    for scope in [
        Scope::Canon,
        Scope::View("work".into()),
        Scope::Inflight,
        Scope::Proposal(Hash([5; 32])),
    ] {
        let e = state.materialized(&scope).err().expect("refused");
        assert_eq!(
            triple(&e),
            (
                "E_BAD_INPUT",
                "holder reads need a tag or landing",
                "name tag:<version> or landing:<hash>"
            ),
            "{scope:?}"
        );
    }
    drop((state, core));
    src.close().unwrap();

    // the adapter's follower exposes no engine handle
    let source = include_str!("../src/follower.rs");
    for raw in [
        "-> Rdy",
        "-> Store",
        "-> FollowerCore",
        "pub core",
        "pub inner",
    ] {
        assert!(!source.contains(raw), "{raw}");
    }
}

#[test]
fn mh6_reopen_rebuilds_index_without_a_source_batch() {
    let s = seeded();
    let log = segment_files(&s.follower);
    let dump_of = |name: &str| {
        let core = FollowerCore::open(&s.follower).unwrap();
        let out = s.base.path().join(name);
        let summary = core.read().unwrap().dump(&out).unwrap();
        assert_eq!(summary.chain_end().end, s.end.end);
        std::fs::read(out).unwrap()
    };
    let first = dump_of("a.rdydump");
    std::fs::remove_file(s.follower.join(".rdy/store/index.redb")).unwrap();
    let f = Follower::open(&s.follower).unwrap();
    assert_eq!(f.end().unwrap(), s.end.end);
    assert!(f.snapshot().unwrap().canon().is_some());
    drop(f);
    assert!(s.follower.join(".rdy/store/index.redb").exists());
    assert_eq!(segment_files(&s.follower), log, "no batch");
    assert_eq!(dump_of("b.rdydump"), first);
    let e = StoreHandle::open(&s.follower).err().unwrap();
    assert_eq!(e.code, "E_STORE_FOLLOWER");
}

#[test]
fn mh6_ahead_log_opens_only_for_recovery() {
    let s = seeded();
    // the source writes on, and its whole later log reaches the follower: a complete suffix
    let mut r = Rdy::open(&s.source).unwrap();
    land_tag(&mut r, 1, "v2.0.0");
    let source_end = r.store().log_end();
    r.close().unwrap();
    let log = segment_files(&s.source);
    for (name, bytes) in &log {
        std::fs::write(s.follower.join(".rdy/store/segments").join(name), bytes).unwrap();
    }
    let f = Follower::open(&s.follower).unwrap();
    assert_eq!(f.end().unwrap(), source_end, "the actual end");
    let want = (
        "E_DUMP_CHAIN",
        "the follower has an unfinished increment",
        "retry the same increment before reading the follower",
    );
    let e = f.snapshot().err().expect("no read");
    assert_eq!(triple(&e), want);
    drop(f);
    let core = FollowerCore::open(&s.follower).unwrap();
    assert_eq!(triple(&core.read().err().unwrap()), want);
    assert_eq!(triple(&core.fsck().err().unwrap()), want);
    drop(core);
    assert_eq!(segment_files(&s.follower), log, "the suffix stays");

    // a zeroed payload no apply authorizes: the corruption's code and message, the seed's fix
    let s = seeded();
    let seg = s.follower.join(".rdy/store/segments/seg-000000.log");
    let mut bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    bytes[37..37 + len].fill(0);
    std::fs::write(&seg, &bytes).unwrap();
    let e = Follower::open(&s.follower).err().expect("refused");
    assert_eq!(e.code, "E_REDACTION_CORRUPT");
    assert_eq!(
        e.message,
        "The redaction record does not authorize the missing payload at 0:0."
    );
    assert_eq!(
        e.fix,
        "seed a new follower from a full dump of the source store"
    );
    // the same damage on an ordinary store keeps the global fix
    let copy = tempfile::tempdir().unwrap();
    let mut src = std::fs::read(s.source.join(".rdy/store/segments/seg-000000.log")).unwrap();
    src[37..37 + len].fill(0);
    let store = copy.path().join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    std::fs::write(store.join("segments/seg-000000.log"), &src).unwrap();
    std::fs::copy(s.source.join(".rdy/store/FEATURES"), store.join("FEATURES")).unwrap();
    std::fs::copy(s.source.join(".rdy/store/FORMAT"), store.join("FORMAT")).unwrap();
    let e: rdy_adapter::RdyError =
        rdy_store::Store::open_with_policy(&store, rdy_store::OpenPolicy::RequireCurrent)
            .err()
            .expect("refused")
            .into();
    assert_eq!(e.code, "E_REDACTION_CORRUPT");
    assert_eq!(
        e.fix,
        "Keep the store unchanged and inspect it with rdy fsck."
    );
}
