//! rpl35 (repo links; D-FK58A, D-CN4QJ, D-8VJAZ): the crash boundaries of the foreign copy, on
//! every native platform. A child process of this test binary is aborted at a named point of a
//! link delivery, a foreign removal or an adoption; the parent reopens the project and checks
//! what the log holds.
//!
//! - A delivery is one sealed batch: after a kill, a partial append, a failed sync or a failed
//!   index update the project holds the old foreign copy or the complete delivery, never a part.
//!   A lost index is rebuilt to the same foreign copy.
//! - A foreign removal is sealed before its erasure: after a kill at any point of the erasure (or
//!   of the open that finishes it) the next open erases the same bytes. Reindex, fsck,
//!   compaction, a dump and its load, and the same delivery taken again never restore them.
//! - A failed adoption (a partial append, a failed sync, a failed index update, or a kill before
//!   its batch is sealed) leaves no durable state: the log, the pins, the views and the answers
//!   are as before, and the same card stays open with its hash.
//!
//! The faults are the store's own test hooks (`CrashPoint`, `Failpoint`, the append, sync and
//! index faults); the children are this binary, as in `store_crash`. Nothing waits on time.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::foreign_delivery::take_delivery;
use rdy_engine::foreign_removal::take_foreign_removal;
use rdy_engine::pending::{Item, ItemKind};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope};
use rdy_model::foreign::{ForeignId, ForeignPin, ForeignSource};
use rdy_model::{AnswerAct, BlockId, Hash, RepoId};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::foreign_fixture::{foreign_facts, store_files};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, pending};
use rdy_store::kinds;
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint_nth};
use rdy_store::segment::{CrashPoint, Location};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PROFILE: &str = "agents";
const SITE: &str = "hub.example";
const AT_MS: i64 = 1_760_000_000_000;
const KEPT_1: &str = "Every service logs in UTC.";
const KEPT_2: &str = "Every service logs in UTC, with the offset.";
/// The body of the cited claim the cited project removes: the canary the erasure must zero.
const REMOVED: &str = "Queues hold at most ten thousand rpl35 jobs.";

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn landed(r: &mut Rdy, view: &str) {
    land(r, view).unwrap_or_else(|e| panic!("land {view}: {e:?}"));
}

fn tag_of(r: &Rdy, name: &str) -> (Hash, Hash) {
    let (_, h, t) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == name)
        .unwrap_or_else(|| panic!("tag {name}"));
    (h, t.landing)
}

/// The cited project's `agents` link delivery at tag `name`.
fn delivery_at(r: &Rdy, name: &str) -> Vec<u8> {
    let (h, landing) = tag_of(r, name);
    let mut out = Vec::new();
    rdy_engine::link_export::write(&r.read(), name, h, landing, PROFILE, &mut out)
        .unwrap_or_else(|e| panic!("delivery at {name}: {e:?}"));
    out
}

/// The cited project's history at tag `name`, as one sync batch.
fn history_at(r: &Rdy, name: &str) -> Vec<u8> {
    let (h, _) = tag_of(r, name);
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), h, &Default::default(), &mut out)
        .unwrap_or_else(|e| panic!("history at {name}: {e:?}"));
    out
}

fn source(link: &BlockId) -> ForeignSource {
    ForeignSource::Link {
        link: link.to_string(),
        site: SITE.into(),
        at_ms: AT_MS,
    }
}

/// The cited project, closed: two deliveries (`kept` changes between them) and the history at
/// the second tag after a person removed `removed`, which carries the portable proof.
struct Cited {
    repo: RepoId,
    profile: BlockId,
    kept: BlockId,
    first: PathBuf,
    second: PathBuf,
    history: PathBuf,
}

