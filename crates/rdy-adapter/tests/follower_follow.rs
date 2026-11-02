//! Following checked increments through the adapter (D-AA4FD: "A repeated `follow` of an increment
//! already written records only the digest."): the links and feature growth a chain load checks,
//! the repeat, removal before zeroing and the source's project files.
use rdy_adapter::maintenance::{self, ChainEnd};
use rdy_adapter::{Follower, RdyError};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::{Hash, Role};
use rdy_store::follower::{FollowError, FollowerFailure, FollowerMark, prepare_follow, read_mark};
use rdy_store::format::{FeatureClass, inject_test_feature};
use rdy_store::kinds;
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use rdy_store::segment::{RemovalStep, on_removal_step};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FOLLOW_FIX: &str =
    "follow the source's increments in chain order from the follower's saved end";
const SEED_FIX: &str = "seed a new follower from a full dump of the source store";
const CANARY: &str = "kestrel canary text";

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn triple(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

fn land(r: &mut Rdy, n: usize, body: &str) -> String {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": "A", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
    ]);
    let w = r
        .write(
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
    w.created["$p"].clone()
}

fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
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

/// A source project, its chain so far and a follower seeded from the first file.
struct Chain {
    base: tempfile::TempDir,
    root: PathBuf,
    r: Rdy,
    last: PathBuf,
    n: usize,
    follower: PathBuf,
    full: PathBuf,
}

fn chain() -> Chain {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    land(&mut r, 0, "first");
    let full = base.path().join("full.rdydump");
    r.read().dump(&full).unwrap();
    let follower = base.path().join("f");
    maintenance::seed_follower(&full, &follower).unwrap();
    Chain {
        base,
        root,
        r,
        last: full.clone(),
        n: 0,
        follower,
        full,
    }
}

impl Chain {
    /// One more landing and its increment.
    fn next(&mut self) -> PathBuf {
        self.n += 1;
        let n = self.n;
        land(&mut self.r, n, &format!("body {n}"));
        self.cut()
    }

    /// The increment of everything since the last file.
    fn cut(&mut self) -> PathBuf {
        let out = self.base.path().join(format!("i{}.rdydump", self.n));
        self.r.read().dump_after_file(&self.last, &out).unwrap();
        self.last = out.clone();
        out
    }

    fn end(&self) -> ChainEnd {
        read_mark(&self.follower.join(".rdy/store"))
            .unwrap()
            .unwrap()
            .chain
    }

    fn dump_bytes(&self) -> Vec<u8> {
        let out = self.base.path().join(format!("whole{}.rdydump", self.n));
        let _ = std::fs::remove_file(&out);
        self.r.read().dump(&out).unwrap();
        std::fs::read(&out).unwrap()
    }
}

/// The follower's own full dump.
fn follower_dump(dir: &Path, base: &Path) -> Vec<u8> {
    let core = rdy_engine::follower::FollowerCore::open(dir).unwrap();
    let out = base.join("follower-whole.rdydump");
    let _ = std::fs::remove_file(&out);
    core.read().unwrap().dump(&out).unwrap();
    std::fs::read(&out).unwrap()
}

