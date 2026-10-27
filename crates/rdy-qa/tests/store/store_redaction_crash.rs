//! Kills during an authorized content removal (plan Redaction, Task 4; D-600CW, D-3M81W): a child
//! process is aborted at each named failpoint of the removal, both while an open finishes a
//! freshly authorized batch and while a live handle seals one; the parent reopens the store and
//! checks the removal finished with the same landing and the same stubs.
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::{CONTENT_REMOVED, DropSpan, RedactTarget};
use rdy_model::{Block, BlockId, Change, ChangeLinks, Hash, Op, Provenance, Role};
use rdy_store::Store;
use rdy_store::kinds::{BATCH_COMMIT, BLOCK};
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint, arm_failpoint_nth};
use redaction_fixture::{
    FixtureOptions, Proof, RequestSpec, authorized_batch_fixture, segment_bytes,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HEADER: usize = 37;

fn secret() -> Block {
    Block {
        id: BlockId::parse("b-00001").unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "canary kill secret".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

/// A closed store at `path` with the secret block committed to view `work`.
fn content(path: &Path) {
    let mut s = Store::create(path).unwrap();
    s.commit_to_view(
        "work",
        Change {
            view: "work".into(),
            ops: vec![Op::Create { block: secret() }],
            deps: vec![],
            provenance: Provenance {
                actor: "agent:w".into(),
                run: None,
                role: Role::Writer,
                at_ms: 1,
            },
            justification: None,
            message: String::new(),
            links: ChangeLinks::default(),
            read_identity: None,
        },
    )
    .unwrap();
    let mut chunked = b"kill blob canary ".repeat(70_000);
    chunked.truncate(1024 * 1024 + 10);
    let blob = s.put_blob(&chunked).unwrap();
    std::fs::write(path.with_file_name("blob"), blob.0).unwrap();
}

fn spec(path: &Path) -> RequestSpec {
    let blob = Hash(
        std::fs::read(path.with_file_name("blob"))
            .unwrap()
            .try_into()
            .unwrap(),
    );
    RequestSpec::new(
        "work",
        vec![
            RedactTarget::Version {
                id: secret().id,
                version: secret().version_hash(),
            },
            RedactTarget::Blob(blob),
        ],
    )
}

fn zeroed(bytes: &BTreeMap<String, Vec<u8>>, spans: &[DropSpan]) -> BTreeMap<String, Vec<u8>> {
    let mut out = bytes.clone();
    for s in spans {
        let file = out
            .get_mut(&format!("seg-{:06}.log", s.place.segment))
            .unwrap();
        let start = s.place.offset as usize + HEADER;
        file[start..start + s.len as usize].fill(0);
    }
    out
}

/// The landing batch's data frames from `from` in a segment file, by header: (kind, payload).
fn items_from(bytes: &[u8], from: u64) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    let mut at = 0usize;
    while at + HEADER <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = bytes[at + 4];
        if at as u64 >= from && kind != BATCH_COMMIT {
            out.push((kind, bytes[at + HEADER..at + HEADER + len].to_vec()));
        }
        at += HEADER + len;
    }
    out
}

fn encode_items(items: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![];
    for (kind, payload) in items {
        out.push(*kind);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
    }
    out
}

fn decode_items(mut bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    while !bytes.is_empty() {
        let kind = bytes[0];
        let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        out.push((kind, bytes[5..5 + len].to_vec()));
        bytes = &bytes[5 + len..];
    }
    out
}

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

#[test]
#[ignore = "rdx4_kills_complete_the_authorized_erase runs this in a child process"]
fn store_redaction_crash_child() {
    let Ok(dir) = std::env::var("RDX4_DIR") else {
        return;
    };
    let point = Failpoint::parse(&std::env::var("RDX4_POINT").unwrap()).expect("a failpoint");
    let items = std::env::var("RDX4_ITEMS").ok();
    let nth = std::env::var("RDX4_NTH").map_or(1, |n| n.parse().unwrap());
    arm_failpoint_nth(point, FailAction::Abort, nth);
    let path = PathBuf::from(dir);
    match items {
        // the open finishes the authorized batch the fixture wrote
        None => drop(Store::open(&path)),
        // a live handle seals the landing batch
        Some(file) => {
            let mut s = Store::open(&path).unwrap();
            // a reader that outlives the removal, as a live daemon's read state can
            let held = std::env::var("RDB7_HOLD_READER")
                .is_ok()
                .then(|| s.reader());
            let items = decode_items(&std::fs::read(file).unwrap());
            let _ = s.seal_redaction_batch_for_tests(items);
            drop(held);
        }
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "NOT KILLED").unwrap();
    out.flush().unwrap();
}

/// Runs the child on `path`; panics unless the failpoint killed it (at its `nth` hit).
fn kill_at(path: &Path, point: Failpoint, items: Option<&Path>, nth: u32) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        &rdy_qa::test_name!("store_redaction_crash_child"),
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads",
        "1",
    ])
    .env("RDX4_DIR", path)
    .env("RDX4_POINT", point.name())
    .env("RDX4_NTH", nth.to_string())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if let Some(i) = items {
        cmd.env("RDX4_ITEMS", i);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success() && !stdout.contains("NOT KILLED"),
        "{point:?}: the child was not killed: {stdout}"
    );
}

