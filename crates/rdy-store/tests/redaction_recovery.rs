//! Payload erasure and recovery of the authorized operation (plan Redaction, Task 4; D-3M81W:
//! rdy "drops their bytes and keeps their hashes"; D-600CW: the record never leaves the log).
//!
//! Every case writes its authorized batch with the shared raw fixture
//! (`redaction_fixture::authorized_batch_fixture`) and lets the open, or the live test writer,
//! finish the removal. The kill case lives in the QA store binary (`store_redaction_crash`).
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::{CONTENT_REMOVED, DropSpan, RedactTarget};
use rdy_model::*;
use rdy_store::kinds::{self, BATCH_COMMIT, BLOB_CHUNK, BLOCK, CHANGE};
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint, arm_failpoint_nth};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};
use redaction_fixture::{
    Fixture, FixtureOptions, LogFrame, Proof, RequestSpec, authorized_batch_fixture, build_request,
    frames, segment_bytes,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const HEADER: usize = 37;

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

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: Some("w1".into()),
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn version(b: &Block) -> RedactTarget {
    RedactTarget::Version {
        id: b.id.clone(),
        version: b.version_hash(),
    }
}

/// A closed store holding one change `c1` that creates the secret block `a` and the bystander
/// block `b`, and a two-chunk blob whose bytes hold a canary.
struct Setup {
    _dir: tempfile::TempDir,
    path: PathBuf,
    a: Block,
    b: Block,
    c1: Hash,
    blob: Hash,
}

impl Setup {
    fn new() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let mut s = Store::create(&path).unwrap();
        let a = block("b-00001", "canary alpha secret");
        let b = block("b-00002", "bravo stays");
        let c1 = s
            .commit_to_view(
                "work",
                change(
                    "work",
                    vec![
                        Op::Create { block: a.clone() },
                        Op::Create { block: b.clone() },
                    ],
                ),
            )
            .unwrap();
        let mut bytes = b"blob canary ".repeat(100_000);
        bytes.truncate(1024 * 1024 + 4096); // two chunks
        let blob = s.put_blob(&bytes).unwrap();
        drop(s);
        Setup {
            _dir: dir,
            path,
            a,
            b,
            c1,
            blob,
        }
    }

    fn spec(&self) -> RequestSpec {
        RequestSpec::new(
            "work",
            vec![version(&self.a), RedactTarget::Blob(self.blob)],
        )
    }

    /// The request the fixture would build now, with its true spans.
    fn draft_spans(&self) -> Vec<DropSpan> {
        let s = Store::open(&self.path).unwrap();
        build_request(&s, &self.spec()).request.manifest.spans
    }

    fn apply(&self, opts: &FixtureOptions) -> Fixture {
        authorized_batch_fixture(&self.path, &[self.spec()], opts)
    }
}

/// The spans the fixture's (single) request names.
fn spans_of(f: &Fixture) -> Vec<DropSpan> {
    f.requests[0].draft.request.manifest.spans.clone()
}

fn seg_name(seg: u32) -> String {
    format!("seg-{seg:06}.log")
}

/// `bytes` with the payload of every span zeroed.
fn zeroed(bytes: &BTreeMap<String, Vec<u8>>, spans: &[DropSpan]) -> BTreeMap<String, Vec<u8>> {
    let mut out = bytes.clone();
    for s in spans {
        let file = out.get_mut(&seg_name(s.place.segment)).unwrap();
        let start = s.place.offset as usize + HEADER;
        file[start..start + s.len as usize].fill(0);
    }
    out
}

/// Zeroes the payload of every span in the files of the closed store at `path`, as an erasure
/// would.
fn zero_in_files(path: &Path, spans: &[DropSpan]) {
    let bytes = zeroed(&segment_bytes(path), spans);
    for (name, b) in bytes {
        std::fs::write(path.join("segments").join(name), b).unwrap();
    }
}

/// Each frame of a segment file by its header alone: (offset, kind, hash, length).
fn headers(bytes: &[u8]) -> Vec<(u64, u8, Hash, u32)> {
    let mut out = vec![];
    let mut at = 0usize;
    while at + HEADER <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let kind = bytes[at + 4];
        let hash = Hash(bytes[at + 5..at + HEADER].try_into().unwrap());
        out.push((at as u64, kind, hash, len));
        at += HEADER + len as usize;
    }
    assert_eq!(at, bytes.len(), "the file ends at a frame boundary");
    out
}

/// The data frames of a segment file from `from`, by header: (kind, payload).
fn items_from(bytes: &[u8], from: u64) -> Vec<(u8, Vec<u8>)> {
    headers(bytes)
        .into_iter()
        .filter(|(at, kind, _, _)| *at >= from && *kind != BATCH_COMMIT)
        .map(|(at, kind, _, len)| {
            let s = at as usize + HEADER;
            (kind, bytes[s..s + len as usize].to_vec())
        })
        .collect()
}