#[test]
fn mh8_follow_links_and_feature_growth_match_chain_load() {
    let mut c = chain();
    let i1 = c.next();
    let i2 = c.next();
    let i3 = c.next();
    let before = segments(&c.follower);
    let mark_before = c.end();
    let mut f = Follower::open(&c.follower).unwrap();

    // a full dump
    assert_eq!(
        triple(&f.follow(&c.full).unwrap_err()),
        (
            "E_DUMP_CHAIN",
            "a follower takes one increment, not a full dump",
            "seed a new follower from this full dump, or follow the next increment of the chain"
        )
    );
    // an increment past the next: the chain loader's position text, with the follow fix
    let e = f.follow(&i2).unwrap_err();
    assert_eq!(e.code, "E_DUMP_CHAIN");
    assert!(
        e.message.contains("but the file before it ends at"),
        "{e:?}"
    );
    assert_eq!(e.fix, FOLLOW_FIX);
    // another repo: repo comes before position
    let other = c.base.path().join("other");
    let mut o = Rdy::init(&other, "rdy-standard").unwrap();
    land(&mut o, 0, "other");
    let of = c.base.path().join("other.rdydump");
    let summary = o.read().dump(&of).unwrap();
    land(&mut o, 1, "other more");
    let oi = c.base.path().join("other-i.rdydump");
    o.read().dump_after(&summary.chain_end(), &oi).unwrap();
    o.close().unwrap();
    let e = f.follow(&oi).unwrap_err();
    assert!(e.message.contains("but the chain holds repo"), "{e:?}");
    assert_eq!(e.fix, FOLLOW_FIX);
    // the right place with another predecessor digest
    let forged = c.base.path().join("forged.rdydump");
    let ended = mark_before.clone();
    let wrong = ChainEnd {
        digest: Hash::of(b"another file"),
        ..ended
    };
    c.r.read().dump_after(&wrong, &forged).unwrap();
    let e = f.follow(&forged).unwrap_err();
    assert!(e.message.contains("follows the file with digest"), "{e:?}");
    assert_eq!(e.fix, FOLLOW_FIX);
    // no refusal so far wrote a byte or moved the mark
    assert_eq!(f.end().unwrap(), mark_before.end);
    drop(f);
    assert_eq!(segments(&c.follower), before);
    assert_eq!(c.end(), mark_before);

    // the correct links, in order; then an older file
    let mut f = Follower::open(&c.follower).unwrap();
    let got = f.follow(&i1).unwrap();
    assert!(!got.repeated);
    assert!(got.batches > 0 && got.frames > 0);
    f.follow(&i2).unwrap();
    let e = f.follow(&i1).unwrap_err();
    assert!(
        e.message.contains("but the file before it ends at"),
        "{e:?}"
    );
    f.follow(&i3).unwrap();
    assert_eq!(f.snapshot().unwrap().tags().unwrap().len(), 0);
    drop(f);
    assert_eq!(segments(&c.follower), segments(&c.root));

    // a manifest that adds a compatible feature by its first use grows the follower
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::BLOB_CHUNK);
    c.r.store_mut().put_blob(b"a first use").unwrap();
    c.r.seal_open().unwrap();
    c.n += 1;
    let grown = c.cut();
    let mut f = Follower::open(&c.follower).unwrap();
    f.follow(&grown).unwrap();
    drop(f);
    drop(_known);
    assert_eq!(segments(&c.follower), segments(&c.root));
    c.r.close().unwrap();
}

#[test]
fn mh8_repeat_only_advances_the_saved_digest() {
    let mut c = chain();
    let i1 = c.next();
    let store = c.follower.join(".rdy/store");
    let old = c.end();
    let mut f = Follower::open(&c.follower).unwrap();
    let first = f.follow(&i1).unwrap();
    assert!(!first.repeated);
    let want = read_mark(&store).unwrap().unwrap();
    assert_eq!(want.chain, first.end);
    // the identical file again appends nothing and is harmless
    let again = f.follow(&i1).unwrap();
    assert!(again.repeated);
    assert_eq!((again.batches, again.frames), (0, 0));
    assert_eq!(again.end, first.end);
    drop(f);
    // the same log with the old mark: the repeat records only the digest
    let bytes = segments(&c.follower);
    let mut stale = want.clone();
    stale.chain = old;
    rdy_store::follower::replace_mark_for_tests(&store, &stale).unwrap();
    let mut f = Follower::open(&c.follower).unwrap();
    let e = match f.snapshot() {
        Err(e) => e,
        Ok(_) => panic!("an unfinished increment reads"),
    };
    assert_eq!(
        triple(&e),
        (
            "E_DUMP_CHAIN",
            "the follower has an unfinished increment",
            "retry the same increment before reading the follower"
        )
    );
    let got = f.follow(&i1).unwrap();
    assert!(got.repeated);
    assert_eq!((got.batches, got.frames), (0, 0));
    assert_eq!(got.end, want.chain);
    assert!(f.snapshot().is_ok());
    drop(f);
    assert_eq!(segments(&c.follower), bytes);
    assert_eq!(read_mark(&store).unwrap().unwrap(), want);
    // the same end with another digest is refused and writes nothing
    let forged = c.base.path().join("same-end.rdydump");
    c.r.read()
        .dump_after(
            &ChainEnd {
                digest: Hash::of(b"another"),
                ..c.end().clone()
            },
            &forged,
        )
        .ok();
    assert!(!forged.exists(), "no batch to hold: no file");
    // a shorter retry than the log holds
    let i2 = c.next();
    let i3 = c.next();
    let mut f = Follower::open(&c.follower).unwrap();
    f.follow(&i2).unwrap();
    f.follow(&i3).unwrap();
    drop(f);
    let mark3 = read_mark(&store).unwrap().unwrap();
    let mut back = mark3.clone();
    back.chain = want.chain.clone();
    rdy_store::follower::replace_mark_for_tests(&store, &back).unwrap();
    let mut f = Follower::open(&c.follower).unwrap();
    assert_eq!(
        triple(&f.follow(&i2).unwrap_err()),
        (
            "E_DUMP_CHAIN",
            "the follower's written prefix differs from the increment",
            SEED_FIX
        )
    );
    drop(f);
    rdy_store::follower::replace_mark_for_tests(&store, &mark3).unwrap();
    c.r.close().unwrap();
}