/// Runs the child on `path` as a live handle that holds a reader while it seals `items`; panics
/// unless the failpoint killed it.
fn kill_with_reader_at(path: &Path, point: Failpoint, items: &Path) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            &rdy_qa::test_name!("store_redaction_crash_child"),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("RDX4_DIR", path)
        .env("RDX4_POINT", point.name())
        .env("RDX4_NTH", "1")
        .env("RDX4_ITEMS", items)
        .env("RDB7_HOLD_READER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success() && !stdout.contains("NOT KILLED"),
        "{point:?}: the child was not killed: {stdout}"
    );
}

/// The reopened store finished the removal: the same landing, the stub, the blob refused, and
/// the log equal to the authorized log with exactly the spans zeroed.
fn assert_finished(
    path: &Path,
    landing: Option<Hash>,
    request: Hash,
    full: &BTreeMap<String, Vec<u8>>,
    spans: &[DropSpan],
    point: Failpoint,
) {
    let mut s = Store::open(path).unwrap_or_else(|e| panic!("{point:?}: reopen: {e}"));
    assert_eq!(
        s.canon_head().unwrap(),
        landing,
        "{point:?}: the same landing"
    );
    let stub = s.get_block(&secret().version_hash()).unwrap();
    assert_eq!(stub.body, CONTENT_REMOVED, "{point:?}");
    assert_eq!(stub.redaction(), Some(request), "{point:?}");
    assert_eq!(
        s.redaction_of(BLOCK, &secret().version_hash()).unwrap(),
        Some(request)
    );
    let counts = s.finish_redactions().unwrap();
    assert_eq!(counts.frames, spans.len() as u64, "{point:?}");
    assert_eq!(
        counts.checked_payloads, 0,
        "{point:?}: nothing left to erase"
    );
    let report = s.verify_report().unwrap();
    assert_eq!(report.redaction.unwrap().frames, spans.len() as u64);
    s.check_objects().unwrap();
    drop(s);
    assert_eq!(segment_bytes(path), zeroed(full, spans), "{point:?}");
}