fn torn_files(path: &Path) -> Vec<String> {
    std::fs::read_dir(path.join("segments"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".torn-"))
        .collect()
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

/// The landing batch of an authorized fixture, split off: the store at `prep` holds everything
/// before it (the content and the preparation batch, its log cut at the landing batch's start),
/// and `items` are the landing batch's data frames, to seal through the live test writer.
/// `full` is the bytes of the whole fixture log, before any open.
struct Split {
    prep: PathBuf,
    items: Vec<(u8, Vec<u8>)>,
    full: BTreeMap<String, Vec<u8>>,
    fixture: Fixture,
}

fn split(setup: &Setup, opts: &FixtureOptions) -> Split {
    let full_path = setup.path.with_file_name("full");
    copy_dir(&setup.path, &full_path);
    let fixture = authorized_batch_fixture(&full_path, &[setup.spec()], opts);
    let full = segment_bytes(&full_path);
    let start = fixture.landing_start.unwrap();
    let prep = setup.path.with_file_name("prep");
    copy_dir(&full_path, &prep);
    let name = seg_name(start.segment);
    assert_eq!(
        full.len(),
        start.segment as usize + 1,
        "one landing segment"
    );
    let items = items_from(&full[&name], start.offset);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(prep.join("segments").join(&name))
        .unwrap();
    f.set_len(start.offset).unwrap();
    drop(f);
    Split {
        prep,
        items,
        full,
        fixture,
    }
}

/// The reads of a finished removal: stubs, the residual change, the bystander, the blob refused
/// and the landing in canon.
fn assert_removed(s: &Store, setup: &Setup, f: &Fixture) {
    let req = f.requests[0].request;
    let stub = s.get_block(&setup.a.version_hash()).unwrap();
    assert_eq!(stub.body, CONTENT_REMOVED);
    assert_eq!(stub.redaction(), Some(req));
    assert_eq!(stub.version_hash(), setup.a.version_hash());
    assert_eq!(s.get_block(&setup.b.version_hash()).unwrap(), setup.b);
    let c = s.change(&setup.c1).unwrap();
    assert_eq!(c.id(), setup.c1, "the residual change keeps its id");
    assert!(matches!(&c.ops[0], Op::Create { block } if block.redaction() == Some(req)));
    assert!(matches!(&c.ops[1], Op::Create { block } if *block == setup.b));
    assert!(matches!(
        s.get_blob(&setup.blob),
        Err(StoreError::Redacted { request }) if request == req
    ));
    assert!(matches!(
        s.get_raw(&setup.a.version_hash()),
        Err(StoreError::Redacted { request }) if request == req
    ));
    assert_eq!(
        s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
        Some(req)
    );
    assert_eq!(s.canon_head().unwrap(), f.landing, "the same landing");
    assert_eq!(
        s.redaction_epoch(),
        0,
        "a fresh reader family starts at zero"
    );
    s.check_redaction_epoch().unwrap();
}

#[test]
fn rdx4_erasure_changes_only_authorized_payload_spans() {
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
    let spans = spans_of(&f);
    let kinds_seen: BTreeSet<u8> = spans.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds_seen,
        BTreeSet::from([BLOCK, CHANGE, BLOB_CHUNK]),
        "the block, the change that embeds it and the blob's chunks"
    );
    assert_eq!(spans.iter().filter(|s| s.kind == BLOB_CHUNK).count(), 2);
    let before = segment_bytes(&setup.path);

    let s = Store::open(&setup.path).unwrap();
    let after = segment_bytes(&setup.path);
    assert_eq!(
        after,
        zeroed(&before, &spans),
        "selected payloads are zero and every other byte agrees"
    );
    for (name, bytes) in &before {
        assert_eq!(
            headers(bytes),
            headers(&after[name]),
            "{name}: every frame keeps its offset, length, kind and hash"
        );
    }
    for span in &spans {
        let file = &after[&seg_name(span.place.segment)];
        let start = span.place.offset as usize + HEADER;
        assert!(
            file[start..start + span.len as usize]
                .iter()
                .all(|b| *b == 0)
        );
    }
    // the record walker reads exactly the spans as dropped
    let dropped: BTreeSet<(u32, u64)> = frames(&s.reader(), s.log_end())
        .into_iter()
        .filter(|fr| fr.payload.is_none())
        .map(|fr| (fr.loc.segment, fr.loc.offset))
        .collect();
    let want: BTreeSet<(u32, u64)> = spans
        .iter()
        .map(|sp| (sp.place.segment, sp.place.offset))
        .collect();
    assert_eq!(dropped, want);
    assert_removed(&s, &setup, &f);
    drop(s);

    // a second open finds the work complete and changes nothing
    let mut s = Store::open(&setup.path).unwrap();
    let counts = s.finish_redactions().unwrap();
    assert_eq!(counts.frames, spans.len() as u64);
    assert_eq!(counts.checked_payloads, 0, "nothing was left to erase");
    drop(s);
    assert_eq!(segment_bytes(&setup.path), after);
}