/// A source with a block `removed` in the seed, removal request landed in the increment.
fn removal_increment(c: &mut Chain, target: &str) -> PathBuf {
    let rq = session(Role::Writer);
    let approver = session(Role::Approver);
    c.r.redact_in(
        &rq,
        "rq",
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "personal".into(),
            reason: "the term names a person".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p =
        c.r.propose(&rq, "rq", "rq", Some("Removes the term."))
            .unwrap();
    c.r.land(&approver, &p.id).unwrap();
    c.n += 1;
    c.cut()
}

/// The seed holds the canary in a landed block; the source has a second landing tagged.
fn canary_chain() -> (Chain, String) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let block = land(&mut r, 0, CANARY);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let full = base.path().join("full.rdydump");
    r.read().dump(&full).unwrap();
    let follower = base.path().join("f");
    maintenance::seed_follower(&full, &follower).unwrap();
    (
        Chain {
            base,
            root,
            r,
            last: full.clone(),
            n: 0,
            follower,
            full,
        },
        block,
    )
}

fn has_canary(root: &Path) -> bool {
    segments(root)
        .values()
        .any(|b| b.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()))
}

#[test]
fn mh8_removal_invalidates_captured_reads_before_zeroing() {
    let (mut c, block) = canary_chain();
    let target = {
        let st = c.r.read();
        let version = st
            .materialized(&rdy_engine::Scope::Canon)
            .unwrap()
            .version_of(&rdy_model::BlockId::parse(&block).unwrap())
            .unwrap()
            .to_string();
        format!("{block}@{version}")
    };
    let inc = removal_increment(&mut c, &target);
    assert!(has_canary(&c.follower));

    let mut f = Follower::open(&c.follower).unwrap();
    // a warm read of the old state: its tag materializes the removed block
    let old = f.snapshot().unwrap();
    assert!(!old.tags().unwrap().is_empty());
    let tag_before = old.tag_history("1.0.0").unwrap();

    // punching unsupported never fails a follow, and the old reader is refused before any zero
    // reaches the file: the removal's probe runs after the zeros and sees the epoch moved
    let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
    let log = std::rc::Rc::clone(&seen);
    on_removal_step(Some(Box::new(move |s| {
        log.borrow_mut().push(s);
        Ok(())
    })));
    // first a failed zero: the follow fails, reads stop, and a reopen completes the removal
    arm_failpoint(Some((Failpoint::AfterZeroWrite, FailAction::Fail)));
    let e = f.follow(&inc).unwrap_err();
    arm_failpoint(None);
    assert_eq!(e.code, "E_REDACTION_PENDING", "{e:?}");
    assert!(old.tag_history("1.0.0").is_err(), "the captured read stops");
    assert!(f.snapshot().is_err());
    drop((old, f));

    // the retry after a reopen appends nothing and finishes
    let mut f = Follower::open(&c.follower).unwrap();
    let got = f.follow(&inc).unwrap();
    assert!(got.repeated);
    assert_eq!((got.batches, got.frames), (0, 0));
    assert!(!has_canary(&c.follower));
    let fresh = f.snapshot().unwrap();
    let want = c.r.read();
    assert_eq!(fresh.tags().unwrap().len(), 1);
    assert_eq!(fresh.tag_history("1.0.0").unwrap().len(), tag_before.len());
    assert_eq!(fresh.canon(), {
        let src = want.canon_head();
        src
    });
    drop((fresh, f));
    on_removal_step(None);
    assert!(
        seen.borrow().iter().any(|s| *s == RemovalStep::BeforePunch),
        "the zeros were durable and the punch ran after them"
    );
    assert_eq!(segments(&c.follower), segments(&c.root));
    assert_eq!(
        follower_dump(&c.follower, c.base.path()),
        c.dump_bytes(),
        "the dump bytes"
    );
    drop(want);
    c.r.close().unwrap();
}