#[test]
fn rdx4_kills_complete_the_authorized_erase() {
    for point in Failpoint::ALL {
        // the open that finishes a freshly authorized batch
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        content(&path);
        let f = authorized_batch_fixture(
            &path,
            &[spec(&path)],
            &FixtureOptions::approved(Proof::local_human()),
        );
        let spans = f.requests[0].draft.request.manifest.spans.clone();
        let full = segment_bytes(&path);
        kill_at(&path, point, None, 1);
        assert_finished(
            &path,
            f.landing,
            f.requests[0].request,
            &full,
            &spans,
            point,
        );

        // a live handle that seals the landing batch
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        content(&path);
        let full_path = dir.path().join("full");
        copy_dir(&path, &full_path);
        let f = authorized_batch_fixture(
            &full_path,
            &[spec(&path)],
            &FixtureOptions::approved(Proof::relayed_human()),
        );
        let spans = f.requests[0].draft.request.manifest.spans.clone();
        let full = segment_bytes(&full_path);
        let start = f.landing_start.unwrap();
        let name = format!("seg-{:06}.log", start.segment);
        let prep = dir.path().join("prep");
        copy_dir(&full_path, &prep);
        let seg = std::fs::OpenOptions::new()
            .write(true)
            .open(prep.join("segments").join(&name))
            .unwrap();
        seg.set_len(start.offset).unwrap();
        drop(seg);
        let items_file = dir.path().join("items");
        std::fs::write(
            &items_file,
            encode_items(&items_from(&full[&name], start.offset)),
        )
        .unwrap();
        kill_at(&prep, point, Some(&items_file), 1);
        assert_finished(
            &prep,
            f.landing,
            f.requests[0].request,
            &full,
            &spans,
            point,
        );
    }

    // a kill inside the 1 MiB chunk's payload: the chunk is partly zero, and the reopen zeroes
    // the rest
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    content(&path);
    let f = authorized_batch_fixture(
        &path,
        &[spec(&path)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let spans = f.requests[0].draft.request.manifest.spans.clone();
    let full = segment_bytes(&path);
    let mut sorted = spans.clone();
    sorted.sort_by_key(|s| (s.place.segment, s.place.offset));
    let big = sorted
        .iter()
        .position(|s| u64::from(s.len) > 3 * BUFFER)
        .expect("a span longer than three buffers");
    let nth = sorted[..big]
        .iter()
        .map(|s| u64::from(s.len).div_ceil(BUFFER) as u32)
        .sum::<u32>()
        + 3;
    kill_at(&path, Failpoint::AfterZeroWrite, None, nth);
    let chunk = &sorted[big];
    let name = format!("seg-{:06}.log", chunk.place.segment);
    let start = chunk.place.offset as usize + HEADER;
    let cut = 3 * BUFFER as usize;
    let mut want = zeroed(&full, &sorted[..big]);
    want.get_mut(&name).unwrap()[start..start + cut].fill(0);
    let now = segment_bytes(&path);
    assert_eq!(
        now, want,
        "killed three buffers into the chunk: only the earlier spans and those buffers are zero"
    );
    assert!(
        now[&name][start + cut..start + chunk.len as usize]
            .iter()
            .any(|b| *b != 0),
        "the chunk is partly zero"
    );
    assert_finished(
        &path,
        f.landing,
        f.requests[0].request,
        &full,
        &spans,
        Failpoint::AfterZeroWrite,
    );
}

/// The erase buffer: one zero write covers at most this many bytes.
const BUFFER: u64 = 65_536;

/// A tag 1 diagram cache row whose payload is the secret's text, as the cache writes one.
fn secret_row() -> (Vec<u8>, Vec<u8>) {
    let mut key = vec![0xAB; 32];
    key.extend_from_slice(b"mmdflux 2.6.1\0test");
    let payload = secret().body.into_bytes();
    let mut value = vec![1u8];
    value.extend_from_slice(&Hash::of(&payload).0);
    value.extend_from_slice(&payload);
    (key, value)
}

/// A live handle's landing of an authorized removal, prepared as
/// rdx4_kills_complete_the_authorized_erase prepares it, over a store whose index holds one
/// diagram cache row with the secret's text (checkpointed, closed cleanly).
struct LiveLanding {
    _dir: tempfile::TempDir,
    prep: PathBuf,
    items_file: PathBuf,
    landing: Option<Hash>,
    request: Hash,
    full: BTreeMap<String, Vec<u8>>,
    spans: Vec<DropSpan>,
}

fn live_landing() -> LiveLanding {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    content(&path);
    {
        let mut s = Store::open(&path).unwrap();
        let (key, value) = secret_row();
        s.reader().diagram_cache().put_raw_for_tests(&key, &value);
        assert_eq!(s.reader().diagram_cache().rows_for_tests(), 1);
        s.checkpoint().unwrap();
        s.close().unwrap();
    }
    let full_path = dir.path().join("full");
    copy_dir(&path, &full_path);
    let f = authorized_batch_fixture(
        &full_path,
        &[spec(&path)],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let spans = f.requests[0].draft.request.manifest.spans.clone();
    let full = segment_bytes(&full_path);
    let start = f.landing_start.unwrap();
    let name = format!("seg-{:06}.log", start.segment);
    let prep = dir.path().join("prep");
    copy_dir(&full_path, &prep);
    let seg = std::fs::OpenOptions::new()
        .write(true)
        .open(prep.join("segments").join(&name))
        .unwrap();
    seg.set_len(start.offset).unwrap();
    drop(seg);
    let items_file = dir.path().join("items");
    std::fs::write(
        &items_file,
        encode_items(&items_from(&full[&name], start.offset)),
    )
    .unwrap();
    {
        let s = Store::open(&prep).unwrap();
        assert_eq!(
            s.reader().diagram_cache().rows_for_tests(),
            1,
            "the row is there before the removal"
        );
    }
    LiveLanding {
        landing: f.landing,
        request: f.requests[0].request,
        _dir: dir,
        prep,
        items_file,
        full,
        spans,
    }
}

/// Every file under `dir` whose bytes hold the secret's text.
fn files_with_secret(dir: &Path) -> Vec<PathBuf> {
    let mut found = vec![];
    let mut files = vec![dir.to_path_buf()];
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        } else if holds_secret(&std::fs::read(&p).unwrap()) {
            found.push(p);
        }
    }
    found.sort();
    found
}

fn holds_secret(bytes: &[u8]) -> bool {
    let secret = secret().body.into_bytes();
    bytes.windows(secret.len()).any(|w| w == secret)
}

/// Every byte an open handle reads now, from its start.
fn read_through(f: &std::fs::File) -> Vec<u8> {
    use std::io::{Read, Seek};
    let mut f = f;
    f.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut out = vec![];
    f.read_to_end(&mut out).unwrap();
    out
}

/// rdb7 (D-XM8MP rule 13a, D-6H0ZX; ruling task-7 Round 2, F3): a kill after the removal's
/// segment sync, or before its new index is published, keeps the old index file. The next open
/// sees the unclean shutdown and a catch-up that replays the removal's apply, so it runs the
/// removal's reindex: no row survives, and no file of the store holds the removed text, while
/// the reopened store is open and after it closes.
#[test]
fn rdb7_a_killed_removal_keeps_no_drawing() {
    for point in [Failpoint::AfterSegmentSync, Failpoint::BeforeIndexPublish] {
        let l = live_landing();
        kill_at(&l.prep, point, Some(&l.items_file), 1);
        {
            let s = Store::open(&l.prep).unwrap_or_else(|e| panic!("{point:?}: reopen: {e}"));
            assert!(s.open_report().unclean_shutdown, "{point:?}");
            // ruling task-7 Round 3, N4: the reindex the mark triggers is this open's rebuild
            assert!(
                s.open_report().index_rebuilt,
                "{point:?}: the report says so"
            );
            let c = s.reader().diagram_cache();
            assert_eq!(c.rows_for_tests(), 0, "{point:?}: no row survives");
            assert_eq!(c.bytes_for_tests(), Some(0), "{point:?}");
            assert_eq!(
                files_with_secret(&l.prep),
                Vec::<PathBuf>::new(),
                "{point:?}: no file holds the removed text while the store is open"
            );
            s.close().unwrap();
        }
        assert_eq!(
            files_with_secret(&l.prep),
            Vec::<PathBuf>::new(),
            "{point:?}: no file holds the removed text after the close"
        );
        assert_finished(&l.prep, l.landing, l.request, &l.full, &l.spans, point);
    }
}

/// rdb7 (D-6H0ZX; ruling task-7 Round 2, F1): a reader holds the old index while a live
/// handle's removal publishes the new one, and the process is killed after the rename, before
/// the reader drops. The old file stays reachable as `index.redb.erase`, and the next open
/// zeroes and removes it: the old inode, read through a handle taken before, holds no removed
/// text, and no file of the store does.
#[test]
fn rdb7_a_killed_removal_with_a_reader_held_leaves_no_copy() {
    let l = live_landing();
    let old = std::fs::File::open(l.prep.join("index.redb")).unwrap();
    assert!(
        holds_secret(&read_through(&old)),
        "the old index holds the row"
    );
    kill_with_reader_at(&l.prep, Failpoint::AfterIndexPublish, &l.items_file);
    assert!(
        l.prep.join("index.redb.erase").exists(),
        "the old index is kept by name until it is zeroed"
    );
    {
        let s = Store::open(&l.prep).unwrap();
        assert!(!l.prep.join("index.redb.erase").exists());
        assert_eq!(s.reader().diagram_cache().rows_for_tests(), 0);
        s.close().unwrap();
    }
    assert!(
        !holds_secret(&read_through(&old)),
        "the old inode holds no removed text"
    );
    assert_eq!(files_with_secret(&l.prep), Vec::<PathBuf>::new());
    assert_finished(
        &l.prep,
        l.landing,
        l.request,
        &l.full,
        &l.spans,
        Failpoint::AfterIndexPublish,
    );
}

/// rdb7 (D-6H0ZX; ruling task-7 Round 2, F2): a removal whose reindex fails (at the segment sync
/// or before the new index is published) leaves the store pending; its close removes the old
/// index, and zeroes and syncs it first. No file of the store holds the removed text, nor does
/// the old inode, read through a handle taken before.
#[test]
fn rdb7_a_failed_removal_close_zeroes_the_old_index() {
    for point in [Failpoint::AfterSegmentSync, Failpoint::BeforeIndexPublish] {
        let l = live_landing();
        let old = std::fs::File::open(l.prep.join("index.redb")).unwrap();
        let mut s = Store::open(&l.prep).unwrap();
        let items = decode_items(&std::fs::read(&l.items_file).unwrap());
        arm_failpoint_nth(point, FailAction::Fail, 1);
        let sealed = s.seal_redaction_batch_for_tests(items);
        arm_failpoint(None);
        assert!(
            matches!(sealed, Err(rdy_store::StoreError::RedactionPending)),
            "{point:?}: {sealed:?}"
        );
        assert!(
            s.close().is_err(),
            "{point:?}: a pending store's close fails"
        );
        assert!(!l.prep.join("index.redb").exists(), "{point:?}");
        assert!(!l.prep.join("index.redb.erase").exists(), "{point:?}");
        assert!(
            !holds_secret(&read_through(&old)),
            "{point:?}: the old inode holds no removed text"
        );
        assert_eq!(
            files_with_secret(&l.prep),
            Vec::<PathBuf>::new(),
            "{point:?}"
        );
        assert_finished(&l.prep, l.landing, l.request, &l.full, &l.spans, point);
    }
}

/// rdb7 (ruling task-7 item 2, F5): when the late zeroing of an old index that a reader held
/// fails, the store logs one line naming the file's former path and the error. The file stays
/// reachable by name, and the next open zeroes and removes it.
#[test]
fn rdb7_a_failed_late_zeroing_is_logged_with_the_former_path() {
    let l = live_landing();
    let index = l.prep.join("index.redb");
    let old = std::fs::File::open(&index).unwrap();
    let mut s = Store::open(&l.prep).unwrap();
    let reader = s.reader();
    let items = decode_items(&std::fs::read(&l.items_file).unwrap());
    s.seal_redaction_batch_for_tests(items).unwrap();
    let failing = rdy_store::fsutil::fail_zeroing_for_tests(&index);
    let capture = rdy_store::capture_notices_for_tests(&[&index.display().to_string()]);
    drop(reader);
    let lines = capture.notices();
    drop(capture);
    drop(failing);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(&index.display().to_string())
            && lines[0].contains("injected zeroing failure"),
        "{lines:?}"
    );
    assert!(l.prep.join("index.redb.erase").exists());
    s.close().unwrap();
    let s = Store::open(&l.prep).unwrap();
    assert!(!l.prep.join("index.redb.erase").exists());
    assert!(!holds_secret(&read_through(&old)));
    drop(s);
    assert_eq!(files_with_secret(&l.prep), Vec::<PathBuf>::new());
}