#[test]
fn rdx4_fsck_distinguishes_checked_and_removed_payloads() {
    let setup = Setup::new();
    // a store with no redaction reports no redaction counts
    {
        let s = Store::open(&setup.path).unwrap();
        let r = s.verify_report().unwrap();
        assert_eq!(r.redaction, None);
        assert_eq!(r.frames, s.verify().unwrap());
    }
    let f = setup.apply(&FixtureOptions::approved(Proof::relayed_human()));
    let spans = spans_of(&f);
    let s = Store::open(&setup.path).unwrap();
    let all: Vec<LogFrame> = frames(&s.reader(), s.log_end());
    let present_bytes: u64 = all
        .iter()
        .filter(|fr| fr.payload.is_some())
        .map(|fr| u64::from(fr.len))
        .sum();
    let data_frames = all.iter().filter(|fr| fr.kind != BATCH_COMMIT).count();
    let objects: BTreeSet<(u8, Hash)> = spans.iter().map(|sp| (sp.kind, sp.hash)).collect();
    let bytes: u64 = spans.iter().map(|sp| u64::from(sp.len)).sum();

    let hashed_before = rdy_store::segment::frame_bytes_hashed();
    let r = s.verify_report().unwrap();
    let hashed = rdy_store::segment::frame_bytes_hashed() - hashed_before;
    assert_eq!(r.frames, data_frames);
    let c = r.redaction.expect("an active redaction reports its counts");
    assert_eq!(c.objects, objects.len() as u64);
    assert_eq!(c.frames, spans.len() as u64);
    assert_eq!(c.bytes, bytes);
    assert_eq!(c.checked_payloads, (data_frames - spans.len()) as u64);
    assert_eq!(
        hashed, present_bytes,
        "every intact payload is hashed once and no erased payload is rehashed"
    );
    assert_eq!(s.verify().unwrap(), data_frames);
    drop(s);

    // a second request for the same version adds no span: nothing is counted twice
    authorized_batch_fixture(
        &setup.path,
        &[RequestSpec::new("work", vec![version(&setup.a)]).at(70)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let s = Store::open(&setup.path).unwrap();
    let again = s.verify_report().unwrap();
    let c2 = again.redaction.unwrap();
    assert_eq!(
        (c2.objects, c2.frames, c2.bytes),
        (c.objects, c.frames, c.bytes)
    );
    assert_eq!(c2.checked_payloads + c2.frames, again.frames as u64);
}

/// One forged or missing authorization: the fixture's options, and whether to zero the true
/// spans' payloads first (a hole the forged record would have to authorize).
struct Forged {
    name: &'static str,
    opts: FixtureOptions,
    zero: bool,
    damage_marker: bool,
}

fn forged(name: &'static str, opts: FixtureOptions) -> Forged {
    Forged {
        name,
        opts,
        zero: true,
        damage_marker: false,
    }
}

fn on_block_span(f: impl Fn(&mut DropSpan) + 'static) -> FixtureOptions {
    let mut o = FixtureOptions::approved(Proof::local_human());
    o.on_draft = Some(Box::new(move |_, d| {
        let span = d
            .request
            .manifest
            .spans
            .iter_mut()
            .find(|s| s.kind == BLOCK)
            .unwrap();
        f(span);
    }));
    o
}

fn with_proof(f: impl Fn(&mut Proof)) -> FixtureOptions {
    let mut p = Proof::local_human();
    f(&mut p);
    FixtureOptions::approved(p)
}

fn with_apply(f: impl Fn(&mut rdy_model::redaction::RedactionApply) + 'static) -> FixtureOptions {
    let mut o = FixtureOptions::approved(Proof::local_human());
    o.on_applies = Some(Box::new(move |applies| {
        for a in applies {
            f(a);
        }
    }));
    o
}

#[test]
fn rdx4_forged_or_missing_authorization_is_corruption() {
    let other = Hash::of(b"rdx4 another object");
    let mut cases = vec![
        forged("location", on_block_span(|s| s.place.offset += 1)),
        forged("length", on_block_span(|s| s.len += 1)),
        forged("kind", on_block_span(|s| s.kind = BLOB_CHUNK)),
        forged(
            "hash",
            on_block_span(move |s| s.hash = Hash::of(b"rdx4 another object")),
        ),
        forged(
            "residue",
            on_block_span(|s| s.residue = Some(Hash::of(b"rdx4 no such residue"))),
        ),
        forged("no residue", on_block_span(|s| s.residue = None)),
        forged("residue frames", {
            let mut o = FixtureOptions::approved(Proof::local_human());
            o.on_draft = Some(Box::new(|_, d| d.residues.clear()));
            o
        }),
        forged("landing", with_apply(move |a| a.landing = other)),
        forged("request", with_apply(move |a| a.request = other)),
        forged("report", with_apply(move |a| a.report_hash = other)),
        forged(
            "policy assurance",
            with_proof(|p| p.assurance = Assurance::Policy),
        ),
        forged(
            "agent role",
            with_proof(|p| p.approved_by.role = Role::Writer),
        ),
        forged(
            "local with a relay",
            with_proof(|p| p.relayed_by = Proof::relayed_human().relayed_by),
        ),
        forged("relayed without evidence", {
            let mut p = Proof::relayed_human();
            p.relay_evidence = None;
            FixtureOptions::approved(p)
        }),
        forged("relayed without a relayer", {
            let mut p = Proof::relayed_human();
            p.relayed_by = None;
            FixtureOptions::approved(p)
        }),
        forged("unsealed apply", {
            let mut o = FixtureOptions::approved(Proof::local_human());
            o.seal = false;
            o
        }),
        forged("missing apply", FixtureOptions::prepared_only()),
        Forged {
            name: "damaged marker",
            opts: FixtureOptions::approved(Proof::local_human()),
            zero: true,
            damage_marker: true,
        },
    ];
    // a forged authorization over intact payloads is refused too, and erases nothing
    cases.push(Forged {
        name: "policy assurance, intact payloads",
        opts: with_proof(|p| p.assurance = Assurance::Policy),
        zero: false,
        damage_marker: false,
    });
    cases.push(Forged {
        name: "location, intact payloads",
        opts: on_block_span(|s| s.place.offset += 1),
        zero: false,
        damage_marker: false,
    });
    for case in cases {
        let setup = Setup::new();
        let true_spans = setup.draft_spans();
        let f = setup.apply(&case.opts);
        if case.zero {
            zero_in_files(&setup.path, &true_spans);
        }
        if case.damage_marker {
            let start = f.landing_start.unwrap();
            let file = setup.path.join("segments").join(seg_name(start.segment));
            let mut bytes = std::fs::read(&file).unwrap();
            let n = bytes.len();
            bytes[n - 2] ^= 0xff; // inside the landing batch's marker payload
            std::fs::write(&file, bytes).unwrap();
        }
        let before = segment_bytes(&setup.path);
        let e = Store::open(&setup.path)
            .err()
            .unwrap_or_else(|| panic!("{}: the open must refuse", case.name));
        assert!(
            matches!(e, StoreError::RedactionCorrupt { .. }),
            "{}: {e:?}",
            case.name
        );
        assert!(
            e.to_string()
                .starts_with("The redaction record does not authorize the missing payload at "),
            "{}: {e}",
            case.name
        );
        assert_eq!(
            segment_bytes(&setup.path),
            before,
            "{}: the refused open changes no byte",
            case.name
        );
        assert!(torn_files(&setup.path).is_empty(), "{}", case.name);
    }
}

/// A reader's verdict: per frame whether it reads as dropped, the redactions of the objects, the
/// reads of the secret and the bystander, and the index's derived rows.
type Verdict = (Vec<(Location, bool)>, Vec<Option<Hash>>, String, String);

/// What a reader of the store at `path` concludes: per frame whether it reads as dropped, the
/// redaction of each object, the secret's read, and the index's rows.
fn verdict(path: &Path, setup: &Setup) -> Verdict {
    let s = Store::open(path).unwrap();
    let walk = frames(&s.reader(), s.log_end())
        .into_iter()
        .map(|fr| (fr.loc, fr.payload.is_none()))
        .collect();
    let of = vec![
        s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
        s.redaction_of(BLOCK, &setup.b.version_hash()).unwrap(),
        s.redaction_of(CHANGE, &setup.c1).unwrap(),
        s.redaction_of(kinds::BLOB_MANIFEST, &setup.blob).unwrap(),
    ];
    let read = format!(
        "{:?} {:?}",
        s.get_block(&setup.a.version_hash()),
        s.get_block(&setup.b.version_hash())
    );
    let d = s.index_dump_for_tests().unwrap();
    let rows = format!(
        "{:?}",
        (
            &d.redaction_carriers,
            &d.redaction_items,
            &d.redaction_objects,
            &d.redaction_applied,
            &d.redaction_active,
            &d.redaction_spans,
            &d.objects,
            &d.typed,
            &d.producers,
            &d.deleters
        )
    );
    (walk, of, read, rows)
}

#[test]
fn rdx4_reindex_cannot_authorize_or_forget_a_hole() {
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
    let req = f.requests[0].request;
    let want = verdict(&setup.path, &setup);
    assert_eq!(want.1[0], Some(req));
    assert_eq!(want.1[1], None, "the bystander is not removed");

    // a deleted index: the open rebuilds it from the log with the same verdict
    std::fs::remove_file(setup.path.join("index.redb")).unwrap();
    assert_eq!(verdict(&setup.path, &setup), want, "deleted index");

    // an explicit reindex
    {
        let mut s = Store::open(&setup.path).unwrap();
        s.reindex().unwrap();
    }
    assert_eq!(verdict(&setup.path, &setup), want, "reindex");

    // a forged index entry: an active span and mask over the bystander's intact frame
    let b_frame = {
        let s = Store::open(&setup.path).unwrap();
        frames(&s.reader(), s.log_end())
            .into_iter()
            .find(|fr| fr.kind == BLOCK && fr.hash == setup.b.version_hash())
            .unwrap()
    };
    {
        let mut s = Store::open(&setup.path).unwrap();
        s.forge_active_span_for_tests(
            b_frame.loc,
            BLOCK,
            b_frame.hash,
            b_frame.len,
            f.applies[0].0,
            req,
        )
        .unwrap();
        assert_eq!(
            s.redaction_of(BLOCK, &setup.b.version_hash()).unwrap(),
            Some(req),
            "the forged row is in the index"
        );
    }
    assert_eq!(verdict(&setup.path, &setup), want, "forged index entry");

    // an index that forgot the authorization: the log still authorizes the holes
    {
        let mut s = Store::open(&setup.path).unwrap();
        s.clear_redaction_rows_for_tests().unwrap();
        assert_eq!(
            s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
            None
        );
    }
    assert_eq!(verdict(&setup.path, &setup), want, "forgotten index rows");
}

#[test]
fn rdx4_no_prepared_request_can_erase() {
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::prepared_only());
    assert!(f.landing.is_none());
    let before = segment_bytes(&setup.path);
    {
        let mut s = Store::open(&setup.path).unwrap();
        let counts = s.finish_redactions().unwrap();
        assert_eq!(
            (counts.objects, counts.frames, counts.bytes),
            (0, 0, 0),
            "a request with no apply authorizes nothing"
        );
        assert_eq!(
            s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
            None
        );
        assert_eq!(s.get_block(&setup.a.version_hash()).unwrap(), setup.a);
        assert_eq!(s.verify_report().unwrap().redaction, None);
    }
    let s = Store::open(&setup.path).unwrap();
    assert_eq!(s.get_block(&setup.a.version_hash()).unwrap(), setup.a);
    assert!(s.get_blob(&setup.blob).is_ok());
    drop(s);
    assert_eq!(
        segment_bytes(&setup.path),
        before,
        "every byte is untouched after reopen"
    );
}

#[test]
fn rdx4_pre_authorization_failure_leaves_the_old_store() {
    // a failed append before the batch is sealed rolls back as an ordinary write
    let setup = Setup::new();
    let sp = split(&setup, &FixtureOptions::approved(Proof::local_human()));
    let before = segment_bytes(&sp.prep);
    {
        let mut s = Store::open(&sp.prep).unwrap();
        let end = s.log_end();
        s.fail_next_append_after(20);
        assert!(s.seal_redaction_batch_for_tests(sp.items.clone()).is_err());
        assert_eq!(s.log_end(), end, "the unsealed batch is rolled back");
        assert_eq!(
            s.redaction_epoch(),
            0,
            "no epoch advance before authorization"
        );
        assert_eq!(
            s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
            None
        );
        assert_eq!(s.get_block(&setup.a.version_hash()).unwrap(), setup.a);
        s.check_redaction_epoch().unwrap();
    }
    assert_eq!(segment_bytes(&sp.prep), before);
    {
        let s = Store::open(&sp.prep).unwrap();
        assert_eq!(
            s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
            None
        );
        assert_eq!(s.verify_report().unwrap().redaction, None);
        assert!(s.get_blob(&setup.blob).is_ok());
    }
    assert_eq!(
        segment_bytes(&sp.prep),
        before,
        "no acknowledged payload changed"
    );

    // a batch whose landing a policy approved is refused before anything is written
    let setup = Setup::new();
    let mut p = Proof::local_human();
    p.assurance = Assurance::Policy;
    let sp = split(&setup, &FixtureOptions::approved(p));
    let before = segment_bytes(&sp.prep);
    {
        let mut s = Store::open(&sp.prep).unwrap();
        let end = s.log_end();
        assert!(matches!(
            s.seal_redaction_batch_for_tests(sp.items.clone()),
            Err(StoreError::RedactionHuman)
        ));
        assert_eq!(s.log_end(), end);
        // the store keeps working
        s.put_blob(b"unrelated").unwrap();
    }
    let s = Store::open(&sp.prep).unwrap();
    assert_eq!(s.get_block(&setup.a.version_hash()).unwrap(), setup.a);
    drop(s);
    let after = segment_bytes(&sp.prep);
    for (name, bytes) in &before {
        assert!(after[name].starts_with(bytes), "{name} kept every byte");
    }
}

/// The live writer seals the landing batch and finishes the removal before it returns.
#[test]
fn rdx4_live_apply_finishes_before_return() {
    let setup = Setup::new();
    let sp = split(&setup, &FixtureOptions::approved(Proof::relayed_human()));
    let spans = spans_of(&sp.fixture);
    let mut s = Store::open(&sp.prep).unwrap();
    let old = s.reader();
    let counts = s.seal_redaction_batch_for_tests(sp.items.clone()).unwrap();
    assert_eq!(counts.frames, spans.len() as u64);
    assert_eq!(
        counts.bytes,
        spans.iter().map(|x| u64::from(x.len)).sum::<u64>()
    );
    assert_eq!(
        s.redaction_epoch(),
        1,
        "one advance per newly synced apply batch"
    );
    assert!(matches!(
        old.check_redaction_epoch(),
        Err(StoreError::RedactedState)
    ));
    s.check_redaction_epoch().unwrap();
    s.reader().check_redaction_epoch().unwrap();
    let req = sp.fixture.requests[0].request;
    assert_eq!(
        s.get_block(&setup.a.version_hash()).unwrap().redaction(),
        Some(req)
    );
    drop(old);
    drop(s);
    assert_eq!(
        segment_bytes(&sp.prep),
        zeroed(&sp.full, &spans),
        "the live log is the fixture's log with the spans zeroed"
    );
    let s = Store::open(&sp.prep).unwrap();
    assert_removed(&s, &setup, &sp.fixture);
}

#[test]
fn rdx4_post_authorization_io_failure_preserves_the_apply() {
    // the live handle: each stage after the synced apply marker
    let stages: Vec<Option<Failpoint>> = vec![
        Some(Failpoint::AfterApplySync),
        None, // the index update of the apply batch
        Some(Failpoint::AfterZeroWrite),
        Some(Failpoint::AfterSegmentSync),
        Some(Failpoint::BeforeIndexPublish),
    ];
    for stage in stages {
        let setup = Setup::new();
        let sp = split(&setup, &FixtureOptions::approved(Proof::local_human()));
        let spans = spans_of(&sp.fixture);
        let mut s = Store::open(&sp.prep).unwrap();
        match stage {
            Some(p) => arm_failpoint(Some((p, FailAction::Fail))),
            None => s.fail_next_index_apply(),
        }
        let e = s
            .seal_redaction_batch_for_tests(sp.items.clone())
            .unwrap_err();
        arm_failpoint(None);
        assert!(
            matches!(e, StoreError::RedactionPending),
            "{stage:?}: {e:?}"
        );
        assert_eq!(
            e.to_string(),
            "Content removal was authorized but has not finished."
        );
        // the handle is poisoned: no write, no read, no new response
        assert!(matches!(
            s.put_blob(b"after"),
            Err(StoreError::RedactionPending)
        ));
        assert!(matches!(
            s.check_redaction_epoch(),
            Err(StoreError::RedactionPending)
        ));
        assert!(matches!(
            s.get_raw(&setup.b.version_hash()),
            Err(StoreError::RedactionPending)
        ));
        assert!(matches!(
            s.get_block(&setup.a.version_hash()),
            Err(StoreError::RedactionPending)
        ));
        // no rollback truncated the apply: the log is the fixture's, some spans perhaps zeroed
        let now = segment_bytes(&sp.prep);
        assert_eq!(
            zeroed(&now, &spans),
            zeroed(&sp.full, &spans),
            "{stage:?}: the apply batch is kept"
        );
        drop(s);
        // reopen finishes the exact authorized operation
        let s = Store::open(&sp.prep).unwrap();
        assert_removed(&s, &setup, &sp.fixture);
        drop(s);
        assert_eq!(
            segment_bytes(&sp.prep),
            zeroed(&sp.full, &spans),
            "{stage:?}"
        );
    }

    // the open that recovers: a failure there refuses the open and keeps every frame
    for p in Failpoint::ALL {
        let setup = Setup::new();
        let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
        let spans = spans_of(&f);
        let full = segment_bytes(&setup.path);
        arm_failpoint(Some((p, FailAction::Fail)));
        let e = Store::open(&setup.path).err();
        arm_failpoint(None);
        assert!(
            matches!(e, Some(StoreError::RedactionPending)),
            "{p:?}: {e:?}"
        );
        let now = segment_bytes(&setup.path);
        assert_eq!(zeroed(&now, &spans), zeroed(&full, &spans), "{p:?}");
        let s = Store::open(&setup.path).unwrap();
        assert_removed(&s, &setup, &f);
        drop(s);
        assert_eq!(segment_bytes(&setup.path), zeroed(&full, &spans), "{p:?}");
    }
}

#[test]
fn rdx4_a_torn_tail_still_uses_the_existing_quarantine_rule() {
    // an ordinary unacknowledged tail after a finished removal: only the tail is quarantined
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
    let spans = spans_of(&f);
    let end = {
        let mut s = Store::open(&setup.path).unwrap();
        s.put_blob(b"a later ordinary write").unwrap();
        s.log_end()
    };
    let erased = segment_bytes(&setup.path);
    let name = seg_name(end.segment);
    let file = setup.path.join("segments").join(&name);
    // a torn frame larger than one copy buffer: a header that claims more than the file holds
    let mut tail = vec![];
    tail.extend_from_slice(&(300_000u32).to_le_bytes());
    tail.push(BLOB_CHUNK);
    tail.extend_from_slice(&[7u8; 32]);
    tail.extend(std::iter::repeat_n(9u8, 200_000));
    let mut bytes = std::fs::read(&file).unwrap();
    bytes.extend_from_slice(&tail);
    std::fs::write(&file, &bytes).unwrap();

    let s = Store::open(&setup.path).unwrap();
    assert_eq!(s.log_end(), end);
    let q = &s.open_report().quarantined;
    assert_eq!(q.len(), 1, "{q:?}");
    assert_eq!(
        std::fs::read(&q[0]).unwrap(),
        tail,
        "the quarantine holds the tail"
    );
    assert_removed(&s, &setup, &f);
    drop(s);
    assert_eq!(segment_bytes(&setup.path), erased);

    // a removal interrupted inside the 1 MiB chunk's payload, then a torn tail: the partly
    // zeroed span is authorized work, never mistaken for the tail
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
    let spans2 = spans_of(&f);
    let full = segment_bytes(&setup.path);
    let (nth, chunk) = inside_the_big_chunk(&spans2, 3);
    arm_failpoint_nth(Failpoint::AfterZeroWrite, FailAction::Fail, nth);
    assert!(matches!(
        Store::open(&setup.path).err(),
        Some(StoreError::RedactionPending)
    ));
    arm_failpoint(None);
    assert_partly_zeroed(&setup.path, &full, &spans2, &chunk, 3);
    let last = full.keys().last().unwrap().clone();
    let file = setup.path.join("segments").join(&last);
    let mut bytes = std::fs::read(&file).unwrap();
    let cut = bytes.len();
    bytes.extend_from_slice(&tail[..100]);
    std::fs::write(&file, &bytes).unwrap();
    let mut s = Store::open(&setup.path).unwrap();
    assert_eq!(s.log_end().offset, cut as u64);
    assert_eq!(s.open_report().quarantined.len(), 1);
    assert_removed(&s, &setup, &f);
    assert_eq!(
        s.finish_redactions().unwrap().checked_payloads,
        0,
        "the reopen zeroed the rest of the chunk"
    );
    drop(s);
    assert_eq!(segment_bytes(&setup.path), zeroed(&full, &spans2));
    assert!(!spans.is_empty());
}

/// The erase buffer: one zero write covers at most this many bytes.
const BUFFER: u64 = 65_536;

/// The zero-write count at which an erasure of `spans` (in `(segment, offset)` order) is `k`
/// buffers into the first span larger than one buffer, and that span.
fn inside_the_big_chunk(spans: &[DropSpan], k: u32) -> (u32, DropSpan) {
    let mut sorted = spans.to_vec();
    sorted.sort_by_key(|s| (s.place.segment, s.place.offset));
    let mut before = 0u32;
    for s in sorted {
        if u64::from(s.len) > BUFFER * u64::from(k) {
            return (before + k, s);
        }
        before += u64::from(s.len).div_ceil(BUFFER) as u32;
    }
    panic!("no span is longer than {k} buffers");
}

/// After an erasure failed `k` buffers into `chunk`: every span before it is zero, the first `k`
/// buffers of its payload are zero and the rest is the original, and every later span is intact.
fn assert_partly_zeroed(
    path: &Path,
    full: &BTreeMap<String, Vec<u8>>,
    spans: &[DropSpan],
    chunk: &DropSpan,
    k: u32,
) {
    let now = segment_bytes(path);
    let at = |s: &DropSpan| (s.place.segment, s.place.offset);
    let before: Vec<DropSpan> = spans
        .iter()
        .filter(|s| at(s) < at(chunk))
        .cloned()
        .collect();
    assert!(
        !before.is_empty() && spans.iter().any(|s| at(s) > at(chunk)),
        "spans on both sides"
    );
    let name = seg_name(chunk.place.segment);
    let start = chunk.place.offset as usize + HEADER;
    let cut = (BUFFER * u64::from(k)) as usize;
    let payload = &now[&name][start..start + chunk.len as usize];
    assert!(
        payload[..cut].iter().all(|b| *b == 0),
        "the first buffers are zero"
    );
    assert_eq!(
        &payload[cut..],
        &full[&name][start + cut..start + chunk.len as usize],
        "the rest of the chunk is untouched"
    );
    assert!(
        payload[cut..].iter().any(|b| *b != 0),
        "the chunk is partly zero"
    );
    // the earlier spans are erased and the later ones intact: the log is the original with
    // only the earlier spans and the chunk's first buffers zeroed
    let mut want = zeroed(full, &before);
    want.get_mut(&name).unwrap()[start..start + cut].fill(0);
    assert_eq!(
        now, want,
        "only the earlier spans and the chunk's first buffers changed"
    );
}

fn frame_bytes(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(&Hash::of(payload).0);
    out.extend_from_slice(payload);
    out
}

/// Appends raw bytes to segment file `seg` of the closed store at `path` (creating it), and
/// returns the offset they start at.
fn append_raw(path: &Path, seg: u32, bytes: &[u8]) -> u64 {
    use std::io::Write as _;
    let file = path.join("segments").join(seg_name(seg));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .unwrap();
    let at = f.metadata().unwrap().len();
    f.write_all(bytes).unwrap();
    f.sync_all().unwrap();
    at
}

/// Appends `items` as one sealed batch, raw, to segment `seg` of the closed store at `path`;
/// returns the location of each data frame.
fn append_raw_batch(path: &Path, seg: u32, items: &[(u8, Vec<u8>)]) -> Vec<Location> {
    let mut at = std::fs::metadata(path.join("segments").join(seg_name(seg)))
        .map(|m| m.len())
        .unwrap_or(0);
    let start = Location {
        segment: seg,
        offset: at,
    };
    let mut bytes = vec![];
    let mut locs = vec![];
    for (kind, payload) in items {
        locs.push(Location {
            segment: seg,
            offset: at,
        });
        let frame = frame_bytes(*kind, payload);
        at += frame.len() as u64;
        bytes.extend(frame);
    }
    let hashes: Vec<Hash> = items.iter().map(|(_, p)| Hash::of(p)).collect();
    let marker = kinds::BatchCommit {
        start,
        frames: items.len() as u32,
        digest: kinds::batch_digest(&hashes),
    };
    bytes.extend(frame_bytes(
        BATCH_COMMIT,
        &postcard::to_allocvec(&marker).unwrap(),
    ));
    append_raw(path, seg, &bytes);
    locs
}

/// The location and payload length of the `kind` frame with hash `hash` in segment `seg`.
fn frame_of(path: &Path, seg: u32, kind: u8, hash: Hash) -> (Location, u32) {
    let bytes = std::fs::read(path.join("segments").join(seg_name(seg))).unwrap();
    let (offset, _, _, len) = headers(&bytes)
        .into_iter()
        .find(|(_, k, h, _)| *k == kind && *h == hash)
        .unwrap();
    (
        Location {
            segment: seg,
            offset,
        },
        len,
    )
}

/// Overwrites `len` payload bytes of the frame at `loc` with `byte`.
fn fill_payload(path: &Path, loc: Location, len: u32, byte: u8) {
    let file = path.join("segments").join(seg_name(loc.segment));
    let mut bytes = std::fs::read(&file).unwrap();
    let start = loc.offset as usize + HEADER;
    bytes[start..start + len as usize].fill(byte);
    std::fs::write(&file, bytes).unwrap();
}

/// Rolls the closed store at `path` to a new segment with one ordinary sealed write.
fn roll(path: &Path) {
    let mut s = Store::open(path).unwrap();
    let seg = s.log_end().segment;
    s.set_segment_max(1);
    s.put_blob(b"rdx4 a later segment").unwrap();
    assert_eq!(s.log_end().segment, seg + 1, "the write rolled");
}

/// The landing batch's items with the apply's payload grown by `extra` zero bytes.
fn padded_apply(items: &[(u8, Vec<u8>)], extra: usize) -> Vec<(u8, Vec<u8>)> {
    let mut items = items.to_vec();
    let apply = items
        .iter_mut()
        .find(|(k, _)| *k == kinds::REDACTION_APPLY)
        .unwrap();
    apply.1.extend(std::iter::repeat_n(0u8, extra));
    items
}

/// Fix round 1, finding 1 (P1): a batch marker that does not match its batch in a sealed
/// segment, before the landing and apply batch. The header pass cannot see past it, so the open
/// refuses: no replay may index an apply that the bootstrap never validated.
#[test]
fn rdx4_a_broken_marker_in_a_sealed_segment_refuses_the_open() {
    let setup = Setup::new();
    let sp = split(&setup, &FixtureOptions::approved(Proof::local_human()));
    assert_eq!(sp.fixture.landing_start.unwrap().segment, 0);
    let forged = kinds::BatchCommit {
        start: Location {
            segment: 0,
            offset: 0,
        },
        frames: 999,
        digest: Hash::of(b"rdx4 no such batch"),
    };
    let bad = append_raw(
        &sp.prep,
        0,
        &frame_bytes(BATCH_COMMIT, &postcard::to_allocvec(&forged).unwrap()),
    );
    append_raw_batch(&sp.prep, 0, &sp.items);
    append_raw_batch(&sp.prep, 1, &[(BLOB_CHUNK, b"rdx4 segment one".to_vec())]);
    let before = segment_bytes(&sp.prep);
    let e = Store::open(&sp.prep)
        .err()
        .expect("a broken marker in a sealed segment refuses the open");
    let StoreError::Io(io) = &e else {
        panic!("damage, not a redaction verdict: {e:?}");
    };
    assert_eq!(io.kind(), std::io::ErrorKind::InvalidData, "{e}");
    assert!(
        e.to_string().contains("segment 0") && e.to_string().contains(&format!("offset {bad}")),
        "{e}"
    );
    assert_eq!(
        segment_bytes(&sp.prep),
        before,
        "the refused open changes no byte"
    );
}

/// Fix round 1, finding 1 (P3): an apply over 16777216 bytes behind a valid prefix, in a sealed
/// batch of the last segment. The marker rule's scan accepts the frame, so the header pass must
/// not stop at it: the apply is refused as a broken record. So is an apply with trailing bytes
/// (not its own canonical encoding).
#[test]
fn rdx4_an_oversized_or_padded_apply_is_corruption() {
    for extra in [16_777_217usize, 10] {
        let setup = Setup::new();
        let sp = split(&setup, &FixtureOptions::approved(Proof::local_human()));
        let items = padded_apply(&sp.items, extra);
        let locs = append_raw_batch(&sp.prep, 0, &items);
        let apply_at = locs[items
            .iter()
            .position(|(k, _)| *k == kinds::REDACTION_APPLY)
            .unwrap()];
        let before = segment_bytes(&sp.prep);
        let e = Store::open(&sp.prep).err();
        assert!(
            matches!(
                e,
                Some(StoreError::RedactionCorrupt { segment, offset })
                    if segment == apply_at.segment && offset == apply_at.offset
            ),
            "{extra}: {e:?}"
        );
        assert_eq!(segment_bytes(&sp.prep), before, "{extra}: no byte changed");
    }
}

/// Fix round 1, finding 1 (P3, marker): a batch marker longer than 64 bytes behind a valid
/// prefix closes the landing batch. The marker rule's scan accepts it, so the header pass does
/// too: both passes see the same apply, and the removal it authorizes finishes.
#[test]
fn rdx4_both_passes_agree_on_a_long_marker() {
    let setup = Setup::new();
    let sp = split(&setup, &FixtureOptions::approved(Proof::local_human()));
    let spans = spans_of(&sp.fixture);
    let start = Location {
        segment: 0,
        offset: std::fs::metadata(sp.prep.join("segments").join(seg_name(0)))
            .unwrap()
            .len(),
    };
    let mut bytes = vec![];
    for (kind, payload) in &sp.items {
        bytes.extend(frame_bytes(*kind, payload));
    }
    let hashes: Vec<Hash> = sp.items.iter().map(|(_, p)| Hash::of(p)).collect();
    let mut marker = postcard::to_allocvec(&kinds::BatchCommit {
        start,
        frames: sp.items.len() as u32,
        digest: kinds::batch_digest(&hashes),
    })
    .unwrap();
    marker.resize(100, 0);
    bytes.extend(frame_bytes(BATCH_COMMIT, &marker));
    append_raw(&sp.prep, 0, &bytes);
    let before = segment_bytes(&sp.prep);
    let s = Store::open(&sp.prep).unwrap();
    assert_eq!(
        s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
        Some(sp.fixture.requests[0].request)
    );
    drop(s);
    assert_eq!(
        segment_bytes(&sp.prep),
        zeroed(&before, &spans),
        "the apply both passes see is finished: its spans are zero"
    );
}

/// Fix round 1, finding 1: a replay indexes only the applies the bootstrap validated. An apply
/// frame the log's authorization does not hold is `RedactionCorrupt` at it, never a row.
#[test]
fn rdx4_replay_refuses_an_apply_the_bootstrap_did_not_validate() {
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
    let apply = f.applies[0].0;
    let (apply_at, _) = frame_of(&setup.path, 0, kinds::REDACTION_APPLY, apply);
    let mut s = Store::open(&setup.path).unwrap();
    s.forget_redaction_authorization_for_tests();
    let e = s.reindex().err();
    assert!(
        matches!(
            e,
            Some(StoreError::RedactionCorrupt { segment, offset })
                if segment == apply_at.segment && offset == apply_at.offset
        ),
        "{e:?}"
    );
}

/// Fix round 1, finding 1: when the index's redaction rows differ from the log's, the open
/// rebuilds the index and checks again; a difference that remains is corruption at the record
/// the authorization does not hold, not a silent rebuild at every open.
#[test]
fn rdx4_a_redaction_index_mismatch_that_remains_is_corruption() {
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::prepared_only());
    let (request_at, _) = frame_of(&setup.path, 0, kinds::REDACTION, f.requests[0].request);
    let mut s = Store::open(&setup.path).unwrap();
    // a matching index passes as it is
    s.check_redaction_index_for_tests().unwrap();
    s.forget_redaction_authorization_for_tests();
    let e = s.check_redaction_index_for_tests().err();
    assert!(
        matches!(
            e,
            Some(StoreError::RedactionCorrupt { segment, offset })
                if segment == request_at.segment && offset == request_at.offset
        ),
        "{e:?}"
    );
}

