//! The follower mark and the write gate (D-AA4FD: "The mark: a file `.rdy/store/FOLLOWER`,
//! outside the log, written as a private replace with a directory sync, holds the source's repo
//! id and the chain's last trailer digest."; "Writes: every write path refuses with
//! `E_STORE_FOLLOWER`"). A marked store never opens as a writer, every ordinary write entry
//! refuses before its fast path, and a follower's open writes no batch.
use rdy_model::*;
use rdy_store::dump_chain::{BatchMarker, ChainEnd};
use rdy_store::follower::{self, FOLLOWER_FILE, FollowerFile, FollowerMark};
use rdy_store::format::{DumpStore, StoreAccess};
use rdy_store::fsutil::{self, PrivateWriteStage};
use rdy_store::inbox::{InboxCursor, InboxSub};
use rdy_store::kinds::{self, BatchCommit};
use rdy_store::segment::{Location, SegmentReader};
use rdy_store::upgrade;
use rdy_store::{OpenPolicy, Store, StoreError, StoreLock, layout, pio};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};
const MAX_MARK: usize = 262_144;

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn by() -> Provenance {
    Provenance {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        at_ms: 10,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: by(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Every path under `dir`, with the bytes of each file (None for a directory).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if std::fs::symlink_metadata(&p).unwrap().is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

/// The store's durable record: its log, `FORMAT` and mark, not the derived index or its sentinel.
fn record(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut t = tree(dir);
    t.retain(|p, _| {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        !["index.redb", "index.open", "lock"].contains(&name.as_str())
    });
    t
}

fn segments(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    tree(&dir.join("segments"))
}

/// The marker of the batch that ends at `end` in the store at `dir`.
fn marker_at(dir: &Path, end: Location) -> BatchMarker {
    let mut out = None;
    SegmentReader::open(&dir.join("segments"))
        .for_each_from::<std::io::Error>(ORIGIN, end, |_, kind, _, payload, next| {
            if kind == kinds::BATCH_COMMIT && next == end {
                out = Some(postcard::from_bytes::<BatchCommit>(payload).unwrap());
            }
            Ok(())
        })
        .unwrap();
    BatchMarker::of_commit(&out.unwrap())
}

/// A closed store of two sealed batches; the end of its first, and its whole end.
struct Source {
    _base: tempfile::TempDir,
    dir: PathBuf,
    first: Location,
    end: Location,
    repo: RepoId,
    store: DumpStore,
}

fn source() -> Source {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join(".rdy").join("store");
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    let mut s = Store::create(&dir).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "first body"),
            }],
        ),
    )
    .unwrap();
    let first = s.log_end();
    s.put_blob(b"a second batch").unwrap();
    let end = s.log_end();
    let repo = s.repo_id().unwrap().unwrap();
    let store = DumpStore::Features(s.features().clone());
    s.close().unwrap();
    Source {
        _base: base,
        dir,
        first,
        end,
        repo,
        store,
    }
}

fn mark_at(f: &Source, end: Location) -> FollowerMark {
    FollowerMark {
        chain: ChainEnd {
            repo_id: f.repo,
            store_format: f.store.clone(),
            end,
            end_batch: marker_at(&f.dir, end),
            digest: Hash::of(b"the trailer"),
        },
        files: vec![],
    }
}

/// A store marked at its end.
fn marked() -> Source {
    let f = source();
    let lock = StoreLock::take(&f.dir).unwrap();
    follower::write_seed_mark(&f.dir, &lock, &mark_at(&f, f.end)).unwrap();
    f
}

fn mark_path(dir: &Path) -> PathBuf {
    dir.join(FOLLOWER_FILE)
}

/// Replaces the mark's bytes as a private file.
fn raw_mark(dir: &Path, bytes: &[u8]) {
    let _ = std::fs::remove_file(mark_path(dir));
    fsutil::write_private_durable(&mark_path(dir), bytes).unwrap();
}