/// rdb7 (ruling task-7 Round 2, F6): an index file with a second link is not zeroed (the zeroing
/// is for rdy's own index file only), and the store logs it; the removal still finishes.
#[test]
fn rdb7_a_removal_skips_zeroing_an_index_with_two_links() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    content(&path);
    let f = authorized_batch_fixture(
        &path,
        &[spec(&path)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let index = path.join("index.redb");
    let other = dir.path().join("other-link");
    std::fs::hard_link(&index, &other).unwrap();
    let before = std::fs::read(&other).unwrap();
    let capture = rdy_store::capture_notices_for_tests(&[&index.display().to_string()]);
    let (s, zeroed) = rdy_store::fsutil::record_zeroed(|| Store::open(&path).unwrap());
    let lines = capture.notices();
    drop(capture);
    assert_eq!(zeroed, vec![], "nothing is zeroed");
    let after = std::fs::read(&other).unwrap();
    assert!(after.len() >= before.len());
    assert!(
        after.iter().any(|b| *b != 0),
        "the other link's file is not zeroed"
    );
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("more than one link"), "{lines:?}");
    assert_eq!(
        s.get_block(&secret().version_hash()).unwrap().redaction(),
        Some(f.requests[0].request)
    );
    s.close().unwrap();
}

/// rdb7 (D-6H0ZX): a removal overwrites the replaced index file with zeros, and syncs it, before
/// the new index takes its name; the store keeps no copy of the removed text or its drawing.
#[test]
fn rdb7_a_removal_zeroes_the_old_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    content(&path);
    {
        let mut s = Store::open(&path).unwrap();
        let (key, value) = secret_row();
        s.reader().diagram_cache().put_raw_for_tests(&key, &value);
        s.checkpoint().unwrap();
        s.close().unwrap();
    }
    let index = path.join("index.redb");
    let before = std::fs::metadata(&index).unwrap().len();
    let f = authorized_batch_fixture(
        &path,
        &[spec(&path)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    // the open finishes the removal: its reindex replaces the old index
    let (s, zeroed) = rdy_store::fsutil::record_zeroed(|| Store::open(&path).unwrap());
    assert_eq!(zeroed.len(), 1, "{zeroed:?}");
    assert_eq!(zeroed[0].path, index);
    assert!(zeroed[0].len >= before, "{zeroed:?}");
    assert!(zeroed[0].all_zero, "every byte is zero before the unlink");
    assert_eq!(s.reader().diagram_cache().rows_for_tests(), 0);
    assert_eq!(
        s.get_block(&secret().version_hash()).unwrap().redaction(),
        Some(f.requests[0].request)
    );
    s.close().unwrap();
    // no file of the store holds the removed text
    let secret = secret().body.into_bytes();
    let mut files = vec![path.clone()];
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        } else {
            let bytes = std::fs::read(&p).unwrap();
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret),
                "{} holds the removed text",
                p.display()
            );
        }
    }
}