/// Fix round 1, finding 2 (P4): the live writer refuses a redaction record over 16777216 bytes
/// before it writes anything, and validates a record exactly as the open does (an apply with
/// trailing bytes is refused too). The store stays as it was, and opens.
#[test]
fn rdx4_the_live_writer_refuses_what_the_open_would_refuse() {
    for (extra, limit) in [(16_777_217usize, true), (10, false)] {
        let setup = Setup::new();
        let sp = split(&setup, &FixtureOptions::approved(Proof::local_human()));
        let before = segment_bytes(&sp.prep);
        {
            let mut s = Store::open(&sp.prep).unwrap();
            let end = s.log_end();
            let e = s
                .seal_redaction_batch_for_tests(padded_apply(&sp.items, extra))
                .err();
            if limit {
                assert!(
                    matches!(
                        e,
                        Some(StoreError::RedactionLimit {
                            limit: "16777216 bytes per record"
                        })
                    ),
                    "{e:?}"
                );
                assert_eq!(
                    e.unwrap().to_string(),
                    "The redaction exceeds 16777216 bytes per record."
                );
            } else {
                assert!(
                    matches!(e, Some(StoreError::RedactionCorrupt { .. })),
                    "{e:?}"
                );
            }
            assert_eq!(s.log_end(), end, "{extra}: nothing was written");
            assert_eq!(s.redaction_epoch(), 0);
            assert_eq!(s.get_block(&setup.a.version_hash()).unwrap(), setup.a);
        }
        assert_eq!(segment_bytes(&sp.prep), before, "{extra}");
        let s = Store::open(&sp.prep).unwrap();
        assert_eq!(
            s.redaction_of(BLOCK, &setup.a.version_hash()).unwrap(),
            None
        );
        assert_eq!(s.get_block(&setup.a.version_hash()).unwrap(), setup.a);
    }
}

