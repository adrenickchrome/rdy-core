//! mh4: the log's byte report and compact at the store (D-4YRQ4: "Result: `{log_bytes,
//! removed_bytes, punched_bytes, on_disk_bytes}`."). The counts come from one walk of the frame
//! headers, every physical frame counts, and a punch failure is a note, never an error.
#[path = "support/carried.rs"]
mod carried_fixture;
#[path = "support/redaction.rs"]
mod redaction_fixture;

use carried_fixture::{
    append_raw, block_residue, change_residue, find, framed_at, load, record, span,
};
use rdy_model::redaction::{DropSpan, RedactTarget};
use rdy_model::*;
use rdy_store::compact::{CompactError, KindBytes, on_allocation};
use rdy_store::inbox::{InboxCursor, InboxSub};
use rdy_store::pio::{self, Punch};
use rdy_store::segment::{Location, SegmentLog};
use rdy_store::{Store, StoreError, codec, kinds};
use redaction_fixture::{Fixture, FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const HEADER: u64 = 37;
const SEGMENT_MAX: u64 = 64 * 1024 * 1024;
const NOT_SUPPORTED_2: &str = "hole punching is not supported for 2 removed spans; they keep their disk space, and their content stays removed";
const NOT_SUPPORTED_4: &str = "hole punching is not supported for 4 removed spans; they keep their disk space, and their content stays removed";
const FAILED_1: &str = "hole punching failed for 1 removed spans; they keep their disk space, and their content stays removed";
const CENSUS: &str =
    "disk allocation could not be read after punching; on_disk_bytes is the count before this call";

/// The closed table of the report's names, by numeric kind.
const NAMES: [(u8, &str); 27] = [
    (1, "block"),
    (2, "change"),
    (3, "proposal"),
    (4, "landing"),
    (5, "snapshot"),
    (6, "blob_chunk"),
    (7, "blob_manifest"),
    (8, "ref_update"),
    (9, "tag"),
    (10, "answer"),
    (11, "signature"),
    (13, "view_op"),
    (14, "batch_commit"),
    (15, "intent"),
    (16, "alt_set"),
    (17, "review"),
    (18, "tension"),
    (19, "transition"),
    (20, "redaction"),
    (21, "redaction_residue"),
    (22, "redaction_apply"),
    (23, "redaction_carried"),
    (24, "plan"),
    (25, "assessment"),
    (26, "dispute"),
    (27, "dispute_resolution"),
    (28, "check_request"),
];

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

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
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

/// A closed store with a sealed removal of one block version, its change and a two-chunk blob:
/// four spans. The next open erases them.
struct Setup {
    _dir: tempfile::TempDir,
    path: PathBuf,
    fixture: Fixture,
}

impl Setup {
    fn new() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let mut s = Store::create(&path).unwrap();
        let secret = block("b-00001", "canary alpha secret");
        let bystander = block("b-00002", "bravo stays");
        s.commit_to_view(
            "work",
            change(vec![
                Op::Create {
                    block: secret.clone(),
                },
                Op::Create { block: bystander },
            ]),
        )
        .unwrap();
        let mut bytes = b"blob canary ".repeat(100_000);
        bytes.truncate(1024 * 1024 + 4096);
        let blob = s.put_blob(&bytes).unwrap();
        drop(s);
        let spec = RequestSpec::new(
            "work",
            vec![
                RedactTarget::Version {
                    id: secret.id.clone(),
                    version: secret.version_hash(),
                },
                RedactTarget::Blob(blob),
            ],
        );
        let fixture = authorized_batch_fixture(
            &path,
            &[spec],
            &FixtureOptions::approved(Proof::local_human()),
        );
        Setup {
            _dir: dir,
            path,
            fixture,
        }
    }

    fn spans(&self) -> Vec<DropSpan> {
        let mut spans = self.fixture.requests[0]
            .draft
            .request
            .manifest
            .spans
            .clone();
        spans.sort_by_key(|s| (s.place.segment, s.place.offset));
        spans
    }

    fn ranges(&self) -> Vec<(u64, u64)> {
        self.spans()
            .iter()
            .map(|s| (s.place.offset + HEADER, u64::from(s.len)))
            .collect()
    }
}