#[test]
fn mh8_removal_renews_caches_with_no_failure() {
    // the live path: a warm old snapshot refuses, a new one shows the removed result
    let (mut c, block) = canary_chain();
    let id = rdy_model::BlockId::parse(&block).unwrap();
    let target = {
        let st = c.r.read();
        let v = st
            .materialized(&rdy_engine::Scope::Canon)
            .unwrap()
            .version_of(&id)
            .unwrap()
            .to_string();
        format!("{block}@{v}")
    };
    let inc = removal_increment(&mut c, &target);
    let mut core = rdy_engine::follower::FollowerCore::open(&c.follower).unwrap();
    let old = core.read().unwrap();
    let tag = old.resolve_scope("tag:1.0.0").unwrap();
    // warm the materialization
    let warm = old.materialized(&tag).unwrap();
    assert_eq!(warm.get(&id).unwrap().body, CANARY);
    core.follow(&inc).unwrap();
    assert!(!has_canary(&c.follower));
    assert_eq!(
        old.ensure_current_epoch().err().map(|e| e.code),
        Some("E_REDACTED_STATE"),
        "the captured state refuses"
    );
    let new = core.read().unwrap();
    let got = new.materialized(&new.resolve_scope("tag:1.0.0").unwrap());
    let body = got.unwrap().get(&id).map(|b| b.body.clone());
    assert_ne!(
        body.as_deref(),
        Some(CANARY),
        "the new state has no removed text"
    );
    drop((old, new, core));

    // a failed fresh state keeps the follower blocked: a broken config at the follow's end
    let mut more = c.next();
    let _ = &mut more;
    let mut core = rdy_engine::follower::FollowerCore::open(&c.follower).unwrap();
    assert!(core.read().is_ok());
    // the mark's write failing leaves it blocked and the retry appends nothing
    let store = c.follower.join(".rdy/store");
    let got = rdy_store::fsutil::with_private_write_probe_for_tests(
        |stage, path: &Path| match stage {
            rdy_store::fsutil::PrivateWriteStage::Created
                if path.to_string_lossy().contains("FOLLOWER") =>
            {
                Err(std::io::Error::other("injected mark failure"))
            }
            _ => Ok(()),
        },
        || core.follow(&more),
    );
    assert!(got.is_err());
    assert_eq!(
        core.read().err().map(|e| e.message),
        Some("the follower has an unfinished increment".to_string())
    );
    let mark = read_mark(&store).unwrap().unwrap();
    let retried = core.follow(&more).unwrap();
    assert!(retried.repeated);
    assert_ne!(read_mark(&store).unwrap().unwrap(), mark);
    assert!(core.read().is_ok());

    // a fresh state that cannot be prepared keeps reads refused: the source's next file set
    // holds a config that does not parse
    let config = c.root.join(".rdy.yaml");
    let good = std::fs::read(&config).unwrap();
    c.n += 1;
    land(&mut c.r, c.n, "the last");
    std::fs::write(&config, "::: not yaml :::").unwrap();
    let bad = c.cut();
    let e = core.follow(&bad).unwrap_err();
    assert_ne!(e.code, "E_DUMP_CHAIN");
    assert_eq!(
        core.read().err().map(|e| e.message),
        Some("the follower has an unfinished increment".to_string())
    );
    drop(core);
    std::fs::write(&config, good).unwrap();

    c.r.close().unwrap();
}