fn cited(dir: &Path) -> Cited {
    let root = dir.join("cited");
    let mut c = Rdy::init(&root, "rdy-standard").unwrap();
    let w = put(
        &mut c,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Service rules"}},
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": KEPT_1},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": REMOVED},
            {"op": "create", "type": "doc", "id": "$admin",
             "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (doc, kept, removed, admin) = (
        bid(&w, "$d"),
        bid(&w, "$k"),
        bid(&w, "$x"),
        bid(&w, "$admin"),
    );
    let p = put(
        &mut c,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$p", "parent": admin.to_string(),
             "fields": {"key": PROFILE, "title": "Agents", "audience": "org",
                        "select": [doc.to_string()]}}
        ]),
    );
    let profile = bid(&p, "$p");
    landed(&mut c, "work");
    c.tag(&approver(), "v1.0.0", None, None).unwrap();
    let first = delivery_at(&c, "v1.0.0");
    put(
        &mut c,
        "work2",
        json!([{"op": "update", "id": kept.to_string(), "body": KEPT_2}]),
    );
    landed(&mut c, "work2");
    c.tag(&approver(), "v1.1.0", None, None).unwrap();
    let second = delivery_at(&c, "v1.1.0");
    c.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![removed.to_string()],
            reason_class: "secret".into(),
            reason: "an internal limit".into(),
            dry_run: false,
        },
    )
    .unwrap_or_else(|e| panic!("redact: {e:?}"));
    let pr = c
        .propose(&writer(), "redact", "Remove the limit", Some(TEST_SUMMARY))
        .unwrap();
    c.land(&approver(), &pr.id)
        .unwrap_or_else(|e| panic!("land the removal: {e:?}"));
    let history = history_at(&c, "v1.1.0");
    let repo = c.store().reader().repo_id().unwrap().unwrap();
    c.close().unwrap();
    let files = dir.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let save = |name: &str, bytes: &[u8]| {
        let p = files.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    Cited {
        repo,
        profile,
        kept,
        first: save("d1", &first),
        second: save("d2", &second),
        history: save("history", &history),
    }
}

/// The citing project, closed: a landed doc with a section and the repo links `core` and
/// `mirror` to the cited repo, and no delivery yet.
struct Citing {
    root: PathBuf,
    section: BlockId,
    live: BlockId,
    mirror: BlockId,
}

fn citing(dir: &Path, c: &Cited) -> Citing {
    let root = dir.join("citing");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Rules we follow"}},
            {"op": "create", "type": "repo_link", "id": "$core", "parent": "$d",
             "fields": {"alias": "core", "repo": c.repo.to_string(), "profile": c.profile.to_string()}},
            {"op": "create", "type": "repo_link", "id": "$mirror", "parent": "$d",
             "fields": {"alias": "mirror", "repo": c.repo.to_string(), "profile": c.profile.to_string()}}
        ]),
    );
    landed(&mut r, "seed");
    r.close().unwrap();
    Citing {
        root,
        section: bid(&w, "$s"),
        live: bid(&w, "$core"),
        mirror: bid(&w, "$mirror"),
    }
}

fn take(
    r: &mut Rdy,
    stream: &Path,
    link: &BlockId,
) -> Result<rdy_engine::foreign_delivery::DeliveryTaken, RdyError> {
    take_delivery(r, &File::open(stream).unwrap(), &source(link), None)
}

/// A copy of the closed project `from` (its files, its store and its index) at `to`.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

fn store_dir(root: &Path) -> PathBuf {
    root.join(".rdy").join("store")
}

/// The bytes of every segment file of the project at `root`, by file name.
fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(store_dir(root).join("segments")).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(e.path()).unwrap());
        }
    }
    out
}

/// How often the canary body occurs in the log of the project at `root`.
fn canaries(root: &Path) -> usize {
    let needle = REMOVED.as_bytes();
    segments(root)
        .values()
        .map(|b| b.windows(needle.len()).filter(|w| *w == needle).count())
        .sum()
}

/// The store files of the project at `root` (all but the derived index) with their bytes.
fn durable(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let dir = store_dir(root);
    store_files(&dir)
        .into_iter()
        .filter(|f| !f.ends_with(".lock") && !f.contains("index"))
        .map(|f| {
            let bytes = std::fs::read(dir.join(&f)).unwrap();
            (f, bytes)
        })
        .collect()
}