/// The totals by logical kind and the frame count, by this test's own walk of the segment files:
/// kind to (frames, bytes with headers).
fn walk(store: &Path) -> (u64, BTreeMap<u8, (u64, u64)>) {
    let mut total = 0;
    let mut rows: BTreeMap<u8, (u64, u64)> = BTreeMap::new();
    let mut seg = 0;
    loop {
        let path = store.join("segments").join(format!("seg-{seg:06}.log"));
        let Ok(bytes) = std::fs::read(&path) else {
            return (total, rows);
        };
        let mut at = 0usize;
        while at < bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as u64;
            let kind = bytes[at + 4] & 0x7f;
            let row = rows.entry(kind).or_default();
            row.0 += 1;
            row.1 += HEADER + len;
            total += HEADER + len;
            at += (HEADER + len) as usize;
        }
        seg += 1;
    }
}

/// Appends one frame of `kind`, as this rdy stores it, twice: two physical copies of one hash.
fn doubled(kind: u8) -> [(u8, Vec<u8>); 2] {
    let body = vec![kind, 0xAB, 0xCD];
    let version = if matches!(kind, kinds::REF_UPDATE | kinds::BATCH_COMMIT) {
        0
    } else {
        codec::latest(kind)
    };
    let frame = codec::frame(kind, version, body).unwrap();
    [frame.clone(), frame]
}

/// Removes the hooks when a test ends, also when it panics.
struct Hooks;
impl Drop for Hooks {
    fn drop(&mut self) {
        pio::on_punch(None);
        on_allocation(None);
    }
}

/// One allocation census: its total on the first segment, or an error.
type Census = Result<u64, ()>;

type Ranges = Rc<RefCell<Vec<(u64, u64)>>>;

/// Answers each census in turn (segment 0 starts one, later segments report 0), and each punch
/// by its 0-based number. It returns the punched ranges.
fn inject(
    censuses: Vec<Census>,
    mut punch: impl FnMut(usize) -> io::Result<Punch> + 'static,
) -> (Ranges, Hooks) {
    let ranges: Ranges = Rc::default();
    let seen = Rc::clone(&ranges);
    let mut n = 0;
    pio::on_punch(Some(Box::new(move |offset, len| {
        seen.borrow_mut().push((offset, len));
        n += 1;
        Some(punch(n - 1))
    })));
    let mut left = censuses.into_iter();
    on_allocation(Some(Box::new(move |seg| {
        if seg != 0 {
            return Some(Ok(0));
        }
        Some(match left.next().expect("one census too many") {
            Ok(v) => Ok(v),
            Err(()) => Err(io::Error::other("injected")),
        })
    })));
    (ranges, Hooks)
}

#[test]
fn mh4_byte_counts_include_every_kind_and_physical_copy() {
    let setup = Setup::new();
    // two more moves of one ref: every REF_UPDATE of the history counts
    let mut s = Store::open(&setup.path).unwrap();
    for at in [1, 2] {
        s.put_inbox(&InboxSub {
            actor: "agent:sub".into(),
            run: Some("s1".into()),
            cursor: InboxCursor {
                at: Location {
                    segment: 0,
                    offset: at,
                },
                skip: 0,
            },
            areas: vec![],
            role: "reader".into(),
        })
        .unwrap();
    }
    // every kind, each twice, written beside the open store: its walk reads the files
    let frames: Vec<(u8, Vec<u8>)> = NAMES
        .iter()
        .filter(|(k, _)| *k != kinds::BATCH_COMMIT)
        .flat_map(|(k, _)| doubled(*k))
        .collect();
    append_raw(&setup.path, &frames, true);

    let report = s.log_bytes().unwrap();
    let (total, by_kind) = walk(&setup.path);
    assert_eq!(report.log_bytes, total);
    assert!(by_kind[&kinds::REF_UPDATE].0 >= 4, "the ref history counts");
    assert!(by_kind[&kinds::BATCH_COMMIT].0 >= 2, "markers count");

    // the spans of the removal: payload bytes only, no header
    let spans = setup.spans();
    let mut removed: BTreeMap<u8, u64> = BTreeMap::new();
    for span in &spans {
        *removed.entry(span.kind).or_default() += u64::from(span.len);
    }
    assert_eq!(
        report.removed_bytes,
        spans.iter().map(|s| u64::from(s.len)).sum::<u64>()
    );

    // every name of the closed table, in numeric order, with exact totals
    let want: Vec<KindBytes> = NAMES
        .iter()
        .map(|&(kind, name)| KindBytes {
            kind,
            name,
            bytes: by_kind[&kind].1,
            removed_bytes: removed.get(&kind).copied().unwrap_or(0),
        })
        .collect();
    assert_eq!(report.by_kind, want);
    assert_eq!(
        report.by_kind.iter().map(|r| r.bytes).sum::<u64>(),
        report.log_bytes
    );
    let first = serde_json::to_string(&report.by_kind[0]).unwrap();
    assert_eq!(
        first,
        format!(
            r#"{{"kind":1,"name":"block","bytes":{},"removed_bytes":{}}}"#,
            by_kind[&1].1, removed[&1]
        )
    );
    let whole = serde_json::to_value(&report).unwrap();
    assert_eq!(
        whole.as_object().unwrap().keys().len(),
        3,
        "a byte report has log_bytes, removed_bytes and by_kind"
    );
    assert!(whole.get("notes").is_none());
}