/// The bytes of a mark of the two parts given as JSON.
fn mark_text(chain: &serde_json::Value, files: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::json!({"chain": chain, "files": files});
    format!("rdy follower 1\n{body}\n").into_bytes()
}

fn chain_json(f: &Source) -> serde_json::Value {
    serde_json::to_value(mark_at(f, f.end).chain).unwrap()
}

fn is_follower(e: StoreError, repo: RepoId, what: &str) {
    match e {
        StoreError::Follower(r) => assert_eq!(r, repo, "{what}"),
        other => panic!("{what}: not the follower refusal: {other:?}"),
    }
}

fn mark_refusal(dir: &Path) -> String {
    let e = follower::read_mark(dir).err().expect("a refused mark");
    assert!(matches!(e, StoreError::FollowerMark(_)), "{e:?}");
    e.to_string()
}

fn open_refusal(dir: &Path) -> StoreError {
    match Store::open_follower(dir) {
        Err(e) => e,
        Ok(_) => panic!("the follower opened"),
    }
}

#[test]
fn mh3_mark_is_private_bounded_and_durable() {
    let f = source();
    let before = segments(&f.dir);
    let lock = StoreLock::take(&f.dir).unwrap();
    let hash = Hash::of(b"config bytes");
    let mut mark = mark_at(&f, f.end);
    mark.files = vec![
        FollowerFile {
            path: ".rdy.yaml".into(),
            hash,
        },
        FollowerFile {
            path: ".rdyignore".into(),
            hash,
        },
    ];
    // no mark: a normal store
    assert!(follower::read_mark(&f.dir).unwrap().is_none());
    follower::refuse_follower(&f.dir).unwrap();
    follower::write_seed_mark(&f.dir, &lock, &mark).unwrap();
    // the round trip of every ChainEnd field and the inventory
    let read = follower::read_mark(&f.dir).unwrap().unwrap();
    assert_eq!(read.chain.repo_id, f.repo);
    assert_eq!(read.chain.store_format, f.store);
    assert_eq!(read.chain.end, f.end);
    assert_eq!(read.chain.end_batch, mark.chain.end_batch);
    assert_eq!(read.chain.digest, mark.chain.digest);
    assert_eq!(read.files, mark.files);
    assert_eq!(segments(&f.dir), before, "no log frame or ref");
    // private, one compact JSON line after the header, with its keys in struct order
    let meta = std::fs::symlink_metadata(mark_path(&f.dir)).unwrap();
    assert!(pio::is_private_file(&meta));
    let text = String::from_utf8(std::fs::read(mark_path(&f.dir)).unwrap()).unwrap();
    let (head, body) = text.split_once('\n').unwrap();
    assert_eq!(head, "rdy follower 1");
    assert!(body.starts_with(r#"{"chain":{"repo_id":""#), "{body}");
    assert!(body.ends_with("}]}\n"), "{body}");
    assert_eq!(body.matches('\n').count(), 1);
    let chain = body.find("\"store_format\"").unwrap();
    assert!(chain < body.find("\"end\"").unwrap());
    assert!(body.find("\"end\"").unwrap() < body.find("\"end_batch\"").unwrap());
    assert!(body.find("\"end_batch\"").unwrap() < body.find("\"digest\":\"").unwrap());
    // a marked store takes no second seed mark
    is_follower(
        follower::write_seed_mark(&f.dir, &lock, &mark).unwrap_err(),
        f.repo,
        "second seed",
    );
    // the header, the JSON and the unknown fields
    let c = chain_json(&f);
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "a bad header",
            b"rdy follower 2\n{}\n".to_vec(),
            "the follower mark does not start with \"rdy follower 1\"",
        ),
        (
            "no header",
            mark_text(&c, &serde_json::json!([]))[15..].to_vec(),
            "the follower mark does not start with \"rdy follower 1\"",
        ),
        (
            "no JSON",
            b"rdy follower 1\nnot json\n".to_vec(),
            "the follower mark is not valid JSON",
        ),
        (
            "no final newline",
            mark_text(&c, &serde_json::json!([]))
                .split_last()
                .unwrap()
                .1
                .to_vec(),
            "the follower mark is not valid JSON",
        ),
        (
            "a third line",
            [mark_text(&c, &serde_json::json!([])), b"\n".to_vec()].concat(),
            "the follower mark is not valid JSON",
        ),
        (
            "a missing part",
            b"rdy follower 1\n{\"files\":[]}\n".to_vec(),
            "the follower mark is not valid JSON",
        ),
        (
            "a top field",
            format!(
                "rdy follower 1\n{}\n",
                serde_json::json!({"chain": c, "files": [], "more": 1})
            )
            .into_bytes(),
            "the follower mark has an unknown field",
        ),
        (
            "a chain field",
            {
                let mut x = c.clone();
                x["more"] = serde_json::json!(1);
                mark_text(&x, &serde_json::json!([]))
            },
            "the follower mark has an unknown field",
        ),
        (
            "a nested field",
            {
                let mut x = c.clone();
                x["end"]["more"] = serde_json::json!(1);
                mark_text(&x, &serde_json::json!([]))
            },
            "the follower mark has an unknown field",
        ),
        (
            "a file field",
            mark_text(
                &c,
                &serde_json::json!([{"path": ".rdy.yaml", "hash": hash.to_string(), "more": 1}]),
            ),
            "the follower mark has an unknown field",
        ),
    ];
    for (what, bytes, message) in cases {
        raw_mark(&f.dir, &bytes);
        assert_eq!(mark_refusal(&f.dir), message, "{what}");
    }
    // the project file list
    let row = |p: &str| serde_json::json!({"path": p, "hash": hash.to_string()});
    let many: Vec<_> = (0..257)
        .map(|i| row(&format!(".rdy/packs/p{i:03}.yaml")))
        .collect();
    for (what, files) in [
        ("not a list", serde_json::json!({})),
        ("a bad path", serde_json::json!([row("../x")])),
        (
            "unsorted",
            serde_json::json!([row(".rdyignore"), row(".rdy.yaml")]),
        ),
        (
            "a repeated path",
            serde_json::json!([row(".rdy.yaml"), row(".rdy.yaml")]),
        ),
        (
            "an upper-case hash",
            serde_json::json!([{"path": ".rdy.yaml", "hash": hash.to_string().to_uppercase()}]),
        ),
        ("257 files", serde_json::json!(many)),
    ] {
        raw_mark(&f.dir, &mark_text(&c, &files));
        assert_eq!(
            mark_refusal(&f.dir),
            "the follower mark has an invalid project file list",
            "{what}"
        );
    }
    raw_mark(&f.dir, &mark_text(&c, &serde_json::json!(many[..256])));
    assert_eq!(
        follower::read_mark(&f.dir).unwrap().unwrap().files.len(),
        256
    );
    // JSON spaces are read, up to the exact size; one byte more is refused
    let compact = mark_text(&c, &serde_json::json!([]));
    let body = &compact[15..compact.len() - 1];
    let exact = |n: usize| {
        let mut b = b"rdy follower 1\n ".to_vec();
        b.resize(n - body.len() - 1, b' ');
        b.extend_from_slice(body);
        b.push(b'\n');
        b
    };
    raw_mark(&f.dir, &exact(MAX_MARK));
    assert_eq!(
        std::fs::metadata(mark_path(&f.dir)).unwrap().len() as usize,
        MAX_MARK
    );
    assert_eq!(
        follower::read_mark(&f.dir).unwrap().unwrap().chain.end,
        f.end
    );
    raw_mark(&f.dir, &exact(MAX_MARK + 1));
    assert_eq!(
        mark_refusal(&f.dir),
        "the follower mark exceeds 262144 bytes"
    );
    // a link, a directory and a shared file are not private regular files
    let private = "the follower mark is not a private regular file";
    raw_mark(&f.dir, &compact);
    pio::set_mode(&mark_path(&f.dir), 0o644).unwrap();
    assert_eq!(mark_refusal(&f.dir), private, "shared");
    std::fs::remove_file(mark_path(&f.dir)).unwrap();
    let target = f.dir.join("target");
    fsutil::write_private_durable(&target, &compact).unwrap();
    pio::symlink(&target, mark_path(&f.dir)).unwrap();
    assert_eq!(mark_refusal(&f.dir), private, "a link");
    std::fs::remove_file(mark_path(&f.dir)).unwrap();
    std::fs::remove_file(&target).unwrap();
    std::fs::create_dir(mark_path(&f.dir)).unwrap();
    assert_eq!(mark_refusal(&f.dir), private, "a directory");
    std::fs::remove_dir(mark_path(&f.dir)).unwrap();
    // a mark that cannot be read (a directory no one may search; root may search it)
    raw_mark(&f.dir, &compact);
    if pio::own_uid() != 0 {
        pio::set_mode(&f.dir, 0o000).unwrap();
        let read = follower::read_mark(&f.dir);
        pio::set_mode(&f.dir, 0o755).unwrap();
        assert_eq!(
            read.err().unwrap().to_string(),
            "the follower mark cannot be read"
        );
    }
    // the replace's boundaries: an old valid mark or a new valid mark, never none
    let other = {
        let mut m = mark_at(&f, f.first);
        m.chain.digest = Hash::of(b"another trailer");
        m
    };
    follower::replace_mark_for_tests(&f.dir, &mark_at(&f, f.end)).unwrap();
    for (stage, new) in [
        (PrivateWriteStage::Created, false),
        (PrivateWriteStage::Written, false),
        (PrivateWriteStage::Synced, false),
        (PrivateWriteStage::Renamed, true),
        (PrivateWriteStage::DirSynced, true),
    ] {
        follower::replace_mark_for_tests(&f.dir, &mark_at(&f, f.end)).unwrap();
        let out = fsutil::with_private_write_probe_for_tests(
            move |s, _| {
                if s == stage {
                    Err(std::io::Error::other("injected"))
                } else {
                    Ok(())
                }
            },
            || follower::replace_mark_for_tests(&f.dir, &other),
        );
        assert!(matches!(out, Err(StoreError::Io(_))), "{stage:?}");
        let now = follower::read_mark(&f.dir).unwrap().expect("a valid mark");
        let want = if new { &other } else { &mark_at(&f, f.end) };
        assert_eq!(now.chain, want.chain, "{stage:?}");
    }
    drop(lock);
    // a seed's write fails without a mark or with a whole one, at each boundary
    let f = source();
    for (stage, placed) in [
        (PrivateWriteStage::Created, false),
        (PrivateWriteStage::Written, false),
        (PrivateWriteStage::Synced, false),
        (PrivateWriteStage::Renamed, true),
        (PrivateWriteStage::DirSynced, true),
    ] {
        let _ = std::fs::remove_file(mark_path(&f.dir));
        let lock = StoreLock::take(&f.dir).unwrap();
        let m = mark_at(&f, f.end);
        let out = fsutil::with_private_write_probe_for_tests(
            move |s, _| {
                if s == stage {
                    Err(std::io::Error::other("injected"))
                } else {
                    Ok(())
                }
            },
            || follower::write_seed_mark(&f.dir, &lock, &m),
        );
        assert!(matches!(out, Err(StoreError::Io(_))), "{stage:?}");
        assert_eq!(
            follower::read_mark(&f.dir).unwrap().is_some(),
            placed,
            "{stage:?}"
        );
        assert!(!f.dir.join(".FOLLOWER.tmp").exists(), "{stage:?}");
    }
    let _ = std::fs::remove_file(mark_path(&f.dir));
    // a lock of another store is the existing refusal, and writes nothing
    let other_store = source();
    let wrong = StoreLock::take(&other_store.dir).unwrap();
    let m = mark_at(&f, f.end);
    assert!(matches!(
        follower::write_seed_mark(&f.dir, &wrong, &m),
        Err(StoreError::Locked(_))
    ));
    assert!(follower::read_mark(&f.dir).unwrap().is_none());
    assert!(matches!(
        match Store::open_follower_locked(&f.dir, &wrong) {
            Ok(_) => panic!("opened"),
            Err(e) => e,
        },
        StoreError::Locked(_)
    ));
    // the saved-boundary scan's work bound, at the limit and one past
    follower::check_frame_count_for_tests(100_000_000).unwrap();
    assert!(matches!(
        follower::check_frame_count_for_tests(100_000_001),
        Err(StoreError::FollowerMarkCheckLimit)
    ));
}