/// rdb7 (ruling task-7 Round 3, N1; the re-review's probe D): a reader outlives its store's
/// close, so the old index stays kept as `index.redb.erase`. The next open zeroes and removes
/// that name, and a later removal's kept file takes it. When the first reader drops, its zeroing
/// leaves the name it no longer owns, and logs one line: the later removal's file survives until
/// its own zeroing runs.
#[test]
fn rdb7_a_late_eraser_leaves_a_name_it_no_longer_owns() {
    let l = live_landing();
    let erase = l.prep.join("index.redb.erase");
    let mut s = Store::open(&l.prep).unwrap();
    let reader = s.reader();
    let items = decode_items(&std::fs::read(&l.items_file).unwrap());
    s.seal_redaction_batch_for_tests(items).unwrap();
    s.close().unwrap();
    assert!(erase.exists(), "kept while the reader lives");
    let s = Store::open(&l.prep).unwrap();
    assert!(
        !erase.exists(),
        "the reopen zeroed and removed the kept name"
    );
    // a later removal's old index, kept under the same free name
    let later = b"a later removal's old index".to_vec();
    std::fs::write(&erase, &later).unwrap();
    let capture = rdy_store::capture_notices_for_tests(&[&erase.display().to_string()]);
    drop(reader);
    let lines = capture.notices();
    drop(capture);
    assert!(erase.exists(), "the later removal's kept name survives");
    assert_eq!(
        std::fs::read(&erase).unwrap(),
        later,
        "and its file is untouched"
    );
    assert_eq!(lines.len(), 1, "{lines:?}");
    drop(s);
    assert_eq!(files_with_secret(&l.prep), Vec::<PathBuf>::new());
}

