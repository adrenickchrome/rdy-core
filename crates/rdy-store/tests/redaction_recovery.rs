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
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
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

    // a removal interrupted after one zero write, then a torn tail: the partly zeroed span is
    // authorized work, never mistaken for the tail
    let setup = Setup::new();
    let f = setup.apply(&FixtureOptions::approved(Proof::local_human()));
    let spans2 = spans_of(&f);
    let full = segment_bytes(&setup.path);
    arm_failpoint(Some((Failpoint::AfterZeroWrite, FailAction::Fail)));
    assert!(matches!(
        Store::open(&setup.path).err(),
        Some(StoreError::RedactionPending)
    ));
    arm_failpoint(None);
    let last = full.keys().last().unwrap().clone();
    let file = setup.path.join("segments").join(&last);
    let mut bytes = std::fs::read(&file).unwrap();
    let cut = bytes.len();
    bytes.extend_from_slice(&tail[..100]);
    std::fs::write(&file, &bytes).unwrap();
    let s = Store::open(&setup.path).unwrap();
    assert_eq!(s.log_end().offset, cut as u64);
    assert_eq!(s.open_report().quarantined.len(), 1);
    assert_removed(&s, &setup, &f);
    drop(s);
    assert_eq!(segment_bytes(&setup.path), zeroed(&full, &spans2));
    assert!(!spans.is_empty());
}