/// Fix round 1, finding 3 (ruling task-4): the open verifies the tail it replays; reads and fsck
/// catch the rest. An all-zero payload in a sealed segment that no redaction authorizes: the
/// open succeeds, a read of it is `RedactionCorrupt`, and fsck names it the same way. A payload
/// that is damaged but not all zero keeps the generic corruption error.
#[test]
fn rdx4_an_unknown_hole_in_a_sealed_segment_is_corruption_at_read_and_fsck() {
    let setup = Setup::new();
    roll(&setup.path);
    let (a_at, a_len) = frame_of(&setup.path, 0, BLOCK, setup.a.version_hash());
    fill_payload(&setup.path, a_at, a_len, 0);
    let s = Store::open(&setup.path).unwrap();
    let corrupt_at_a = |e: &StoreError| {
        matches!(e, StoreError::RedactionCorrupt { segment, offset }
            if *segment == a_at.segment && *offset == a_at.offset)
    };
    let e = s.get_block(&setup.a.version_hash()).unwrap_err();
    assert!(corrupt_at_a(&e), "{e:?}");
    assert_eq!(
        e.to_string(),
        format!(
            "The redaction record does not authorize the missing payload at 0:{}.",
            a_at.offset
        )
    );
    let e = s.get_raw(&setup.a.version_hash()).unwrap_err();
    assert!(corrupt_at_a(&e), "{e:?}");
    let e = s.verify_report().unwrap_err();
    assert!(corrupt_at_a(&e), "fsck: {e:?}");
    assert_eq!(s.get_block(&setup.b.version_hash()).unwrap(), setup.b);
    drop(s);

    // a damaged payload that is not all zero keeps today's error
    let setup = Setup::new();
    roll(&setup.path);
    let (b_at, b_len) = frame_of(&setup.path, 0, BLOCK, setup.b.version_hash());
    fill_payload(&setup.path, b_at, b_len, 7);
    let s = Store::open(&setup.path).unwrap();
    let e = s.get_block(&setup.b.version_hash()).unwrap_err();
    assert!(
        matches!(&e, StoreError::Io(io) if io.kind() == std::io::ErrorKind::InvalidData)
            && e.to_string().contains("corrupt or missing frame"),
        "{e:?}"
    );
}

