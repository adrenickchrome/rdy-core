//! scc2 (D-3RSCW, D-WWAK7): a project's sync state. The `meta/sync/<remote>/` refs hold closed
//! postcard values; only the store writes them, and the first one activates the incompatible
//! feature `sync_state` in its own batch. Progress batches compare their own push. Dump and load
//! keep the refs, a clone keeps canon and view states, a fork keeps none. The unsent table is
//! derived from canon, the views and the refs, so a reindex rebuilds it.
use crate::carried_fixture;

use carried_fixture::copy_store;
use rdy_model::*;
use rdy_store::format::{
    FEATURE_SYNC_STATE, FeatureClass, REF_STORE_FEATURES, check_features, older_reader_for_tests,
    read_format_file,
};
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::meta::Origin;
use rdy_store::segment::Location;
use rdy_store::sync_state::{
    KIND_CANON, KIND_VIEW, MAX_VALUE_BYTES, SYNC_LOCKED_FIX, SyncCanon, SyncHeld, SyncPending,
    SyncRefName, SyncStep, SyncUpload, SyncValue, SyncView, canon_ref, check_ref, pending_ref,
    sync_locked_message, upload_ref, view_ref,
};
use rdy_store::{LandRequest, Store, StoreError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The writer's clock of every sync state value here (injected).
const AT: i64 = 1_700_000_000_123;
const URL: &str = "https://127.0.0.1:9/r/demo";

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: if role == Role::Approver {
            "human:a".into()
        } else {
            "agent:w".into()
        },
        run: None,
        role,
        at_ms: 1,
    }
}