#[test]
fn mh3_every_ordinary_write_refuses_before_its_fast_path() {
    let f = marked();
    let mut s = Store::open_follower(&f.dir).unwrap();
    assert_eq!(s.access(), StoreAccess::ReadOnly);
    // a repeated object: the fast path of a known record
    let known = s.get_ref("view/work").unwrap();
    assert!(known.is_some());
    let before = record(&f.dir);
    let end = s.log_end();
    let far = std::time::Instant::now() + std::time::Duration::from_secs(86_400);
    let hash = Hash::of(b"x");
    let sub = InboxSub {
        actor: "agent:a".into(),
        run: None,
        cursor: InboxCursor { at: end, skip: 0 },
        areas: vec![],
        role: "writer".into(),
    };
    let tag = Tag {
        name: "v1.0.0".into(),
        landing: hash,
        message: None,
        tagged_by: by(),
    };
    let plan = upgrade::inspect(&source().dir).unwrap();
    let lock = StoreLock::take(&tempfile::tempdir().unwrap().keep()).unwrap();
    let upload = tempfile::NamedTempFile::new().unwrap();
    let target = rdy_store::take_batch::UploadTarget {
        tag: "v1.0.0".into(),
        landing: hash,
    };
    let repo = f.repo;
    let check = |what: &str, r: Result<(), StoreError>| {
        is_follower(r.expect_err(what), repo, what);
    };
    check("empty write", s.write(vec![]).map(drop));
    check(
        "write",
        s.write(vec![(kinds::BLOB_CHUNK, b"raw".to_vec())])
            .map(drop),
    );
    check("put", s.put(kinds::TAG, &tag).map(drop));
    check("empty blob", s.put_blob(b"").map(drop));
    check("repeated blob", s.put_blob(b"a second batch").map(drop));
    check("empty refs", s.set_refs(vec![]));
    check("refs", s.set_refs(vec![("t/a".into(), vec![1])]));
    check("empty seal", s.seal(vec![]));
    check("seal", s.seal(vec![(kinds::BLOB_CHUNK, b"x".to_vec())]));
    check("checkpoint", s.checkpoint());
    check("idle checkpoint", s.checkpoint_if_idle(far).map(drop));
    check("reindex", s.reindex());
    check("removal", s.finish_redactions().map(drop));
    check("apply", s.seal_redaction_batch_for_tests(vec![]).map(drop));
    check(
        "commit",
        s.commit_to_view("work", change("work", vec![])).map(drop),
    );
    check(
        "change",
        s.commit_change(change("work", vec![]), vec![]).map(drop),
    );
    check(
        "view",
        s.view_create("work", None, None, false, &by()).map(drop),
    );
    check("proposal", s.propose("work", "T", by()).map(drop));
    check("landing", s.land(&hash, by()).map(drop));
    check("tag", s.put_tag(&tag).map(drop));
    check("inbox", s.put_inbox(&sub));
    check("check requests", s.put_check_requests(&[]).map(drop));
    check("upgrade inspect", upgrade::inspect(&f.dir).map(drop));
    check(
        "upgrade apply",
        upgrade::apply_locked(&f.dir, &lock, &plan).map(drop),
    );
    check(
        "ingest",
        rdy_store::ingest::Ingest::create(&f.dir).map(drop),
    );
    match s.take_batches(
        &[std::fs::File::open(upload.path()).unwrap()],
        &target,
        |_, _| Err::<rdy_store::take_batch::TagChoice, ()>(()),
    ) {
        Err(rdy_store::take_batch::TakeError::Store(e)) => is_follower(e, repo, "take"),
        _ => panic!("not the follower refusal"),
    }
    // held-mode setup is not a content write
    s.set_held(true).unwrap();
    check(
        "held write",
        s.write(vec![(kinds::BLOB_CHUNK, b"x".to_vec())]).map(drop),
    );
    assert!(s.open_batch().is_none());
    s.set_held(false).unwrap();
    assert_eq!(s.log_end(), end);
    assert_eq!(record(&f.dir), before, "no record moved");
    s.close().unwrap();
    assert_eq!(record(&f.dir), before, "the close wrote no batch");
}