/// A closed store at `<dir>/s` with a regular file left as `index.redb.erase`.
fn store_with_leftover(dir: &Path) -> (PathBuf, PathBuf) {
    let path = dir.join("s");
    content(&path);
    let erase = path.join("index.redb.erase");
    std::fs::write(&erase, b"a kept index file").unwrap();
    (path, erase)
}

/// rdb7 (ruling task-7 Round 3, N3): the open checks the leftover's links on the handle it
/// zeroes through, not on the path: a file swapped in after the name was read, which has
/// another link, is not zeroed, and the open logs it.
#[test]
fn rdb7_a_leftover_swapped_for_a_shared_file_is_not_zeroed() {
    let dir = tempfile::tempdir().unwrap();
    let (path, erase) = store_with_leftover(dir.path());
    let other = dir.path().join("other");
    let text = b"not rdy's file".to_vec();
    std::fs::write(&other, &text).unwrap();
    let swap = dir.path().join("swap");
    let swapped = std::sync::atomic::AtomicBool::new(false);
    let (at, from) = (erase.clone(), other.clone());
    let hook = rdy_store::on_leftover_checked_for_tests(move |p| {
        if p == at && !swapped.swap(true, std::sync::atomic::Ordering::SeqCst) {
            std::fs::hard_link(&from, &swap).unwrap();
            std::fs::rename(&swap, &at).unwrap();
        }
    });
    let capture = rdy_store::capture_notices_for_tests(&[&erase.display().to_string()]);
    let s = Store::open(&path);
    let lines = capture.notices();
    drop(capture);
    drop(hook);
    let s = s.unwrap_or_else(|e| panic!("open: {e}"));
    assert_eq!(
        std::fs::read(&other).unwrap(),
        text,
        "the shared file is not zeroed"
    );
    assert!(erase.exists(), "the name is left as it is");
    assert_eq!(lines.len(), 1, "{lines:?}");
    drop(s);
}