/// What a reopened project holds: its log end and its foreign copy. fsck passes.
fn state(root: &Path) -> (Location, Value) {
    let r = Rdy::open(root).unwrap_or_else(|e| panic!("open {}: {e:?}", root.display()));
    r.fsck().unwrap_or_else(|e| panic!("fsck: {e:?}"));
    let out = (r.store().log_end(), foreign_facts(&r));
    r.close().unwrap();
    out
}

// ---------------------------------------------------------------------------------------------
// the child process

/// The children: `RPL35_MODE` names what the child does on the project `RPL35_ROOT` with the
/// fault `RPL35_POINT` armed; it prints `NOT KILLED` when the fault did not kill it.
#[test]
#[ignore = "the rpl35 crash tests run this in a child process"]
fn store_foreign_crash_child() {
    let Ok(root) = std::env::var("RPL35_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let var = |k: &str| std::env::var(k).unwrap();
    let point = var("RPL35_POINT");
    match var("RPL35_MODE").as_str() {
        "delivery" => {
            let mut r = Rdy::open(&root).unwrap();
            r.store_mut()
                .crash_at(CrashPoint::parse(&point).expect("a crash point"));
            let link = BlockId::parse(&var("RPL35_LINK")).unwrap();
            let _ = take(&mut r, Path::new(&var("RPL35_FILE")), &link);
        }
        "removal" => {
            let mut r = Rdy::open(&root).unwrap();
            let repo: RepoId = var("RPL35_REPO").parse().unwrap();
            let nth = var("RPL35_NTH").parse().unwrap();
            arm_failpoint_nth(
                Failpoint::parse(&point).expect("a failpoint"),
                FailAction::Abort,
                nth,
            );
            let _ = take_foreign_removal(&mut r, &File::open(var("RPL35_FILE")).unwrap(), &repo);
        }
        "open" => {
            arm_failpoint_nth(
                Failpoint::parse(&point).expect("a failpoint"),
                FailAction::Abort,
                1,
            );
            let _ = Rdy::open(&root);
        }
        "adopt" => {
            let mut r = Rdy::open(&root).unwrap();
            r.store_mut()
                .crash_at(CrashPoint::parse(&point).expect("a crash point"));
            let _ = adopt(&mut r, &var("RPL35_REF"), &var("RPL35_HASH"));
        }
        other => panic!("unknown mode {other}"),
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "NOT KILLED").unwrap();
    out.flush().unwrap();
}

/// Runs the child in `mode` on `root` with `point` armed and `env`; returns whether the fault
/// killed it (an abnormal exit with no `NOT KILLED`).
fn child(mode: &str, root: &Path, point: &str, env: &[(&str, String)]) -> bool {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        &rdy_qa::test_name!("store_foreign_crash_child"),
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads",
        "1",
    ])
    .env("RPL35_ROOT", root)
    .env("RPL35_MODE", mode)
    .env("RPL35_POINT", point)
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.contains("NOT KILLED") {
        assert!(out.status.success(), "{mode} {point}: {stdout}");
        return false;
    }
    assert!(
        !out.status.success(),
        "{mode} {point}: the child ran no case: {stdout}"
    );
    true
}

// ---------------------------------------------------------------------------------------------
// a delivery