#[test]
fn mh4_byte_counts_keep_a_held_span_and_list_only_present_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let mut s = Store::create(&src).unwrap();
    let held = block("b-00001", "held text");
    let id = s
        .commit_to_view(
            "work",
            change(vec![Op::Create {
                block: held.clone(),
            }]),
        )
        .unwrap();
    let stored = s.change(&id).unwrap();
    let writer = stored.provenance.clone();
    let human = Provenance {
        actor: "human:alice".into(),
        run: None,
        role: Role::Approver,
        at_ms: 11,
    };
    let p = s.propose("work", "held", writer).unwrap();
    s.land(&p, human).unwrap();
    assert_eq!(s.log_bytes().unwrap().removed_bytes, 0);
    drop(s);
    let framed = framed_at(&src);
    let residues = vec![block_residue(&held), change_residue(&stored)];
    let build = |is_held: bool| {
        let mut spans = vec![
            span(
                &find(&framed, kinds::BLOCK, held.version_hash()),
                Some(&residues[0]),
                is_held,
            ),
            span(
                &find(&framed, kinds::CHANGE, id),
                Some(&residues[1]),
                is_held,
            ),
        ];
        spans.sort_by_key(|s| s.place);
        (
            spans.iter().map(|s| u64::from(s.len)).sum::<u64>(),
            record(
                vec![RedactTarget::Version {
                    id: held.id.clone(),
                    version: held.version_hash(),
                }],
                spans,
            ),
        )
    };

    // a held span keeps its payload: retained, not removed
    let (_, rec) = build(true);
    let to = dir.path().join("held");
    load(&src, &to, &residues, std::slice::from_ref(&rec)).unwrap();
    let s = Store::open(&to).unwrap();
    let report = s.log_bytes().unwrap();
    assert_eq!(report.removed_bytes, 0);
    assert_eq!(report.log_bytes, walk(&to).0);
    assert!(report.by_kind.iter().all(|r| r.removed_bytes == 0));
    assert!(
        !report.by_kind.iter().any(|r| r.kind == kinds::TAG),
        "a kind with no frame has no row"
    );
    drop(s);

    // the same span not held is removed
    let (bytes, rec) = build(false);
    let to = dir.path().join("dropped");
    load(&src, &to, &residues, std::slice::from_ref(&rec)).unwrap();
    let s = Store::open(&to).unwrap();
    assert_eq!(s.log_bytes().unwrap().removed_bytes, bytes);
}

#[test]
fn mh4_byte_counts_refuse_damage_through_store_errors() {
    fn store_error(e: CompactError) -> StoreError {
        match e {
            CompactError::Store(e) => e,
            other => panic!("not a store error: {other}"),
        }
    }
    let raw_case = |frames: Vec<(u8, Vec<u8>)>| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let s = Store::create(&path).unwrap();
        append_raw(&path, &frames, true);
        (dir, store_error(s.log_bytes().unwrap_err()))
    };

    // code 12 is unassigned: refused, never an `unknown` row
    let (_d, e) = raw_case(vec![(12, vec![1, 2, 3])]);
    assert_eq!(
        e.to_string(),
        StoreError::Format {
            message: "the store holds an object this rdy cannot read: kind 12, encoding 0".into(),
            fix: "open the project with a newer rdy".into(),
        }
        .to_string()
    );
    // an encoding this rdy does not read, and one no frame may have
    let (_d, e) = raw_case(vec![(kinds::BLOCK | 0x80, vec![0xFF, 0xFF, 1])]);
    assert!(matches!(e, StoreError::Format { .. }), "{e}");
    let (_d, e) = raw_case(vec![(kinds::REF_UPDATE | 0x80, vec![1, 0, 1])]);
    assert!(matches!(e, StoreError::Decode(_)), "{e}");

    // a removed span that holds a byte again
    let setup = Setup::new();
    let s = Store::open(&setup.path).unwrap();
    let spans = setup.spans();
    let path = setup.path.join("segments/seg-000000.log");
    let mut bytes = std::fs::read(&path).unwrap();
    let at = (spans[1].place.offset + HEADER) as usize;
    bytes[at] = 1;
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        store_error(s.log_bytes().unwrap_err()),
        StoreError::RedactionCorrupt { offset, .. } if offset == spans[1].place.offset
    ));
    bytes[at] = 0;
    // a removed span whose header names another hash
    let at = (spans[2].place.offset + 5) as usize;
    bytes[at] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        store_error(s.log_bytes().unwrap_err()),
        StoreError::RedactionCorrupt { offset, .. } if offset == spans[2].place.offset
    ));
    bytes[at] ^= 0xFF;
    // a torn frame at the end of the log
    bytes.extend_from_slice(&[0xFF; 10]);
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        store_error(s.log_bytes().unwrap_err()),
        StoreError::Io(_)
    ));
    drop(s);

    // an open batch
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.set_held(true).unwrap();
    s.put(kinds::BLOCK, &block("b-00009", "open")).unwrap();
    let want = "a held batch is open: seal it first";
    assert_eq!(s.log_bytes().unwrap_err().to_string(), want);
    assert_eq!(s.compact(true).unwrap_err().to_string(), want);
}

