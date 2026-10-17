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
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint_nth};
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
            let items = decode_items(&std::fs::read(file).unwrap());
            let _ = s.seal_redaction_batch_for_tests(items);
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