#[test]
fn mh8_project_files_and_scopes_follow_the_source() {
    let mut c = chain();
    // an addition and a replacement in the next increment
    let root = c.root.clone();
    std::fs::write(root.join(".rdyignore"), "*.draft.yaml\n").unwrap();
    let i1 = c.next();
    let mut f = Follower::open(&c.follower).unwrap();
    f.follow(&i1).unwrap();
    assert_eq!(
        std::fs::read(c.follower.join(".rdyignore")).unwrap(),
        b"*.draft.yaml\n"
    );
    drop(f);
    // no local.yaml comes from the source, and a local one stays
    std::fs::create_dir_all(root.join(".rdy")).unwrap();
    std::fs::write(root.join(".rdy/local.yaml"), "x: 1\n").unwrap();
    std::fs::write(c.follower.join(".rdy/local.yaml"), "y: 2\n").unwrap();
    // a local edit refuses, with nothing appended
    let keep = std::fs::read(c.follower.join(".rdyignore")).unwrap();
    std::fs::write(c.follower.join(".rdyignore"), "edited\n").unwrap();
    std::fs::remove_file(root.join(".rdyignore")).unwrap();
    let i2 = c.next();
    let bytes = segments(&c.follower);
    let mut f = Follower::open(&c.follower).unwrap();
    assert_eq!(
        triple(&f.follow(&i2).unwrap_err()),
        (
            "E_DUMP_CHAIN",
            "the follower's project files differ from its saved source files",
            SEED_FIX
        )
    );
    assert!(
        f.snapshot().is_ok(),
        "a refusal before the append keeps reads"
    );
    drop(f);
    assert_eq!(segments(&c.follower), bytes);
    // a deletion follows the source, and the local file stays
    std::fs::write(c.follower.join(".rdyignore"), keep).unwrap();
    let mut f = Follower::open(&c.follower).unwrap();
    f.follow(&i2).unwrap();
    assert!(!c.follower.join(".rdyignore").exists());
    assert_eq!(
        std::fs::read_to_string(c.follower.join(".rdy/local.yaml")).unwrap(),
        "y: 2\n"
    );
    drop(f);
    // partial replacement: log ahead and old files, the retry finishes from the full set
    std::fs::write(root.join(".rdyignore"), "again\n").unwrap();
    let i3 = c.next();
    let store = c.follower.join(".rdy/store");
    let mark = read_mark(&store).unwrap().unwrap();
    let mut f = Follower::open(&c.follower).unwrap();
    f.follow(&i3).unwrap();
    drop(f);
    rdy_store::follower::replace_mark_for_tests(&store, &mark).unwrap();
    std::fs::write(c.follower.join(".rdyignore"), "half written").unwrap();
    let mut f = Follower::open(&c.follower).unwrap();
    assert!(f.follow(&i3).unwrap().repeated);
    assert_eq!(
        std::fs::read(c.follower.join(".rdyignore")).unwrap(),
        b"again\n"
    );
    drop(f);
    c.r.close().unwrap();
}

#[test]
fn mh8_holder_follow_lists_no_files_and_refuses_project_scopes() {
    use rdy_engine::{Scope, stream_export, stream_take};
    use rdy_store::take_batch::UploadTarget;
    use std::collections::BTreeSet;
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    land(&mut r, 0, "one");
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let repo = r.store().repo_id().unwrap().unwrap();
    let take = |r: &Rdy,
                h: &mut Rdy,
                have: &BTreeSet<rdy_store::object_refs::ObjectKey>,
                name: &str,
                i: usize| {
        let tags = r.store().tags().unwrap();
        let (_, t, _) = tags
            .iter()
            .find(|(n, _, _)| n.contains(name))
            .cloned()
            .unwrap();
        let landing = r.store().read_tag(&t).unwrap().landing;
        let mut batch = Vec::new();
        stream_export::write(&r.read(), t, have, &mut batch).unwrap();
        let file = d.path().join(format!("{i}.rdysync"));
        std::fs::write(&file, &batch).unwrap();
        stream_take::take(
            h,
            &[std::fs::File::open(&file).unwrap()],
            UploadTarget {
                tag: format!("v{name}"),
                landing,
            },
        )
        .unwrap();
        landing
    };
    let hdir = d.path().join("h");
    let mut h = Rdy::create_holder(&hdir, repo).unwrap();
    take(&r, &mut h, &BTreeSet::new(), "1.0.0", 1);
    let full = d.path().join("h.rdydump");
    h.read().dump(&full).unwrap();
    let hf = d.path().join("hf");
    maintenance::seed_follower(&full, &hf).unwrap();
    land(&mut r, 1, "two");
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    let landing = take(&r, &mut h, &BTreeSet::new(), "2.0.0", 2);
    let inc = d.path().join("h1.rdydump");
    h.read().dump_after_file(&full, &inc).unwrap();
    let mut f = rdy_engine::follower::FollowerCore::open(&hf).unwrap();
    assert!(!f.follow(&inc).unwrap().repeated);
    let mark = read_mark(&hf.join(".rdy/store")).unwrap().unwrap();
    assert!(mark.files.is_empty());
    let state = f.read().unwrap();
    assert!(state.materialized(&Scope::At(landing)).is_ok());
    assert!(state.materialized(&Scope::Canon).is_err());
    drop((state, f));
    assert_eq!(segments(&hf), segments(&hdir));
    h.close().unwrap();
    r.close().unwrap();
}