#[test]
fn rpl35_delivery_crash_has_old_or_complete_batch() {
    let dir = tempfile::tempdir().unwrap();
    let c = cited(dir.path());
    let base = citing(dir.path(), &c);
    let old = state(&base.root);
    let old_segments = segments(&base.root);

    // the reference: the delivery, whole
    let full = dir.path().join("full");
    copy_dir(&base.root, &full);
    let entry = {
        let mut r = Rdy::open(&full).unwrap();
        let t = take(&mut r, &c.first, &base.live).unwrap();
        assert!(!t.unchanged && t.added >= 2, "{t:?}");
        r.close().unwrap();
        t.entry
    };
    let complete = state(&full);
    assert_ne!(old, complete);
    assert_eq!(complete.1["sources"][0]["head"], json!(entry.to_string()));

    // the delivery's bytes: one tail of the last segment
    let full_segments = segments(&full);
    assert_eq!(
        full_segments.keys().collect::<Vec<_>>(),
        old_segments.keys().collect::<Vec<_>>(),
        "one segment"
    );
    let (seg, before) = old_segments.iter().next_back().unwrap();
    let after = &full_segments[seg];
    assert!(after.starts_with(before), "the delivery only appends");
    let tail = &after[before.len()..];
    assert!(!tail.is_empty());

    // a partial append, cut at every frame boundary and inside each frame: the old copy until
    // the whole batch, its marker included, is in the log
    let mut cuts = vec![];
    let mut at = 0usize;
    while at + 37 <= tail.len() {
        let len = u32::from_le_bytes(tail[at..at + 4].try_into().unwrap()) as usize;
        cuts.extend([at, at + 1, at + 36, at + 37 + len / 2]);
        at += 37 + len;
    }
    assert_eq!(at, tail.len(), "the tail is whole frames");
    cuts.push(tail.len() - 1);
    cuts.sort();
    cuts.dedup();
    for (i, cut) in cuts.iter().enumerate() {
        let p = dir.path().join(format!("cut-{i}"));
        copy_dir(&base.root, &p);
        let mut bytes = before.clone();
        bytes.extend_from_slice(&tail[..*cut]);
        std::fs::write(store_dir(&p).join("segments").join(seg), bytes).unwrap();
        assert_eq!(state(&p), old, "a cut at byte {cut} of {}", tail.len());
        // the delivery still takes, to the same entry
        let mut r = Rdy::open(&p).unwrap();
        assert_eq!(take(&mut r, &c.first, &base.live).unwrap().entry, entry);
        r.close().unwrap();
        assert_eq!(state(&p).1, complete.1, "cut {cut}: taken again");
    }

    // a kill at each point of the write: the old copy or the complete delivery, never a part;
    // once synced it is the complete delivery
    for point in [
        CrashPoint::BeforeMarker,
        CrashPoint::BeforeSync,
        CrashPoint::AfterSync,
        CrashPoint::AfterIndex,
    ] {
        let p = dir.path().join(format!("kill-{}", point.name()));
        copy_dir(&base.root, &p);
        let killed = child(
            "delivery",
            &p,
            point.name(),
            &[
                ("RPL35_LINK", base.live.to_string()),
                ("RPL35_FILE", c.first.display().to_string()),
            ],
        );
        assert!(killed, "{point:?} is on the delivery's path");
        let now = state(&p);
        assert!(now == old || now == complete, "{point:?}: {now:?}");
        if matches!(point, CrashPoint::AfterSync | CrashPoint::AfterIndex) {
            assert_eq!(now, complete, "{point:?}: synced is durable");
        }
        let mut r = Rdy::open(&p).unwrap();
        let again = take(&mut r, &c.first, &base.live).unwrap();
        assert_eq!(again.entry, entry);
        assert_eq!(again.unchanged, now == complete, "{point:?}");
        r.close().unwrap();
        assert_eq!(state(&p).1, complete.1, "{point:?}: converges");
    }

    // a failed sync: the take fails and writes nothing
    let p = dir.path().join("sync");
    copy_dir(&base.root, &p);
    {
        let mut r = Rdy::open(&p).unwrap();
        r.store_mut().fail_next_sync();
        take(&mut r, &c.first, &base.live).expect_err("the sync fails");
    }
    assert_eq!(state(&p), old, "a failed sync");
    assert_eq!(segments(&p), old_segments);

    // a failed index update after the sync: the old copy or the whole delivery
    let p = dir.path().join("index-apply");
    copy_dir(&base.root, &p);
    {
        let mut r = Rdy::open(&p).unwrap();
        r.store_mut().fail_next_index_apply();
        take(&mut r, &c.first, &base.live).expect_err("the index update fails");
    }
    let now = state(&p);
    assert!(
        now == old || now == complete,
        "a failed index update: {now:?}"
    );

    // a lost index: the open rebuilds it from the log, to the same foreign copy
    let p = dir.path().join("lost-index");
    copy_dir(&full, &p);
    for e in std::fs::read_dir(store_dir(&p)).unwrap() {
        let e = e.unwrap();
        if e.file_name().to_string_lossy().starts_with("index.redb") {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
    {
        let r = Rdy::open(&p).unwrap();
        let report = r.store().open_report().clone();
        assert!(report.caught_up_frames > 0, "{report:?}");
        r.close().unwrap();
    }
    assert_eq!(state(&p), complete, "a lost index");
}

// ---------------------------------------------------------------------------------------------
// a foreign removal

/// The citing project with the first delivery taken on `core`, closed.
fn delivered(dir: &Path, c: &Cited) -> Citing {
    let x = citing(dir, c);
    let mut r = Rdy::open(&x.root).unwrap();
    take(&mut r, &c.first, &x.live).unwrap();
    r.close().unwrap();
    x
}

/// The checks after a removal (finished by an open where it was interrupted): the canary is
/// gone from the log and stays gone through a reopen, a reindex, fsck, compaction, the same
/// delivery taken again, a lost index, and a dump and its load.
fn stays_removed(root: &Path, want: &Value, what: &str) {
    let (_, facts) = state(root);
    assert_eq!(&facts, want, "{what}: the removal finished");
    assert_eq!(canaries(root), 0, "{what}: the bytes are zero");
    let mut r = Rdy::open(root).unwrap();
    r.reindex().unwrap();
    r.fsck().unwrap();
    assert_eq!(canaries(root), 0, "{what}: after a reindex");
    r.store_mut().compact(false).unwrap();
    assert_eq!(canaries(root), 0, "{what}: after compaction");
    r.close().unwrap();
    assert_eq!(state(root).1, *want, "{what}: after compaction");

    // the lost index is rebuilt from the log and its sealed authority
    for e in std::fs::read_dir(store_dir(root)).unwrap() {
        let e = e.unwrap();
        if e.file_name().to_string_lossy().starts_with("index.redb") {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
    assert_eq!(state(root).1, *want, "{what}: a lost index");
    assert_eq!(canaries(root), 0, "{what}: a lost index");

    // a dump and its load carry the removal, never the bytes
    let out = root.with_file_name(format!(
        "{}.rdydump",
        root.file_name().unwrap().to_string_lossy()
    ));
    let r = Rdy::open(root).unwrap();
    r.read().dump(&out).unwrap();
    r.close().unwrap();
    let to = root.with_file_name(format!(
        "{}-loaded",
        root.file_name().unwrap().to_string_lossy()
    ));
    rdy_engine::load::load(&out, &to).unwrap_or_else(|e| panic!("{what}: load: {e:?}"));
    assert_eq!(canaries(&to), 0, "{what}: the loaded store");
    let (_, loaded) = state(&to);
    assert_eq!(loaded["removed_blocks"], want["removed_blocks"], "{what}");
}

#[test]
fn rpl35_removal_crash_never_restores_removed_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let c = cited(dir.path());
    let pre = delivered(dir.path(), &c);
    assert!(canaries(&pre.root) >= 1, "the delivery holds the canary");
    let repo = c.repo.to_string();

    // the reference: the removal, whole
    let full = dir.path().join("full");
    copy_dir(&pre.root, &full);
    {
        let mut r = Rdy::open(&full).unwrap();
        let t = take_foreign_removal(&mut r, &File::open(&c.history).unwrap(), &c.repo).unwrap();
        assert!(!t.unchanged, "{t:?}");
        r.close().unwrap();
    }
    let (_, want) = state(&full);
    assert_eq!(
        want["removed_blocks"].as_array().unwrap().len(),
        1,
        "{want}"
    );
    assert_eq!(canaries(&full), 0);

    // the same delivery taken again, and on a second link, writes no removed byte back
    {
        let mut r = Rdy::open(&full).unwrap();
        assert!(take(&mut r, &c.first, &pre.live).unwrap().unchanged);
        // the second link holds the removed version as the store does: removed, with no bytes
        let mirrored = take(&mut r, &c.first, &pre.mirror).unwrap();
        assert!(!mirrored.unchanged && mirrored.added >= 2, "{mirrored:?}");
        assert_eq!(mirrored.blocks_taken, 0, "{mirrored:?}");
        r.close().unwrap();
        assert_eq!(canaries(&full), 0, "a second link");
    }

    // a kill at each point of the erasure, while the live handle seals and erases
    for point in Failpoint::ALL
        .into_iter()
        .chain([Failpoint::AfterIndexPublish])
    {
        let p = dir.path().join(format!("kill-{}", point.name()));
        copy_dir(&pre.root, &p);
        let killed = child(
            "removal",
            &p,
            point.name(),
            &[
                ("RPL35_REPO", repo.clone()),
                ("RPL35_NTH", "1".into()),
                ("RPL35_FILE", c.history.display().to_string()),
            ],
        );
        if !killed {
            // the point is not on a live take's path: the take finished
            assert_eq!(point, Failpoint::AfterIndexPublish, "{point:?} never hit");
        }
        if point == Failpoint::AfterApplySync {
            assert!(canaries(&p) >= 1, "sealed before its erasure");
        }
        stays_removed(&p, &want, point.name());
    }

    // a kill while an open finishes a sealed removal, then the next open finishes it
    for point in Failpoint::ALL {
        let p = dir.path().join(format!("open-{}", point.name()));
        copy_dir(&pre.root, &p);
        assert!(child(
            "removal",
            &p,
            Failpoint::AfterApplySync.name(),
            &[
                ("RPL35_REPO", repo.clone()),
                ("RPL35_NTH", "1".into()),
                ("RPL35_FILE", c.history.display().to_string()),
            ],
        ));
        let killed = child("open", &p, point.name(), &[]);
        assert!(
            killed || point == Failpoint::AfterApplySync,
            "{point:?}: the open's erasure passes it"
        );
        stays_removed(&p, &want, &format!("open {}", point.name()));
    }
}

// ---------------------------------------------------------------------------------------------
// an adoption

fn adopt(r: &mut Rdy, item: &str, hash: &str) -> Result<Value, RdyError> {
    r.answer(
        &approver(),
        PERSON,
        AnswerRequest {
            r#ref: item.into(),
            option: "adopt".into(),
            card_hash: Some(hash.into()),
            ..AnswerRequest::default()
        },
    )
    .map(|i| serde_json::to_value(i).unwrap())
}

fn the_card(r: &Rdy) -> Item {
    let mut cards: Vec<Item> = pending(r)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Adopt)
        .collect();
    assert_eq!(cards.len(), 1, "{cards:#?}");
    cards.remove(0)
}

/// What an adoption moves: the citing claim's pins, the adopt views, the Adopt answers, the open
/// card and the log end.
fn adoption_state(root: &Path, claim: &BlockId) -> Value {
    let r = Rdy::open(root).unwrap();
    r.fsck().unwrap_or_else(|e| panic!("fsck: {e:?}"));
    let pins: BTreeMap<ForeignId, ForeignPin> =
        r.read().block(&Scope::Canon, claim).unwrap().foreign.pins;
    let views: Vec<String> = r
        .store()
        .refs_with_prefix("view/adopt-")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    let mut adopts = vec![];
    let st = r.read();
    st.reader()
        .for_each_object_of(kinds::ANSWER, |h| {
            let a: rdy_model::Answer = st.reader().get_typed(kinds::ANSWER, &h)?;
            if a.act == AnswerAct::Adopt {
                adopts.push(h.to_string());
            }
            Ok(())
        })
        .unwrap();
    drop(st);
    let cards: Vec<Value> = pending(&r)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Adopt)
        .map(|i| json!([i.r#ref, i.card_hash]))
        .collect();
    let end = r.store().log_end();
    let canon = r.read().canon_head().map(|h| h.to_string());
    r.close().unwrap();
    json!({
        "pins": pins.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        "views": views,
        "adopts": adopts,
        "cards": cards,
        "canon": canon,
        "end": [end.segment, end.offset],
    })
}

#[test]
fn rpl35_adopt_failure_changes_no_durable_state() {
    let dir = tempfile::tempdir().unwrap();
    let c = cited(dir.path());
    let x = delivered(dir.path(), &c);
    // a landed claim cites `kept`; the second delivery changes it: one adopt card
    let claim = {
        let mut r = Rdy::open(&x.root).unwrap();
        let w = put(
            &mut r,
            "cite",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": x.section.to_string(),
                    "body": format!("Our jobs follow {{{{core:{}}}}}.", c.kept)}]),
        );
        landed(&mut r, "cite");
        // an accepted decision cites it too: the adoption's frozen-decision exception
        let d = r
            .decide(
                &writer(),
                "dec",
                DecisionInput {
                    parent: x.section.to_string(),
                    statement: format!("Every job logs as {{{{core:{}}}}} says.", c.kept),
                    title: Some("Log time".into()),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("decide: {e:?}"));
        assert!(d.created.contains_key("$decision"));
        landed(&mut r, "dec");
        take(&mut r, &c.second, &x.live).unwrap();
        r.close().unwrap();
        bid(&w, "$c")
    };
    let before = adoption_state(&x.root, &claim);
    let files = durable(&x.root);
    let (item, hash) = {
        let r = Rdy::open(&x.root).unwrap();
        let card = the_card(&r);
        r.close().unwrap();
        (card.r#ref, card.card_hash)
    };
    assert_eq!(before["cards"], json!([[item, hash]]));

    // each failure, in the process: an error, and nothing durable
    type Fault = fn(&mut Rdy);
    let faults: [(&str, Fault); 4] = [
        ("a partial append", |r| {
            r.store_mut().fail_append_at_frame_for_tests(Some((1, 7)))
        }),
        ("a torn first frame", |r| {
            r.store_mut().fail_next_append_after(3)
        }),
        ("a failed sync", |r| r.store_mut().fail_next_sync()),
        ("a failed index update", |r| {
            r.store_mut().fail_next_index_apply()
        }),
    ];
    for (what, arm) in faults {
        let p = dir.path().join(what.replace(' ', "-"));
        copy_dir(&x.root, &p);
        {
            let mut r = Rdy::open(&p).unwrap();
            arm(&mut r);
            let e = adopt(&mut r, &item, &hash).expect_err(what);
            assert!(!e.code.is_empty(), "{what}: {e:?}");
        }
        assert_eq!(adoption_state(&p, &claim), before, "{what}");
        assert_eq!(durable(&p), files, "{what}: no log byte");
        // the same card still adopts after the reopen
        let mut r = Rdy::open(&p).unwrap();
        adopt(&mut r, &item, &hash).unwrap_or_else(|e| panic!("{what}: adopt: {e:?}"));
        r.close().unwrap();
        let after = adoption_state(&p, &claim);
        assert_ne!(after["pins"], before["pins"], "{what}: the pin moves");
        assert_eq!(after["adopts"].as_array().unwrap().len(), 1, "{what}");
    }

    // a kill before the batch is sealed: nothing durable either
    let p = dir.path().join("kill");
    copy_dir(&x.root, &p);
    assert!(child(
        "adopt",
        &p,
        CrashPoint::BeforeMarker.name(),
        &[("RPL35_REF", item.clone()), ("RPL35_HASH", hash.clone())],
    ));
    assert_eq!(
        adoption_state(&p, &claim),
        before,
        "a kill before the marker"
    );
    let mut r = Rdy::open(&p).unwrap();
    adopt(&mut r, &item, &hash).unwrap();
    r.close().unwrap();
}