/// Fix round 1, finding 3: the catch-up path. An unknown all-zero hole that the catch-up replays
/// refuses the open with `RedactionCorrupt` at it.
#[test]
fn rdx4_an_unknown_hole_the_catch_up_replays_is_corruption() {
    let setup = Setup::new();
    roll(&setup.path);
    let (a_at, a_len) = frame_of(&setup.path, 0, BLOCK, setup.a.version_hash());
    fill_payload(&setup.path, a_at, a_len, 0);
    std::fs::remove_file(setup.path.join("index.redb")).unwrap();
    let e = Store::open(&setup.path).err();
    assert!(
        matches!(
            e,
            Some(StoreError::RedactionCorrupt { segment, offset })
                if segment == a_at.segment && offset == a_at.offset
        ),
        "{e:?}"
    );
}

/// Fix round 1, finding 3: an authorized span in a sealed segment reads as the stub, and fsck
/// counts it as removed.
#[test]
fn rdx4_an_authorized_span_in_a_sealed_segment_reads_as_the_stub() {
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
    drop(Store::open(&setup.path).unwrap());
    roll(&setup.path);
    let s = Store::open(&setup.path).unwrap();
    assert!(s.log_end().segment >= 1);
    assert_removed(&s, &setup, &f);
    let r = s.verify_report().unwrap();
    assert_eq!(r.redaction.unwrap().frames, spans_of(&f).len() as u64);
}