fn block(n: u32) -> Block {
    Block {
        id: BlockId::parse(&format!("b-{n:05}")).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: format!("claim {n}"),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn change(view: &str, n: u32) -> Change {
    Change {
        view: view.into(),
        ops: vec![Op::Create { block: block(n) }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: format!("change {n}"),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn project() -> (tempfile::TempDir, PathBuf, Store) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("p");
    let s = Store::create(&path).unwrap();
    (d, path, s)
}

/// Lands one change that creates block `n` through the store's own landing path.
fn land(s: &mut Store, n: u32) -> Hash {
    s.commit_to_view("work", change("work", n)).unwrap();
    let p = s
        .propose("work", &format!("p{n}"), prov(Role::Writer))
        .unwrap();
    s.land_atomic(LandRequest {
        proposal: p,
        accept: None,
        approver: prov(Role::Approver),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    })
    .unwrap()
    .landing
}

fn repo(s: &Store) -> RepoId {
    s.reader().repo_id().unwrap().unwrap()
}

fn canon(r: RepoId, fetched: Option<Hash>, applied: Option<Hash>) -> SyncCanon {
    SyncCanon {
        repo_id: r,
        url: URL.into(),
        fetched,
        applied,
        held: None,
    }
}

fn upload(r: RepoId, push: &str) -> SyncUpload {
    SyncUpload {
        repo_id: r,
        url: URL.into(),
        push: push.into(),
        kind: KIND_CANON,
        view: None,
        at_ms: AT,
    }
}

fn pending(r: RepoId, push: &str, head: Hash) -> SyncPending {
    SyncPending {
        repo_id: r,
        url: URL.into(),
        push: push.into(),
        kind: KIND_CANON,
        view: None,
        expected: None,
        head,
        at_ms: AT,
    }
}

/// The ref names of the last batch of the log, in log order.
fn last_batch_refs(s: &Store) -> Vec<String> {
    let mut last = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            last = b
                .frames
                .iter()
                .filter(|f| f.kind() == kinds::REF_UPDATE)
                .map(|f| {
                    postcard::from_bytes::<RefUpdate>(f.present_payload().unwrap())
                        .unwrap()
                        .name
                })
                .collect();
            Ok(())
        })
        .unwrap();
    last
}

fn fsck_text(s: &Store) -> String {
    match s.check_objects() {
        Err(StoreError::Decode(m)) => m,
        other => panic!("{other:?}"),
    }
}

fn mirror_lists(dir: &Path) -> bool {
    read_format_file(dir)
        .unwrap()
        .is_some_and(|f| f.has(FEATURE_SYNC_STATE))
}

#[test]
fn scc2_sync_state_first_ref_feature_and_rollback_are_atomic() {
    let (_d, dir, mut s) = project();
    let l1 = land(&mut s, 1);
    let r = repo(&s);
    // a new store does not list the feature: only a first use activates it
    assert!(!s.features().has(FEATURE_SYNC_STATE));
    assert!(!mirror_lists(&dir));
    let step = || vec![SyncStep::Canon(canon(r, Some(l1), Some(l1)))];

    // a failed batch rolls back the ref and the feature with it, and FORMAT is put back
    let end = s.log_end();
    s.fail_next_index_apply();
    assert!(s.write_sync("origin", step()).is_err());
    assert_eq!(s.log_end(), end);
    assert!(!s.features().has(FEATURE_SYNC_STATE));
    assert!(!mirror_lists(&dir));
    assert_eq!(s.reader().sync_canon("origin").unwrap(), None);
    drop(s);
    let mut s = Store::open(&dir).unwrap();
    assert!(!s.features().has(FEATURE_SYNC_STATE));
    assert_eq!(s.reader().get_ref(&canon_ref("origin")).unwrap(), None);

    // a refused value writes nothing and activates nothing
    let dangling = Hash([9; 32]);
    let end = s.log_end();
    assert!(
        s.write_sync(
            "origin",
            vec![SyncStep::Canon(canon(r, Some(dangling), None))]
        )
        .is_err()
    );
    assert_eq!(s.log_end(), end);
    assert!(!s.features().has(FEATURE_SYNC_STATE));

    // the first ref and the feature are one batch
    let writes = s.write_count();
    s.write_sync("origin", step()).unwrap();
    assert_eq!(s.write_count(), writes + 1, "one batch");
    assert_eq!(
        last_batch_refs(&s),
        [canon_ref("origin"), REF_STORE_FEATURES.to_string()]
    );
    let f = s.features().clone();
    let entry = f
        .features
        .iter()
        .find(|f| f.name == FEATURE_SYNC_STATE)
        .unwrap();
    assert_eq!(entry.class, FeatureClass::Incompatible);
    assert_eq!(entry.required_rdy, "0.53.0");
    assert!(mirror_lists(&dir));
    assert_eq!(
        s.reader().sync_canon("origin").unwrap(),
        Some(canon(r, Some(l1), Some(l1)))
    );
    // a later write activates nothing more
    s.write_sync("backup", step()).unwrap();
    assert_eq!(last_batch_refs(&s), [canon_ref("backup")]);
    s.check_objects().unwrap();
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert!(s.features().has(FEATURE_SYNC_STATE));
    assert!(s.reader().sync_canon("origin").unwrap().is_some());
}

#[test]
fn scc2_older_reader_refuses_sync_state_with_required_version() {
    let (_d, dir, mut s) = project();
    let l1 = land(&mut s, 1);
    let r = repo(&s);
    // a store with no sync state opens in an older rdy
    drop(s);
    {
        let _older = older_reader_for_tests(&[FEATURE_SYNC_STATE]);
        drop(Store::open(&dir).unwrap());
    }
    let mut s = Store::open(&dir).unwrap();
    s.write_sync("origin", vec![SyncStep::Canon(canon(r, Some(l1), None))])
        .unwrap();
    let manifest = s.features().clone();
    drop(s);
    let _older = older_reader_for_tests(&[FEATURE_SYNC_STATE]);
    let want = |e: StoreError| match e {
        StoreError::Format { message, fix } => {
            assert_eq!(message, "store feature sync_state needs rdy 0.53.0");
            assert_eq!(fix, "open the project with rdy 0.53.0 or later");
        }
        other => panic!("{other:?}"),
    };
    want(check_features(&manifest).unwrap_err());
    want(
        Store::open(&dir)
            .err()
            .expect("an older rdy refuses the store"),
    );
    drop(_older);
    // this rdy knows it
    check_features(&manifest).unwrap();
    Store::open(&dir).unwrap().check_objects().unwrap();
}

#[test]
fn scc2_sync_meta_shapes_bounds_targets_and_secrets_are_closed() {
    // names: a remote as `rdy remote` names it, then canon, upload, pending or view/<name>
    let long_view = "v".repeat(256);
    for (name, ok) in [
        ("meta/sync/origin/canon", true),
        ("meta/sync/origin/upload", true),
        ("meta/sync/origin/pending", true),
        ("meta/sync/origin/view/work", true),
        ("meta/sync/o.r_i-g1n/canon", true),
        (&format!("meta/sync/{}/canon", "r".repeat(64)), true),
        (&format!("meta/sync/origin/view/{long_view}"), true),
        ("meta/sync/origin/view/a/b", true),
        ("meta/sync/", false),
        ("meta/sync/origin", false),
        ("meta/sync/origin/", false),
        ("meta/sync/origin/other", false),
        ("meta/sync/origin/canon/x", false),
        ("meta/sync/origin/view/", false),
        ("meta/sync/-origin/canon", false),
        ("meta/sync/ori gin/canon", false),
        ("meta/sync//canon", false),
        (&format!("meta/sync/{}/canon", "r".repeat(65)), false),
        (&format!("meta/sync/origin/view/{long_view}v"), false),
    ] {
        assert_eq!(SyncRefName::parse(name).is_some(), ok, "{name}");
    }
    assert_eq!(
        check_ref("meta/sync/origin/other", b"x").unwrap_err(),
        "ref meta/sync/origin/other: not a sync ref name"
    );

    let (_d, _dir, mut s) = project();
    let l1 = land(&mut s, 1);
    let l2 = land(&mut s, 2);
    let r = repo(&s);
    let op = s.reader().view_head("work").unwrap().unwrap().head;
    let held = SyncHeld {
        head: l2,
        request: u64::MAX,
    };
    // every closed value round-trips with every field, null cases included
    let values = [
        (
            canon_ref("origin"),
            SyncValue::Canon(SyncCanon {
                held: Some(held.clone()),
                ..canon(r, Some(l2), Some(l1))
            }),
        ),
        (canon_ref("origin"), SyncValue::Canon(canon(r, None, None))),
        (
            view_ref("origin", "work"),
            SyncValue::View(SyncView {
                repo_id: r,
                url: URL.into(),
                root: op,
                fetched: Some(op),
                applied: None,
            }),
        ),
        (upload_ref("origin"), SyncValue::Upload(upload(r, "p-1"))),
        (
            upload_ref("origin"),
            SyncValue::Upload(SyncUpload {
                kind: KIND_VIEW,
                view: Some("work".into()),
                ..upload(r, "P_9")
            }),
        ),
        (
            pending_ref("origin"),
            SyncValue::Pending(SyncPending {
                expected: Some(l1),
                ..pending(r, "p-1", l2)
            }),
        ),
        (
            pending_ref("origin"),
            SyncValue::Pending(SyncPending {
                kind: KIND_VIEW,
                view: Some("work".into()),
                ..pending(r, "p-1", op)
            }),
        ),
    ];
    for (name, v) in &values {
        let bytes = v.encode();
        assert!(bytes.len() <= MAX_VALUE_BYTES);
        assert_eq!(check_ref(name, &bytes).unwrap().as_ref(), Some(v), "{name}");
        // trailing bytes are not the closed value
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(
            check_ref(name, &longer).unwrap_err(),
            format!("ref {name}: not a sync value")
        );
    }
    // the injected clock is kept exactly
    let SyncValue::Upload(u) = check_ref(&upload_ref("o"), &values[3].1.encode())
        .unwrap()
        .unwrap()
    else {
        panic!("an upload")
    };
    assert_eq!(u.at_ms, AT);
    // an empty value is a cleared ref
    assert_eq!(check_ref(&canon_ref("origin"), &[]).unwrap(), None);
    // the 8,192-byte bound comes before the decode
    let name = canon_ref("origin");
    assert_eq!(
        check_ref(&name, &vec![0xff; MAX_VALUE_BYTES]).unwrap_err(),
        format!("ref {name}: not a sync value")
    );
    assert_eq!(
        check_ref(&name, &vec![0xff; MAX_VALUE_BYTES + 1]).unwrap_err(),
        format!("ref {name}: value exceeds 8192 bytes")
    );
    // a 4,096-byte URL fits; one byte more does not
    let mut big = canon(r, Some(l2), Some(l1));
    big.url = format!("https://127.0.0.1/{}", "a".repeat(4096 - 18));
    assert_eq!(big.url.len(), 4096);
    let bytes = SyncValue::Canon(big.clone()).encode();
    assert!(bytes.len() <= MAX_VALUE_BYTES);
    check_ref(&name, &bytes).unwrap().unwrap();
    big.url.push('a');
    assert_eq!(
        check_ref(&name, &SyncValue::Canon(big).encode()).unwrap_err(),
        format!("ref {name}: url exceeds 4096 bytes")
    );
    // no credential, query, fragment or plain http rides in a URL
    for (url, why) in [
        ("http://127.0.0.1/r", "url is not https"),
        ("https://", "url has no host"),
        (
            "https://user:secret@127.0.0.1/r",
            "url holds more than a canonical hub url",
        ),
        (
            "https://127.0.0.1/r?token=secret",
            "url holds more than a canonical hub url",
        ),
        (
            "https://127.0.0.1/r#secret",
            "url holds more than a canonical hub url",
        ),
    ] {
        let v = SyncValue::Canon(SyncCanon {
            url: url.into(),
            ..canon(r, None, None)
        });
        assert_eq!(
            check_ref(&name, &v.encode()).unwrap_err(),
            format!("ref {name}: {why}"),
            "{url}"
        );
    }
    // the push id and the kind
    let up = upload_ref("origin");
    for (v, why) in [
        (upload(r, ""), "invalid push id"),
        (upload(r, &"p".repeat(65)), "invalid push id"),
        (upload(r, "p/1"), "invalid push id"),
        (
            SyncUpload {
                kind: 2,
                ..upload(r, "p")
            },
            "unknown write kind",
        ),
        (
            SyncUpload {
                view: Some("work".into()),
                ..upload(r, "p")
            },
            "write kind and view do not agree",
        ),
        (
            SyncUpload {
                kind: KIND_VIEW,
                ..upload(r, "p")
            },
            "write kind and view do not agree",
        ),
    ] {
        assert_eq!(
            check_ref(&up, &SyncValue::Upload(v.clone()).encode()).unwrap_err(),
            format!("ref {up}: {why}"),
            "{v:?}"
        );
    }
    check_ref(&up, &SyncValue::Upload(upload(r, &"p".repeat(64))).encode())
        .unwrap()
        .unwrap();

    // only the store writes them: a public write and set_refs are refused
    let item = |name: &str, value: Vec<u8>| {
        (
            kinds::REF_UPDATE,
            postcard::to_allocvec(&RefUpdate {
                name: name.into(),
                value,
                at: Location {
                    segment: 0,
                    offset: 0,
                },
            })
            .unwrap(),
        )
    };
    let good = SyncValue::Canon(canon(r, Some(l1), None)).encode();
    let end = s.log_end();
    match s.write(vec![item(&name, good.clone())]) {
        Err(StoreError::Decode(m)) => assert_eq!(
            m,
            format!("ref {name:?} is project metadata: only the store writes meta/ refs")
        ),
        other => panic!("{other:?}"),
    }
    assert!(s.set_refs(vec![(name.clone(), good.clone())]).is_err());
    assert_eq!(s.log_end(), end);
    // a hash the store does not hold, or of another kind, is refused
    for v in [
        SyncValue::Canon(canon(r, Some(Hash([7; 32])), None)),
        SyncValue::Canon(canon(r, Some(op), None)),
    ] {
        let SyncValue::Canon(c) = v else {
            unreachable!()
        };
        match s.write_sync("origin", vec![SyncStep::Canon(c)]) {
            Err(StoreError::Decode(m)) => assert!(m.contains("which is not stored"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(s.log_end(), end);
    // fsck: a forged ref with no feature listed is one this rdy does not know
    s.set_refs_unchecked_for_tests(vec![(name.clone(), good.clone())])
        .unwrap();
    assert_eq!(
        fsck_text(&s),
        format!(
            "{name:?} is not a meta ref this rdy knows (meta/store_features, meta/upgrade, meta/store_format, meta/repo_id, meta/origin, meta/store_mode)"
        )
    );
    // with the feature: the value and its targets
    s.write_sync("origin", vec![SyncStep::Canon(canon(r, Some(l1), None))])
        .unwrap();
    s.check_objects().unwrap();
    s.set_refs_unchecked_for_tests(vec![(name.clone(), vec![1, 2, 3])])
        .unwrap();
    assert_eq!(fsck_text(&s), format!("ref {name}: not a sync value"));
    let dangling = Hash([7; 32]);
    s.set_refs_unchecked_for_tests(vec![(
        name.clone(),
        SyncValue::Canon(canon(r, None, Some(dangling))).encode(),
    )])
    .unwrap();
    assert_eq!(
        fsck_text(&s),
        format!("ref {name}: names landing {dangling}, which the store does not hold")
    );
    s.set_refs_unchecked_for_tests(vec![
        (name.clone(), good.clone()),
        ("meta/sync/origin/x".into(), good.clone()),
    ])
    .unwrap();
    assert_eq!(fsck_text(&s), "ref meta/sync/origin/x: not a sync ref name");

    // a holder never writes them
    let hd = tempfile::tempdir().unwrap();
    let mut h = Store::create_holder(&hd.path().join("h"), r).unwrap();
    match h.write_sync("origin", vec![SyncStep::Canon(canon(r, None, None))]) {
        Err(StoreError::Refused(m)) => assert_eq!(m, "a holder store cannot write ref meta/sync/"),
        other => panic!("{other:?}"),
    }
    match h.write(vec![item(
        &name,
        SyncValue::Canon(canon(r, None, None)).encode(),
    )]) {
        Err(StoreError::Refused(m)) => {
            assert_eq!(m, format!("a holder store cannot write ref {name}"))
        }
        other => panic!("{other:?}"),
    }
    assert!(!h.features().has(FEATURE_SYNC_STATE));
}

#[test]
fn scc2_sync_progress_batches_compare_their_own_push() {
    let (_d, _dir, mut s) = project();
    let l1 = land(&mut s, 1);
    let r = repo(&s);
    let busy = |e: StoreError| {
        assert_eq!(e.to_string(), sync_locked_message("origin"));
        assert_eq!(
            e.to_string(),
            "a push or pull with remote origin is running"
        );
        assert!(matches!(e, StoreError::SyncBusy(ref n) if n == "origin"));
    };
    assert_eq!(SYNC_LOCKED_FIX, "wait for it to end, then try again");
    // a reply or a clear with nothing to clear
    busy(
        s.write_sync("origin", vec![SyncStep::Reply { push: "a".into() }])
            .unwrap_err(),
    );
    busy(
        s.write_sync("origin", vec![SyncStep::ClearUpload { push: "a".into() }])
            .unwrap_err(),
    );
    s.write_sync("origin", vec![SyncStep::Upload(upload(r, "a"))])
        .unwrap();
    assert_eq!(
        s.reader().sync_upload("origin").unwrap(),
        Some(upload(r, "a"))
    );
    // a second sync of the same remote never clears or replaces the first's work
    let end = s.log_end();
    busy(
        s.write_sync("origin", vec![SyncStep::Upload(upload(r, "b"))])
            .unwrap_err(),
    );
    busy(
        s.write_sync("origin", vec![SyncStep::Send(pending(r, "b", l1))])
            .unwrap_err(),
    );
    busy(
        s.write_sync("origin", vec![SyncStep::ClearUpload { push: "b".into() }])
            .unwrap_err(),
    );
    assert_eq!(s.log_end(), end, "a failed compare writes nothing");
    // another remote is its own
    s.write_sync("backup", vec![SyncStep::Upload(upload(r, "b"))])
        .unwrap();
    // the send replaces upload with pending in one batch
    s.write_sync("origin", vec![SyncStep::Send(pending(r, "a", l1))])
        .unwrap();
    assert_eq!(
        last_batch_refs(&s),
        [upload_ref("origin"), pending_ref("origin")]
    );
    assert_eq!(s.reader().sync_upload("origin").unwrap(), None);
    assert_eq!(
        s.reader().sync_pending("origin").unwrap(),
        Some(pending(r, "a", l1))
    );
    busy(
        s.write_sync("origin", vec![SyncStep::Send(pending(r, "a", l1))])
            .unwrap_err(),
    );
    busy(
        s.write_sync("origin", vec![SyncStep::Reply { push: "b".into() }])
            .unwrap_err(),
    );
    // the reply clears its own pending and writes applied in the same batch
    s.write_sync(
        "origin",
        vec![
            SyncStep::Reply { push: "a".into() },
            SyncStep::Canon(canon(r, Some(l1), Some(l1))),
        ],
    )
    .unwrap();
    assert_eq!(
        last_batch_refs(&s),
        [pending_ref("origin"), canon_ref("origin")]
    );
    assert_eq!(s.reader().sync_pending("origin").unwrap(), None);
    // the other remote's upload is untouched, and clears with its own push
    assert_eq!(
        s.reader().sync_upload("backup").unwrap(),
        Some(upload(r, "b"))
    );
    s.write_sync("backup", vec![SyncStep::ClearUpload { push: "b".into() }])
        .unwrap();
    assert_eq!(s.reader().sync_upload("backup").unwrap(), None);
    s.check_objects().unwrap();
}

/// The sync refs of `s` that hold a value, by name.
fn held_sync_refs(s: &Store) -> BTreeMap<String, Vec<u8>> {
    s.reader()
        .refs_with_prefix("meta/sync/")
        .unwrap()
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

#[test]
fn scc2_dump_load_clone_and_fork_keep_only_the_decided_sync_refs() {
    let (d, dir, mut s) = project();
    let l1 = land(&mut s, 1);
    let r = repo(&s);
    let op = s.reader().view_head("work").unwrap().unwrap().head;
    s.write_sync(
        "origin",
        vec![
            SyncStep::Canon(canon(r, Some(l1), Some(l1))),
            SyncStep::View {
                name: "work".into(),
                value: SyncView {
                    repo_id: r,
                    url: URL.into(),
                    root: op,
                    fetched: Some(op),
                    applied: Some(op),
                },
            },
            SyncStep::Upload(upload(r, "a")),
        ],
    )
    .unwrap();
    s.write_sync("origin", vec![SyncStep::Send(pending(r, "a", l1))])
        .unwrap();
    s.write_sync("backup", vec![SyncStep::Upload(upload(r, "b"))])
        .unwrap();
    let all = held_sync_refs(&s);
    assert_eq!(
        all.keys().cloned().collect::<Vec<_>>(),
        [
            upload_ref("backup"),
            canon_ref("origin"),
            pending_ref("origin"),
            view_ref("origin", "work"),
        ]
    );
    let manifest = s.features().clone();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    drop(s);

    // dump and load keep every ref
    let loaded = d.path().join("loaded");
    let mut i = Ingest::create(&loaded).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(&rdy_store::format::DumpStore::Features(manifest))
        .unwrap();
    i.finish().unwrap();
    let l = Store::open(&loaded).unwrap();
    assert_eq!(held_sync_refs(&l), all);
    assert!(l.features().has(FEATURE_SYNC_STATE));
    l.check_objects().unwrap();
    drop(l);

    // a clone keeps canon and view states and drops upload and pending in its origin batch
    let cloned = d.path().join("clone");
    copy_store(&dir, &cloned);
    let mut c = Store::open(&cloned).unwrap();
    let writes = c.write_count();
    c.put_origin(
        &Origin {
            fork: false,
            repo_id: r,
            tag: None,
            landing: Some(l1),
        },
        None,
    )
    .unwrap();
    assert_eq!(c.write_count(), writes + 1, "one origin batch");
    let kept: BTreeMap<String, Vec<u8>> = all
        .iter()
        .filter(|(n, _)| n.ends_with("/canon") || n.contains("/view/"))
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();
    assert_eq!(held_sync_refs(&c), kept);
    assert_eq!(c.reader().sync_upload("backup").unwrap(), None);
    assert_eq!(c.reader().sync_pending("origin").unwrap(), None);
    assert!(last_batch_refs(&c).contains(&"meta/origin".to_string()));
    c.check_objects().unwrap();
    drop(c);

    // a fork drops every sync ref in its origin batch
    let forked = d.path().join("fork");
    copy_store(&dir, &forked);
    let mut f = Store::open(&forked).unwrap();
    f.put_origin(
        &Origin {
            fork: true,
            repo_id: r,
            tag: None,
            landing: Some(l1),
        },
        Some(RepoId([42; 16])),
    )
    .unwrap();
    assert!(held_sync_refs(&f).is_empty());
    assert_eq!(f.reader().sync_remotes().unwrap(), Vec::<String>::new());
    f.check_objects().unwrap();
}

#[test]
fn scc2_unsent_rebuilds_from_applied_and_held() {
    let (_d, dir, mut s) = project();
    let r = repo(&s);
    let ls: Vec<Hash> = (1..=4).map(|n| land(&mut s, n)).collect();
    // a remote with no state has no table
    assert_eq!(s.reader().sync_unsent("origin").unwrap(), None);
    // a view with three ops past its start, synced up to its second op
    s.view_create("draft", None, None, false, &prov(Role::Writer))
        .unwrap();
    for n in 10..13 {
        s.commit_to_view("draft", change("draft", n)).unwrap();
    }
    let mut ops = vec![];
    let mut at = Some(s.reader().view_head("draft").unwrap().unwrap().head);
    while let Some(h) = at {
        ops.push(h);
        at = s.reader().view_op(&h).unwrap().prev;
    }
    ops.reverse();
    // the create, a commit op for each change, and any op the store adds between them
    assert!(ops.len() >= 4, "{ops:?}");
    let synced = ops.len() - 3;
    let end = s.log_end();
    s.write_sync(
        "origin",
        vec![
            SyncStep::Canon(SyncCanon {
                held: Some(SyncHeld {
                    head: ls[2],
                    request: 7,
                }),
                ..canon(r, Some(ls[0]), Some(ls[0]))
            }),
            SyncStep::View {
                name: "draft".into(),
                value: SyncView {
                    repo_id: r,
                    url: URL.into(),
                    root: ops[0],
                    fetched: Some(ops[synced]),
                    applied: Some(ops[synced]),
                },
            },
        ],
    )
    .unwrap();
    let want = |s: &Store| {
        let u = s.reader().sync_unsent("origin").unwrap().unwrap();
        assert_eq!(u.landings, [ls[3]]);
        assert_eq!(u.held, [ls[1], ls[2]]);
        assert_eq!(
            u.views,
            BTreeMap::from([("draft".into(), ops[synced + 1..].to_vec())])
        );
        assert_eq!(
            u.counts(),
            (1, 2, BTreeMap::from([("draft".to_string(), 2)]))
        );
    };
    want(&s);
    // no log record holds the table: the state batch is its refs and the features ref alone
    let mut kinds_after = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            if b.start >= end {
                kinds_after.extend(b.frames.iter().map(|f| f.kind()));
            }
            Ok(())
        })
        .unwrap();
    assert!(!kinds_after.is_empty());
    assert!(kinds_after.iter().all(|k| *k == kinds::REF_UPDATE));
    assert!(
        s.reader()
            .refs_with_prefix("")
            .unwrap()
            .iter()
            .all(|(n, _)| !n.contains("unsent"))
    );
    // a reindex and a reopen rebuild each count
    s.reindex().unwrap();
    want(&s);
    drop(s);
    let mut s = Store::open(&dir).unwrap();
    want(&s);
    // applied null: the whole chain is unsent or held; applied at the tip leaves nothing
    s.write_sync(
        "origin",
        vec![SyncStep::Canon(SyncCanon {
            held: Some(SyncHeld {
                head: ls[0],
                request: 1,
            }),
            ..canon(r, None, None)
        })],
    )
    .unwrap();
    let u = s.reader().sync_unsent("origin").unwrap().unwrap();
    assert_eq!(u.held, [ls[0]]);
    assert_eq!(u.landings, ls[1..]);
    s.write_sync("origin", vec![SyncStep::Canon(canon(r, None, Some(ls[3])))])
        .unwrap();
    let u = s.reader().sync_unsent("origin").unwrap().unwrap();
    assert!(u.landings.is_empty() && u.held.is_empty());
    // another remote's table is its own
    assert_eq!(s.reader().sync_unsent("backup").unwrap(), None);
}