/// rdb7 (ruling task-7 Round 3, N5): an open that a leftover it cannot zero refuses names the
/// leftover file in its error text.
#[test]
fn rdb7_an_open_refused_by_a_leftover_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (path, erase) = store_with_leftover(dir.path());
    let failing = rdy_store::fsutil::fail_zeroing_for_tests(&erase);
    let opened = Store::open(&path);
    drop(failing);
    let Err(e) = opened else {
        panic!("the open is refused");
    };
    let text = e.to_string();
    assert!(text.contains(&erase.display().to_string()), "{text}");
    assert!(text.contains("injected zeroing failure"), "{text}");
}

/// rdb7 (ruling task-7 Round 3, the mark at every open; D-6H0ZX): a copy of a cleanly closed
/// store whose log holds a finished removal, with an index older than that removal (a copy or a
/// restore). The open is clean, but its catch-up replays the removal's apply, so it runs the
/// removal's reindex and reports the rebuild: no file of the copy holds the removed text, while
/// it is open and after it closes.
#[test]
fn rdb7_a_copy_whose_log_replays_a_removal_keeps_no_drawing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    content(&path);
    {
        let mut s = Store::open(&path).unwrap();
        let (key, value) = secret_row();
        s.reader().diagram_cache().put_raw_for_tests(&key, &value);
        s.checkpoint().unwrap();
        s.close().unwrap();
    }
    let old_index = dir.path().join("old-index.redb");
    std::fs::copy(path.join("index.redb"), &old_index).unwrap();
    assert!(holds_secret(&std::fs::read(&old_index).unwrap()));
    let f = authorized_batch_fixture(
        &path,
        &[spec(&path)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    // the open finishes the removal, and the store closes cleanly
    Store::open(&path).unwrap().close().unwrap();
    assert_eq!(files_with_secret(&path), Vec::<PathBuf>::new());
    let copy = dir.path().join("copy");
    copy_dir(&path, &copy);
    std::fs::copy(&old_index, copy.join("index.redb")).unwrap();
    {
        let s = Store::open(&copy).unwrap();
        assert_eq!(
            files_with_secret(&copy),
            Vec::<PathBuf>::new(),
            "no file holds the removed text while the copy is open"
        );
        let report = s.open_report();
        assert!(!report.unclean_shutdown, "a clean open");
        assert!(report.index_rebuilt, "the mark's reindex is reported");
        assert_eq!(s.reader().diagram_cache().rows_for_tests(), 0);
        assert_eq!(
            s.get_block(&secret().version_hash()).unwrap().redaction(),
            Some(f.requests[0].request)
        );
        s.close().unwrap();
    }
    assert_eq!(
        files_with_secret(&copy),
        Vec::<PathBuf>::new(),
        "no file holds the removed text after the close"
    );
}