#[test]
fn mh8_hostile_increment_files_refuse_with_no_append() {
    let mut c = chain();
    let i1 = c.next();
    let bytes = std::fs::read(&i1).unwrap();
    let before = segments(&c.follower);
    let mut f = Follower::open(&c.follower).unwrap();
    let bad = c.base.path().join("bad.rdydump");
    // truncation, a flipped body byte, nonsense and a missing file
    for (what, b) in [
        ("truncated", bytes[..bytes.len() / 2].to_vec()),
        ("nonsense", b"not a dump".to_vec()),
        ("flipped", {
            let mut b = bytes.clone();
            let at = b.len() - 200;
            b[at] ^= 0xff;
            b
        }),
    ] {
        std::fs::write(&bad, &b).unwrap();
        let e = f.follow(&bad).unwrap_err();
        assert!(
            e.code == "E_DUMP" || e.code == "E_DUMP_CHAIN",
            "{what}: {e:?}"
        );
        assert_eq!(f.end().unwrap(), c.end().end, "{what}");
        assert!(f.snapshot().is_ok(), "{what}: reads stay");
    }
    assert_eq!(
        f.follow(&c.base.path().join("gone")).unwrap_err().code,
        "E_BAD_INPUT"
    );
    drop(f);
    assert_eq!(segments(&c.follower), before);

    // a file swapped after its check, and a capability of another follower: no append
    let store_dir = c.follower.join(".rdy/store");
    let mark = read_mark(&store_dir).unwrap().unwrap();
    let mut store = rdy_store::Store::open_follower(&store_dir).unwrap();
    let swapped = c.base.path().join("swapped.rdydump");
    std::fs::copy(&i1, &swapped).unwrap();
    let v = prepare_follow(&store, &mark, &swapped).unwrap();
    let moved = c.base.path().join("swapped-new.rdydump");
    std::fs::copy(&i1, &moved).unwrap();
    std::fs::rename(&moved, &swapped).unwrap();
    assert!(matches!(
        store.follow_verified(v),
        Err(FollowError::Follower(FollowerFailure::ChangedInput))
    ));
    let other = FollowerMark {
        chain: ChainEnd {
            digest: Hash::of(b"another"),
            ..mark.chain.clone()
        },
        files: vec![],
    };
    let v = prepare_follow(&store, &other, &c.base.path().join("full.rdydump"));
    assert!(matches!(
        v.err(),
        Some(FollowError::Follower(FollowerFailure::FullDump))
    ));
    let v = prepare_follow(&store, &mark, &i1).unwrap();
    rdy_store::follower::replace_mark_for_tests(&store_dir, &other).unwrap();
    assert!(matches!(
        store.follow_verified(v),
        Err(FollowError::Follower(FollowerFailure::SavedChain))
    ));
    rdy_store::follower::replace_mark_for_tests(&store_dir, &mark).unwrap();
    store.close().unwrap();
    assert_eq!(segments(&c.follower), before);

    // the new limits' text
    assert_eq!(
        triple(&rdy_engine::dump_error::follow_limit(1_000_000, "batches")),
        (
            "E_LIMIT",
            "the follower increment exceeds 1000000 batches",
            "start a new chain with a full dump and seed a new follower"
        )
    );
    c.r.close().unwrap();
}