/// Fixture options that alter the request's draft (its targets, manifest or residues) before its
/// preparation batch is encoded; the apply binds the altered request and its report hash.
fn with_draft(f: impl Fn(&mut redaction_fixture::Draft) + 'static) -> FixtureOptions {
    let mut o = FixtureOptions::approved(Proof::local_human());
    o.on_draft = Some(Box::new(move |_, d| f(d)));
    o
}

/// The change residue of a draft (the one change span's).
fn change_residue(d: &mut redaction_fixture::Draft) -> &mut rdy_model::redaction::ChangeResidue {
    d.residues
        .iter_mut()
        .find_map(|r| match r {
            rdy_model::redaction::RedactionResidue::Change(c) => Some(c),
            _ => None,
        })
        .unwrap()
}

/// One inconsistent authorization over intact (or partly erased) payloads: the fixture options
/// and the spans to zero before the open (an erasure a kill interrupted).
struct Inconsistent {
    name: &'static str,
    opts: FixtureOptions,
    zero_kinds: &'static [u8],
}

/// The inconsistent requests of final review findings 1 and 2: each is a sealed, human-approved
/// apply whose request, report hash and residue frames are hash-valid, but whose request does
/// not select what its manifest removes, whose manifest omits a physical copy of the selected
/// content, or whose change residue does not replay the original change.
fn inconsistent_cases(setup: &Setup) -> Vec<Inconsistent> {
    let (a, b, blob) = (setup.a.clone(), setup.b.clone(), setup.blob);
    let (a2, b2) = (a.clone(), b.clone());
    vec![
        Inconsistent {
            name: "the selector names another block",
            opts: with_draft(move |d| {
                d.request.targets = vec![version(&b), RedactTarget::Blob(blob)];
            }),
            zero_kinds: &[],
        },
        Inconsistent {
            name: "the manifest relabels the version as another block's",
            opts: with_draft(move |d| {
                let v = a2.version_hash();
                d.request.targets = vec![
                    RedactTarget::Version {
                        id: b2.id.clone(),
                        version: v,
                    },
                    RedactTarget::Blob(blob),
                ];
                d.request.manifest.versions = vec![(b2.id.clone(), v)];
            }),
            zero_kinds: &[],
        },
        Inconsistent {
            name: "the manifest omits the change that embeds the version",
            opts: with_draft(|d| {
                d.request.manifest.spans.retain(|s| s.kind != CHANGE);
                d.residues
                    .retain(|r| matches!(r, rdy_model::redaction::RedactionResidue::Block(_)));
            }),
            zero_kinds: &[],
        },
        Inconsistent {
            name: "the manifest omits the change, and the block span is already zero",
            opts: with_draft(|d| {
                d.request.manifest.spans.retain(|s| s.kind != CHANGE);
                d.residues
                    .retain(|r| matches!(r, rdy_model::redaction::RedactionResidue::Block(_)));
            }),
            zero_kinds: &[BLOCK],
        },
        Inconsistent {
            name: "the manifest omits a chunk of the blob",
            opts: with_draft(|d| {
                let i = d
                    .request
                    .manifest
                    .spans
                    .iter()
                    .rposition(|s| s.kind == BLOB_CHUNK)
                    .unwrap();
                d.request.manifest.spans.remove(i);
            }),
            zero_kinds: &[],
        },
        Inconsistent {
            name: "the change residue drops the sibling's op",
            opts: with_draft(|d| {
                change_residue(d).ops.truncate(1);
                let residues = d.residues.clone();
                redaction_fixture::relink(&mut d.request, &residues);
            }),
            zero_kinds: &[],
        },
        Inconsistent {
            name: "the change residue drops the sibling's op, and the block span is already zero",
            opts: with_draft(|d| {
                change_residue(d).ops.truncate(1);
                let residues = d.residues.clone();
                redaction_fixture::relink(&mut d.request, &residues);
            }),
            zero_kinds: &[BLOCK],
        },
        Inconsistent {
            name: "the change residue names other dependencies",
            opts: with_draft(|d| {
                change_residue(d).deps = vec![Hash::of(b"rdx4 another dependency")];
                let residues = d.residues.clone();
                redaction_fixture::relink(&mut d.request, &residues);
            }),
            zero_kinds: &[],
        },
        Inconsistent {
            name: "the change residue renames the sibling's version",
            opts: with_draft(|d| {
                if let rdy_model::redaction::ResidualOp::Create { version, .. } =
                    &mut change_residue(d).ops[1]
                {
                    *version = Hash::of(b"rdx4 a version never stored");
                }
                let residues = d.residues.clone();
                redaction_fixture::relink(&mut d.request, &residues);
            }),
            zero_kinds: &[],
        },
    ]
}