#[test]
fn mh3_follower_open_has_no_local_batch() {
    let f = marked();
    let before = record(&f.dir);
    // the derived index goes: the open rebuilds it from the log and writes no batch
    std::fs::remove_file(f.dir.join("index.redb")).unwrap();
    let s = Store::open_follower(&f.dir).unwrap();
    assert_eq!(s.access(), StoreAccess::ReadOnly);
    assert!(s.reader().ensure_writable().is_err());
    assert_eq!(s.log_end(), f.end);
    assert_eq!(s.follower_end().unwrap(), f.end);
    assert!(s.open_report().caught_up_frames > 0, "built from the log");
    assert_eq!(s.repo_id().unwrap(), Some(f.repo));
    assert!(s.get_ref("view/work").unwrap().is_some());
    assert_eq!(record(&f.dir), before, "open: no frame, ref or first use");
    s.close().unwrap();
    assert_eq!(record(&f.dir), before, "close: no frame");
    assert!(f.dir.join("index.redb").exists(), "the derived index stays");
    assert!(!f.dir.join("index.open").exists(), "a clean close");
    // again, over the index the close kept, and from the layout's entry
    let s = layout::open_follower_store(f.dir.parent().unwrap()).unwrap();
    assert_eq!(s.follower_end().unwrap(), f.end);
    assert!(!s.open_report().index_rebuilt);
    assert_eq!(s.open_report().caught_up_frames, 0);
    drop(s);
    assert_eq!(record(&f.dir), before, "drop: no frame");
    // the locked open borrows the caller's lock and keeps it held
    let lock = StoreLock::take(&f.dir).unwrap();
    let s = Store::open_follower_locked(&f.dir, &lock).unwrap();
    s.close().unwrap();
    assert!(matches!(
        StoreLock::take(&f.dir),
        Err(StoreError::Locked(_))
    ));
}