#[test]
fn mh4_report_counts_freed_bytes_and_best_effort_notes() {
    fn ok(_: usize) -> io::Result<Punch> {
        Ok(Punch::Supported)
    }
    let setup = Setup::new();
    let s = Store::open(&setup.path).unwrap();
    let want = s.log_bytes().unwrap();

    // freed bytes are before minus after; every span is punched in ascending place order
    let (ranges, hooks) = inject(vec![Ok(9000), Ok(4096)], ok);
    let r = s.compact(false).unwrap();
    assert_eq!(*ranges.borrow(), setup.ranges());
    drop(hooks);
    assert_eq!(
        (r.log_bytes, r.removed_bytes, &r.by_kind),
        (want.log_bytes, want.removed_bytes, &want.by_kind)
    );
    assert_eq!((r.punched_bytes, r.on_disk_bytes), (4904, 4096));
    assert!(r.notes.is_empty());

    // a repeat, or a span with no aligned interior: accepted, nothing freed, no note
    let (ranges, hooks) = inject(vec![Ok(4096), Ok(4096)], ok);
    let r = s.compact(false).unwrap();
    assert_eq!(ranges.borrow().len(), 4);
    drop(hooks);
    assert_eq!((r.punched_bytes, r.on_disk_bytes), (0, 4096));
    assert!(r.notes.is_empty());

    // an allocation that grew frees nothing
    let hooks = inject(vec![Ok(10), Ok(20)], ok).1;
    let r = s.compact(false).unwrap();
    drop(hooks);
    assert_eq!((r.punched_bytes, r.on_disk_bytes), (0, 20));

    // a file system that cannot punch
    let hooks = inject(vec![Ok(10), Ok(10)], |_| Ok(Punch::Unsupported)).1;
    let r = s.compact(false).unwrap();
    drop(hooks);
    assert_eq!(r.notes, [NOT_SUPPORTED_4]);

    // both outcomes, then a failed final census, in their fixed order; each span is still tried
    let (ranges, hooks) = inject(vec![Ok(70), Err(())], |n| match n {
        0 | 2 => Ok(Punch::Unsupported),
        1 => Err(io::Error::other("injected")),
        _ => Ok(Punch::Supported),
    });
    let r = s.compact(false).unwrap();
    assert_eq!(ranges.borrow().len(), 4);
    drop(hooks);
    assert_eq!(r.notes, [NOT_SUPPORTED_2, FAILED_1, CENSUS]);
    assert_eq!((r.punched_bytes, r.on_disk_bytes), (0, 70));

    // a failed first census is an ordinary error, before the first punch
    let (ranges, hooks) = inject(vec![Err(())], ok);
    assert!(matches!(
        s.compact(false).unwrap_err(),
        CompactError::Store(StoreError::Io(_))
    ));
    assert!(ranges.borrow().is_empty());
    drop(hooks);

    // the sum of the segments' real allocation, when nothing is injected
    let real: u64 = {
        let f = std::fs::File::open(setup.path.join("segments/seg-000000.log")).unwrap();
        pio::allocated_bytes(&f).unwrap()
    };
    let r = s.compact(true).unwrap();
    assert_eq!((r.punched_bytes, r.on_disk_bytes), (0, real));
    assert_eq!(s.log_bytes().unwrap(), want);
    // the log still opens at its own marker: nothing was appended
    let (log, torn) =
        SegmentLog::open_marked(&setup.path.join("segments"), SEGMENT_MAX, None).unwrap();
    assert!(torn.is_empty());
    assert_eq!(log.end(), s.log_end());
}