/// Final review fix round 1, findings 1 and 2: recovery shares the checked apply path's
/// semantic validation. A sealed, human-approved apply whose request selects other content than
/// its manifest removes, whose manifest omits a physical copy of the selected content (the
/// embedding change, a chunk), or whose change residue does not replay the original change
/// exactly, is refused by the open with `RedactionCorrupt` before any zero write, whatever the
/// index holds. No byte changes, and the original change keeps both its operations.
#[test]
fn rdx4_recovery_refuses_an_inconsistent_request_before_any_zero_write() {
    let probe = Setup::new();
    let mut accepted = vec![];
    for case in inconsistent_cases(&probe) {
        let setup = Setup::new();
        let true_spans = setup.draft_spans();
        let f = setup.apply(&case.opts);
        assert!(f.landing.is_some(), "{}", case.name);
        let zero: Vec<DropSpan> = true_spans
            .iter()
            .filter(|s| case.zero_kinds.contains(&s.kind))
            .cloned()
            .collect();
        zero_in_files(&setup.path, &zero);
        // with or without the derived index
        for drop_index in [false, true] {
            if drop_index {
                let _ = std::fs::remove_file(setup.path.join("index.redb"));
            }
            let before = segment_bytes(&setup.path);
            let Some(e) = Store::open(&setup.path).err() else {
                accepted.push(case.name);
                break;
            };
            assert!(
                matches!(e, StoreError::RedactionCorrupt { .. }),
                "{} (index dropped: {drop_index}): {e:?}",
                case.name
            );
            assert_eq!(
                segment_bytes(&setup.path),
                before,
                "{}: the refused open changes no byte",
                case.name
            );
            assert!(torn_files(&setup.path).is_empty(), "{}", case.name);
        }
        if accepted.last() == Some(&case.name) {
            continue;
        }
        // the original change's bytes, both operations, are still on disk
        let bytes = segment_bytes(&setup.path);
        let sibling = b"bravo stays";
        assert!(
            bytes
                .values()
                .any(|b| b.windows(sibling.len()).any(|w| w == sibling)),
            "{}",
            case.name
        );
    }
    assert!(accepted.is_empty(), "the open accepted: {accepted:#?}");
}

/// Final review fix round 1, findings 1 and 2: the live apply writer validates its batch with
/// the same semantic checks as the open, before anything is written.
#[test]
fn rdx4_the_live_writer_refuses_an_inconsistent_request() {
    let probe = Setup::new();
    let mut accepted = vec![];
    for case in inconsistent_cases(&probe)
        .into_iter()
        .filter(|c| c.zero_kinds.is_empty())
    {
        let setup = Setup::new();
        let sp = split(&setup, &case.opts);
        let before = segment_bytes(&sp.prep);
        {
            let mut s = Store::open(&sp.prep).unwrap();
            let end = s.log_end();
            let e = s.seal_redaction_batch_for_tests(sp.items.clone()).err();
            if e.is_none() {
                accepted.push(case.name);
                continue;
            }
            assert!(
                matches!(e, Some(StoreError::RedactionCorrupt { .. })),
                "{}: {e:?}",
                case.name
            );
            assert_eq!(s.log_end(), end, "{}: nothing was written", case.name);
            assert_eq!(s.redaction_epoch(), 0, "{}", case.name);
        }
        assert_eq!(segment_bytes(&sp.prep), before, "{}", case.name);
    }
    assert!(
        accepted.is_empty(),
        "the live writer accepted: {accepted:#?}"
    );
}