#[test]
fn mh3_bad_or_missing_mark_never_opens_as_a_writer() {
    // absent on the explicit open; a normal store keeps its normal open
    let f = source();
    let e = open_refusal(&f.dir);
    assert!(matches!(e, StoreError::FollowerMark(_)), "{e:?}");
    assert_eq!(e.to_string(), "this store has no follower mark");
    Store::open(&f.dir).unwrap().close().unwrap();

    // a malformed mark refuses both opens, before anything moves
    let f = marked();
    let good = std::fs::read(mark_path(&f.dir)).unwrap();
    raw_mark(&f.dir, b"rdy follower 1\nnot json\n");
    let before = tree(&f.dir);
    for policy in [OpenPolicy::AutoUpgrade, OpenPolicy::RequireCurrent] {
        let e = Store::open_with_policy(&f.dir, policy).err().unwrap();
        assert_eq!(e.to_string(), "the follower mark is not valid JSON");
    }
    assert_eq!(
        open_refusal(&f.dir).to_string(),
        "the follower mark is not valid JSON"
    );
    assert_eq!(tree(&f.dir), before);
    raw_mark(&f.dir, &good);

    // a valid mark: every ordinary open refuses before a sentinel, an index or an upgrade
    std::fs::remove_file(f.dir.join("index.redb")).unwrap();
    let before = tree(&f.dir);
    let opens: Vec<(&str, Result<Store, StoreError>)> = vec![
        ("open", Store::open(&f.dir)),
        ("create", Store::create(&f.dir)),
        ("holder", Store::create_holder(&f.dir, f.repo)),
        (
            "layout",
            layout::open_project_store(f.dir.parent().unwrap()),
        ),
        (
            "create layout",
            layout::create_project_store(f.dir.parent().unwrap()),
        ),
    ];
    for (what, out) in opens {
        is_follower(out.err().expect(what), f.repo, what);
    }
    is_follower(Store::check_existing(&f.dir).unwrap_err(), f.repo, "check");
    {
        let lock = StoreLock::take(&f.dir).unwrap();
        let before = tree(&f.dir);
        is_follower(
            Store::open_locked(&f.dir, &lock).err().unwrap(),
            f.repo,
            "locked",
        );
        assert_eq!(tree(&f.dir), before);
    }
    let mut lockless = before.clone();
    lockless.remove(&f.dir.join("lock"));
    let mut now = tree(&f.dir);
    now.remove(&f.dir.join("lock"));
    assert_eq!(now, lockless, "no side effect");

    // the mark against the log, each with its exact text; the log stays
    let f = source();
    let hex = |r: RepoId| r.to_string();
    let good_mark = mark_at(&f, f.end);
    let log = segments(&f.dir);
    let foreign = RepoId([7; 16]);
    let past = Location {
        segment: f.end.segment,
        offset: f.end.offset + 37,
    };
    let inside = Location {
        segment: f.first.segment,
        offset: f.first.offset - 1,
    };
    let other_store = {
        let mut m = match &f.store {
            DumpStore::Features(m) => m.clone(),
            DumpStore::Legacy(_) => unreachable!(),
        };
        m.features.pop();
        DumpStore::Features(m)
    };
    let cases: Vec<(&str, FollowerMark, String)> = vec![
        (
            "foreign repo",
            FollowerMark {
                chain: ChainEnd {
                    repo_id: foreign,
                    ..good_mark.chain.clone()
                },
                files: vec![],
            },
            format!(
                "the follower mark names repo {}, but the log holds repo {}",
                hex(foreign),
                hex(f.repo)
            ),
        ),
        (
            "past the end",
            FollowerMark {
                chain: ChainEnd {
                    end: past,
                    ..good_mark.chain.clone()
                },
                files: vec![],
            },
            format!(
                "the follower mark ends at {}.{}, past the log's end at {}.{}",
                past.segment, past.offset, f.end.segment, f.end.offset
            ),
        ),
        (
            "a wrong marker",
            FollowerMark {
                chain: ChainEnd {
                    end_batch: BatchMarker {
                        frames: good_mark.chain.end_batch.frames + 1,
                        ..good_mark.chain.end_batch
                    },
                    ..good_mark.chain.clone()
                },
                files: vec![],
            },
            "the follower mark's batch marker differs from the log's at its saved end".into(),
        ),
        (
            "not a batch end",
            FollowerMark {
                chain: ChainEnd {
                    end: inside,
                    ..good_mark.chain.clone()
                },
                files: vec![],
            },
            "the follower mark's batch marker differs from the log's at its saved end".into(),
        ),
        (
            "a wrong saved manifest",
            FollowerMark {
                chain: ChainEnd {
                    store_format: other_store,
                    ..good_mark.chain.clone()
                },
                files: vec![],
            },
            "the follower mark's store format differs from the log's at its saved end".into(),
        ),
        (
            "a scalar store",
            FollowerMark {
                chain: ChainEnd {
                    store_format: DumpStore::Legacy(18),
                    ..good_mark.chain.clone()
                },
                files: vec![],
            },
            "the follower mark's store format differs from the log's at its saved end".into(),
        ),
    ];
    for (what, bad, text) in cases {
        // the seed helper checks the same facts, and writes no mark
        let lock = StoreLock::take(&f.dir).unwrap();
        let e = follower::write_seed_mark(&f.dir, &lock, &bad).unwrap_err();
        assert_eq!(e.to_string(), text, "seed: {what}");
        assert!(follower::read_mark(&f.dir).unwrap().is_none(), "{what}");
        drop(lock);
        follower::replace_mark_for_tests(&f.dir, &bad).unwrap();
        let e = open_refusal(&f.dir);
        assert!(matches!(e, StoreError::FollowerMark(_)), "{what}: {e:?}");
        assert_eq!(e.to_string(), text, "open: {what}");
        is_follower(Store::open(&f.dir).err().unwrap(), bad.chain.repo_id, what);
        assert_eq!(segments(&f.dir), log, "{what}");
        std::fs::remove_file(mark_path(&f.dir)).unwrap();
    }

    // a mark on a store with no batch: past the end, and no first batch is written
    let base = tempfile::tempdir().unwrap();
    let empty = base.path().join("s");
    std::fs::create_dir_all(empty.join("segments")).unwrap();
    rdy_store::format::write_format_file(&empty, &rdy_store::format::baseline_features()).unwrap();
    follower::replace_mark_for_tests(&empty, &good_mark).unwrap();
    assert_eq!(
        open_refusal(&empty).to_string(),
        format!(
            "the follower mark ends at {}.{}, past the log's end at 0.0",
            f.end.segment, f.end.offset
        )
    );
    assert_eq!(segments(&empty).len(), 0);

    // a valid suffix after the saved end stays, and the end names it
    let f = source();
    let log = segments(&f.dir);
    follower::replace_mark_for_tests(&f.dir, &mark_at(&f, f.first)).unwrap();
    let s = Store::open_follower(&f.dir).unwrap();
    assert_eq!(s.follower_end().unwrap(), f.end, "the suffix is not cut");
    s.close().unwrap();
    assert_eq!(segments(&f.dir), log);

    // a zeroed payload no apply authorizes keeps the corruption refusal, past a parsed mark
    let f = marked();
    let seg = f.dir.join("segments").join("seg-000000.log");
    let mut bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    bytes[37..37 + len].fill(0);
    std::fs::write(&seg, &bytes).unwrap();
    let e = open_refusal(&f.dir);
    assert!(
        matches!(e, StoreError::RedactionCorrupt { .. }),
        "the existing refusal: {e:?}"
    );

    // a sealed upgrade not yet completed: RequireCurrent refuses it, nothing moves
    use rdy_store::format::{FeatureClass, StoreFeature, StoreFeatures};
    let f = source();
    let registry = vec![upgrade::UpgradeStep {
        id: "add_zz".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![StoreFeature {
            name: "zz_added".into(),
            class: FeatureClass::Compatible,
            required_rdy: "100.2.0".into(),
        }]),
    }];
    let lock = StoreLock::take(&f.dir).unwrap();
    let plan = upgrade::inspect_with_registry_for_tests(&f.dir, &registry).unwrap();
    upgrade::apply_locked_with_registry_for_tests(&f.dir, &lock, &plan, &registry).unwrap();
    drop(lock);
    let end = {
        let s = upgrade::open_pending_for_tests(&f.dir, &StoreLock::take(&f.dir).unwrap()).unwrap();
        let end = s.log_end();
        drop(s);
        end
    };
    follower::replace_mark_for_tests(&f.dir, &mark_at(&f, end)).unwrap();
    let log = segments(&f.dir);
    let e = open_refusal(&f.dir);
    assert!(matches!(e, StoreError::UpgradeRequired), "{e:?}");
    assert_eq!(segments(&f.dir), log);
}
