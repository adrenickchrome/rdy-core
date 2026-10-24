//! hc7: a chain load is one transaction (D-2FMRR: "one ingest"; D-QF2HQ: "one carried batch";
//! D-69AY3, D-FR0C4, D-F0904, D-CC90E). After the pre-pass, the load reads each file that
//! contributes retained batches through its end, into one `.rdy/store.loading`: the retained
//! source batches at their original places, then the one carried batch of a stopped load, then
//! at most one upgrade batch. A payload that a removal at or before the stop removed is never
//! written, even for a moment. Each file's trailer must be the one the pre-pass read, and no
//! store takes its place before every file passed; a failure restores the target.
use rdy_engine::dump_stop::DumpStop;
use rdy_engine::load::{LoadSummary, PlaceFault, fail_next_placement_for_tests, load, load_chain};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::redaction::{RedactionApply, RedactionCarried};
use rdy_model::{Hash, RepoId, Role};
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader, TRAILER_LEN};
use rdy_store::dump_chain::batch_end;
use rdy_store::holder::StoreMode;
use rdy_store::kinds;
use rdy_store::redaction_carried::{HOLDER_FIX, PROJECT_FIX};
use rdy_store::segment::{Location, on_segment_write};
use rdy_store::take_batch::UploadTarget;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SECRET: &str = "The launch code is ZEBRA-7731.";
const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};
/// The length of a container record's frame header.
const HEAD: usize = 37;
const DAMAGED_FIX: &str = "dump the project again; rdy never repairs a dump";

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver).unwrap()
}

/// Writes one doc with a prose block to view `work`; returns the block's id.
fn add(r: &mut Rdy, n: usize, body: &str) -> String {
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": format!("D{n}"), "doc_type": "page"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    w.created["$p"].clone()
}

/// Lands view `work`; returns the landing id.
fn land(r: &mut Rdy) -> String {
    let p = r
        .propose(&writer(), "work", "Work", Some("test summary"))
        .unwrap();
    r.land(&approver(), &p.id).unwrap()
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&approver(), name, None, None).unwrap();
}

/// Removes `target` through the human flow.
fn remove(r: &mut Rdy, target: &str) {
    r.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p = r
        .propose(&writer(), "redact", "Remove the code", Some("test summary"))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
}

fn stop(text: &str) -> DumpStop {
    DumpStop::parse(text).unwrap()
}

fn pos(l: Location) -> DumpStop {
    stop(&format!("pos:{}.{}", l.segment, l.offset))
}

fn chain_load(files: &[PathBuf], to: &Path, at: Option<DumpStop>) -> LoadSummary {
    match load_chain(files, to, at.as_ref()) {
        Ok(s) => s,
        Err(e) => panic!("the load failed: {e:?}"),
    }
}

fn chain_err(files: &[PathBuf], to: &Path, at: Option<DumpStop>) -> RdyError {
    match load_chain(files, to, at.as_ref()) {
        Ok(s) => panic!("the load passed: {s:?}"),
        Err(e) => e,
    }
}

/// The segment files of the store under `project`, by name.
fn segs(project: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(project.join(".rdy/store/segments")).unwrap() {
        let e = e.unwrap();
        out.insert(
            e.file_name().to_string_lossy().into_owned(),
            std::fs::read(e.path()).unwrap(),
        );
    }
    out
}

fn contains(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Whether any segment of the store under `project` holds `needle`.
fn holds(project: &Path, needle: &str) -> bool {
    segs(project).values().any(|b| contains(b, needle))
}

/// Every path under `dir` with its bytes (None for a directory); empty when `dir` is missing.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    if !dir.exists() {
        return out;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().to_path_buf();
            if p.is_dir() {
                stack.push(p);
                out.insert(rel, None);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    out
}

/// The sealed batches of the store under `project` in `[from, to)`.
fn batches(project: &Path, from: Location, to: Option<Location>) -> Vec<DumpBatch> {
    let s = rdy_store::Store::open(&project.join(".rdy/store")).unwrap();
    let to = to.unwrap_or_else(|| s.log_end());
    let mut out = vec![];
    s.reader()
        .for_each_batch_after(from, to, &mut |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

/// The batches of the dump `file`, and its header's log end.
fn dump_batches(file: &Path) -> (Location, Vec<DumpBatch>) {
    let (mut r, _, h) =
        DumpReader::open(BufReader::new(std::fs::File::open(file).unwrap())).unwrap();
    let mut out = vec![];
    while let Some(item) = r.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    (h.log_end, out)
}

/// The first present frame of `kind` in the store under `project`: its hash, the end of its
/// batch and its payload.
fn first_of(project: &Path, kind: u8) -> Option<(Hash, Location, Vec<u8>)> {
    batches(project, ORIGIN, None).into_iter().find_map(|b| {
        let end = batch_end(&b).unwrap();
        b.frames.iter().find_map(|f| {
            (f.kind() == kind).then(|| {
                (
                    *f.hash(),
                    end,
                    f.present_payload().unwrap_or_default().to_vec(),
                )
            })
        })
    })
}

/// One container record of a dump: its offset, kind and payload length.
#[derive(Clone, Copy, Debug)]
struct RecordAt {
    offset: usize,
    kind: u8,
    len: usize,
}

/// Every record of the dump `bytes`, by its framing.
fn records(bytes: &[u8]) -> Vec<RecordAt> {
    let mut out = vec![];
    let mut at = 12usize;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        out.push(RecordAt {
            offset: at,
            kind: bytes[at + 4],
            len,
        });
        at += HEAD + len;
    }
    out
}

/// A copy of the dump `file` at `out` with the last payload byte of its first record of store
/// kind `kind` flipped; returns that record's offset.
fn flip_frame(file: &Path, out: &Path, kind: u8) -> usize {
    let mut bytes = std::fs::read(file).unwrap();
    let r = records(&bytes)
        .into_iter()
        .find(|r| r.kind == kind && r.len > 0)
        .expect("a record of the kind");
    bytes[r.offset + HEAD + r.len - 1] ^= 1;
    std::fs::write(out, &bytes).unwrap();
    r.offset
}

/// The byte offset of the trailer record of the dump `file`.
fn trailer_at(file: &Path) -> u64 {
    std::fs::metadata(file).unwrap().len() - (HEAD + TRAILER_LEN) as u64
}

/// A write observer for this thread: whether any segment write held `needle`, and how many
/// writes it saw. Dropping the guard removes it.
struct Watch {
    seen: Rc<Cell<bool>>,
    writes: Rc<Cell<u64>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        on_segment_write(None);
    }
}

fn watch(needle: &'static str) -> Watch {
    let seen = Rc::new(Cell::new(false));
    let writes = Rc::new(Cell::new(0));
    let (s, w) = (seen.clone(), writes.clone());
    on_segment_write(Some(Box::new(move |b: &[u8]| {
        w.set(w.get() + 1);
        if contains(b, needle) {
            s.set(true);
        }
        Ok(())
    })));
    Watch { seen, writes }
}

/// Runs `step` once, at the first segment write of this thread (the ingest has started, after
/// the pre-pass). Dropping the guard removes it.
fn at_first_write(step: impl FnOnce() + 'static) -> Watch {
    let step = RefCell::new(Some(step));
    on_segment_write(Some(Box::new(move |_: &[u8]| {
        if let Some(f) = step.borrow_mut().take() {
            f();
        }
        Ok(())
    })));
    Watch {
        seen: Rc::new(Cell::new(false)),
        writes: Rc::new(Cell::new(0)),
    }
}

/// Refuses one segment write of this thread with an injected i/o error: the first one whose
/// bytes `pick` chooses, given the length of the write before it. Nothing of that write reaches
/// the segment; the writes before it do. The guard's `seen` tells whether the fault fired.
fn fail_write(mut pick: impl FnMut(&[u8], usize) -> bool + 'static) -> Watch {
    let seen = Rc::new(Cell::new(false));
    let s = seen.clone();
    let mut last = 0;
    on_segment_write(Some(Box::new(move |b: &[u8]| {
        let before = std::mem::replace(&mut last, b.len());
        if !s.get() && pick(b, before) {
            s.set(true);
            return Err(std::io::Error::other("injected segment write failure"));
        }
        Ok(())
    })));
    Watch {
        seen,
        writes: Rc::new(Cell::new(0)),
    }
}

/// A project with a first landing, the secret landed and tagged `v1`, a full dump `f0`; then the
/// secret's removal, a later landing tagged `v2`, and the increment `i1` after `f0`. Closed.
struct Removed {
    dir: tempfile::TempDir,
    root: PathBuf,
    f0: PathBuf,
    i1: PathBuf,
    f0_end: Location,
    secret_landing: String,
    late_landing: String,
}

fn removed() -> Removed {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r);
    let target = add(&mut r, 1, SECRET);
    let secret_landing = land(&mut r);
    tag(&mut r, "v1.0.0");
    let f0 = dir.path().join("f0.rdydump");
    let f = r.read().dump(&f0).unwrap();
    remove(&mut r, &target);
    add(&mut r, 2, "a later block");
    let late_landing = land(&mut r);
    tag(&mut r, "v2.0.0");
    let i1 = dir.path().join("i1.rdydump");
    r.read().dump_after(&f.chain_end(), &i1).unwrap();
    r.close().unwrap();
    Removed {
        root,
        f0,
        i1,
        f0_end: f.end,
        secret_landing,
        late_landing,
        dir,
    }
}

impl Removed {
    fn files(&self) -> Vec<PathBuf> {
        vec![self.f0.clone(), self.i1.clone()]
    }

    fn to(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// The source apply of `project`'s store: its record hash, the end of its batch and its record.
fn apply_of(project: &Path) -> (Hash, Location, RedactionApply) {
    let (h, end, payload) = first_of(project, kinds::REDACTION_APPLY).expect("an apply");
    (h, end, rdy_store::bounded::decode(&payload).unwrap())
}

#[test]
fn hc7_whole_chain_preserves_the_source_log() {
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r);
    let f0 = d.path().join("f0.rdydump");
    let s0 = r.read().dump(&f0).unwrap();
    add(&mut r, 1, "a second block");
    land(&mut r);
    tag(&mut r, "v1.0.0");
    let i1 = d.path().join("i1.rdydump");
    let s1 = r.read().dump_after(&s0.chain_end(), &i1).unwrap();
    add(&mut r, 2, "a third block");
    land(&mut r);
    let i2 = d.path().join("i2.rdydump");
    let s2 = r.read().dump_after(&s1.chain_end(), &i2).unwrap();
    r.close().unwrap();

    let to = d.path().join("to");
    let files = vec![f0.clone(), i1.clone(), i2.clone()];
    let s = chain_load(&files, &to, None);
    assert_eq!(segs(&to), segs(&root), "the source log, byte for byte");
    assert_eq!(s.dump_format, 4);
    assert_eq!(
        s.store_format,
        rdy_store::format::DumpStore::Features(rdy_store::format::baseline_features())
    );
    let source = batches(&root, ORIGIN, None);
    assert_eq!(s.batches, source.len() as u64);
    assert_eq!(
        s.frames,
        source.iter().map(|b| b.frames.len() as u64).sum::<u64>()
    );
    assert_eq!((s.landings, s.tags), (3, 1));
    let rows: Vec<_> = s
        .chain
        .iter()
        .map(|c| (c.file.clone(), c.kind, c.base, c.end))
        .collect();
    assert_eq!(
        rows,
        vec![
            (f0.display().to_string(), DumpKind::Full, ORIGIN, s0.end),
            (
                i1.display().to_string(),
                DumpKind::Increment,
                s0.end,
                s1.end
            ),
            (
                i2.display().to_string(),
                DumpKind::Increment,
                s1.end,
                s2.end
            ),
        ]
    );
    assert!(s.stopped_at.is_none() && s.warnings.is_empty());
    let v = serde_json::to_value(&s).unwrap();
    assert!(v["stopped_at"].is_null(), "{v}");
    assert!(v.get("warnings").is_none(), "{v}");
    assert_eq!(v["chain"].as_array().unwrap().len(), 3);
    // the loaded project opens and passes fsck
    let got = Rdy::open(&to).unwrap();
    got.fsck().unwrap();
    got.close().unwrap();
    // a single file of dump format 3 is one full row
    let one = d.path().join("one");
    let s = load(&f0, &one).unwrap();
    assert_eq!(s.chain.len(), 1);
    assert_eq!(
        (s.chain[0].kind, s.chain[0].base, s.chain[0].end),
        (DumpKind::Full, ORIGIN, s0.end)
    );
    assert!(s.stopped_at.is_none());
}

#[test]
fn hc7_stopped_load_has_exact_prefix_residues_carried_upgrade_order() {
    let _clock = pin_clock(1_000_000);
    let c = removed();
    let (apply, _, record) = apply_of(&c.root);
    let to = c.to("to");
    let s = chain_load(&c.files(), &to, Some(pos(c.f0_end)));
    // the source prefix, byte for byte
    assert_eq!(
        batches(&to, ORIGIN, Some(c.f0_end)),
        dump_batches(&c.f0).1,
        "the prefix"
    );
    // byte for byte: the source today has erased the secret, so the prefix is compared with
    // the whole load of the full dump alone
    let alone = c.to("alone");
    chain_load(std::slice::from_ref(&c.f0), &alone, None);
    assert_eq!(c.f0_end.segment, 0, "one segment");
    let first = |m: &BTreeMap<String, Vec<u8>>| m.values().next().unwrap().clone();
    assert_eq!(
        first(&segs(&to))[..c.f0_end.offset as usize],
        first(&segs(&alone))[..]
    );
    // then one batch: the missing residues by hash, then the carried record; no upgrade batch
    // follows, as the pair (3, 16) is this rdy's. Both spans are held, yet both residues are
    // here: a carried BLOCK or CHANGE span keeps its residue record, held or not (D-QF2HQ's span
    // carries a residue, and the carried check needs it; a residue holds no removed content).
    // Zeros are written only for a span with held: false (ruling task-7)
    let tail = batches(&to, c.f0_end, None);
    assert_eq!(tail.len(), 1, "{tail:?}");
    let kinds_of: Vec<u8> = tail[0].frames.iter().map(|f| f.kind()).collect();
    assert_eq!(
        kinds_of,
        vec![
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_CARRIED
        ]
    );
    let residues: Vec<Hash> = tail[0].frames[..2].iter().map(|f| *f.hash()).collect();
    assert!(residues[0] < residues[1], "residues sorted by hash");
    let carried_hash = *tail[0].frames[2].hash();
    let carried: RedactionCarried =
        rdy_store::bounded::decode(tail[0].frames[2].present_payload().unwrap()).unwrap();
    assert_eq!(
        (carried.request, carried.apply, carried.landing),
        (record.request, apply, record.landing)
    );
    assert_eq!(carried.spans.len(), 2);
    assert!(carried.spans.iter().all(|s| s.held));
    // the counts are the source's retained batches only
    let (_, prefix) = dump_batches(&c.f0);
    assert_eq!(s.batches, prefix.len() as u64);
    assert_eq!(
        s.frames,
        prefix.iter().map(|b| b.frames.len() as u64).sum::<u64>()
    );
    // the stop and the one warning
    let at = s.stopped_at.as_ref().unwrap();
    assert_eq!(
        (at.position, at.landing, at.tag.as_deref()),
        (c.f0_end, None, None)
    );
    assert_eq!(s.warnings.len(), 1);
    let w = &s.warnings[0];
    assert_eq!(w.code, "W_STOP_BEFORE_REDACTION");
    assert_eq!((w.apply, w.carried), (apply, carried_hash));
    assert_eq!(w.fix, PROJECT_FIX);
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(
        v["stopped_at"],
        json!({"position": {"segment": c.f0_end.segment, "offset": c.f0_end.offset}, "landing": null, "tag": null})
    );
    assert_eq!(v["warnings"][0]["carried"], carried_hash.to_string());
    assert_eq!(
        v["chain"][1]["file"],
        json!(c.i1.display().to_string()),
        "every file is a row, the one after the stop too"
    );
    // a landing stop names its landing; a tag stop its tag and landing
    let s = chain_load(
        &c.files(),
        &c.to("by-landing"),
        Some(stop(&format!("landing:{}", c.secret_landing))),
    );
    let at = s.stopped_at.unwrap();
    assert_eq!(
        (at.landing.map(|h| h.to_string()), at.tag),
        (Some(c.secret_landing.clone()), None)
    );
    let s = chain_load(&c.files(), &c.to("by-tag"), Some(stop("tag:v1.0.0")));
    let at = s.stopped_at.unwrap();
    assert_eq!(
        (at.landing.map(|h| h.to_string()), at.tag),
        (Some(c.secret_landing.clone()), Some("v1.0.0".to_string()))
    );
    let v = serde_json::to_value(
        chain_load(&c.files(), &c.to("tag-json"), Some(stop("tag:v1.0.0"))).stopped_at,
    )
    .unwrap();
    assert_eq!(v["landing"], json!(c.secret_landing));
    assert_eq!(v["tag"], json!("v1.0.0"));
}

#[test]
fn hc7_removed_at_stop_is_never_written() {
    let _clock = pin_clock(1_000_000);
    let c = removed();
    let (_, apply_end, _) = apply_of(&c.root);
    // the full dump predates the removal: it holds the secret, and the increment its apply
    assert!(contains(&std::fs::read(&c.f0).unwrap(), SECRET));
    let stops = [
        ("whole", None),
        ("apply-batch", Some(pos(apply_end))),
        (
            "late-landing",
            Some(stop(&format!("landing:{}", c.late_landing))),
        ),
        ("late-tag", Some(stop("tag:v2.0.0"))),
    ];
    for (name, at) in stops {
        let w = watch(SECRET);
        let to = c.to(name);
        let s = chain_load(&c.files(), &to, at);
        assert!(w.writes.get() > 0, "{name}: the observer saw the writes");
        assert!(!w.seen.get(), "{name}: a write held the removed payload");
        drop(w);
        assert!(!holds(&to, SECRET), "{name}");
        assert!(s.warnings.is_empty(), "{name}: {:?}", s.warnings);
        let r = Rdy::open(&to).unwrap();
        r.fsck().unwrap();
        r.close().unwrap();
    }
    // the observer does see a payload the load restores: a stop before the removal
    let w = watch(SECRET);
    chain_load(&c.files(), &c.to("before"), Some(pos(c.f0_end)));
    assert!(w.seen.get(), "the held secret is written");
}

#[test]
fn hc7_stop_before_removal_restores_only_held_spans() {
    let _clock = pin_clock(1_000_000);
    // the frames are present in the retained prefix: held, and restored
    let c = removed();
    let to = c.to("held");
    let s = chain_load(&c.files(), &to, Some(pos(c.f0_end)));
    assert!(holds(&to, SECRET));
    assert!(
        s.warnings[0]
            .message
            .ends_with("It holds 2 removed payloads again, and 0 stay removed."),
        "{:?}",
        s.warnings
    );
    let r = Rdy::open(&to).unwrap();
    assert_eq!(r.fsck().unwrap().warnings.len(), 1);
    r.close().unwrap();

    // a full dump written after the removal holds its frames as dropped: a stop before the
    // apply keeps them zero (held: false), and nothing restores the secret
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let target = add(&mut r, 0, SECRET);
    land(&mut r);
    add(&mut r, 1, "another block");
    let before = land(&mut r);
    remove(&mut r, &target);
    let full = d.path().join("full.rdydump");
    r.read().dump(&full).unwrap();
    r.close().unwrap();
    assert!(!contains(&std::fs::read(&full).unwrap(), SECRET));
    let to = d.path().join("to");
    let w = watch(SECRET);
    let s = chain_load(
        std::slice::from_ref(&full),
        &to,
        Some(stop(&format!("landing:{before}"))),
    );
    assert!(!w.seen.get());
    drop(w);
    assert!(!holds(&to, SECRET));
    assert_eq!(s.warnings.len(), 1);
    assert!(
        s.warnings[0]
            .message
            .ends_with("It holds 0 removed payloads again, and 2 stay removed."),
        "{:?}",
        s.warnings
    );
    let tail = batches(&to, s.stopped_at.as_ref().unwrap().position, None);
    let carried: RedactionCarried =
        rdy_store::bounded::decode(tail[0].frames.last().unwrap().present_payload().unwrap())
            .unwrap();
    assert!(carried.spans.iter().all(|s| !s.held), "{carried:?}");
    // fsck repeats a warning only for spans still held
    let r = Rdy::open(&to).unwrap();
    assert!(r.fsck().unwrap().warnings.is_empty());
    r.close().unwrap();
}

#[test]
fn hc7_two_stopped_loads_have_identical_log_bytes() {
    let c = removed();
    let one = {
        let _clock = pin_clock(5_000_000);
        chain_load(&c.files(), &c.to("one"), Some(pos(c.f0_end)));
        segs(&c.to("one"))
    };
    let two = {
        let _clock = pin_clock(9_000_000);
        chain_load(
            &c.files(),
            &c.to("another-directory-name"),
            Some(pos(c.f0_end)),
        );
        segs(&c.to("another-directory-name"))
    };
    assert_eq!(one, two);
    // the same stop by another selector that names the same batch end gives the same bytes
    let three = c.to("three");
    let s = chain_load(&c.files(), &three, Some(stop("tag:v1.0.0")));
    if s.stopped_at.unwrap().position == c.f0_end {
        assert_eq!(segs(&three), one);
    }
}

#[test]
fn hc7_stopped_store_dumps_loads_reindexes_and_accepts_writes() {
    let _clock = pin_clock(1_000_000);
    let c = removed();
    let to = c.to("to");
    chain_load(&c.files(), &to, Some(pos(c.f0_end)));
    let mut r = Rdy::open(&to).unwrap();
    let before = r.fsck().unwrap();
    assert_eq!(before.warnings.len(), 1);
    r.reindex().unwrap();
    let after = r.fsck().unwrap();
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    add(&mut r, 9, "a write after the load");
    land(&mut r);
    let again = c.to("again.rdydump");
    r.read().dump(&again).unwrap();
    r.close().unwrap();
    // its dump loads whole: the carried record is retained, so nothing new is carried
    let to2 = c.to("to2");
    let s = chain_load(std::slice::from_ref(&again), &to2, None);
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    assert_eq!(segs(&to2), segs(&to));
    let r = Rdy::open(&to2).unwrap();
    assert_eq!(
        r.fsck().unwrap().warnings.len(),
        1,
        "the span is still held"
    );
    r.close().unwrap();
}

/// A project of three landings with the full dump `f0` and the increments `i1` and `i2`, and a
/// fork of it at `f0` with an increment `i1x` of other content after `f0`.
struct Three {
    dir: tempfile::TempDir,
    f0: PathBuf,
    i1: PathBuf,
    i2: PathBuf,
    i1x: PathBuf,
    f0_end: Location,
    i1_end: Location,
}

fn three() -> Three {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&dir.path().join("p"), "rdy-standard").unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r);
    let f0 = dir.path().join("f0.rdydump");
    let s0 = r.read().dump(&f0).unwrap();
    let fork = dir.path().join("fork");
    load(&f0, &fork).unwrap();
    add(&mut r, 1, "a second block");
    land(&mut r);
    let i1 = dir.path().join("i1.rdydump");
    let s1 = r.read().dump_after(&s0.chain_end(), &i1).unwrap();
    add(&mut r, 2, "a third block");
    land(&mut r);
    let i2 = dir.path().join("i2.rdydump");
    r.read().dump_after(&s1.chain_end(), &i2).unwrap();
    r.close().unwrap();
    let mut x = Rdy::open(&fork).unwrap();
    add(&mut x, 1, "another second block");
    land(&mut x);
    let i1x = dir.path().join("i1x.rdydump");
    x.read().dump_after(&s0.chain_end(), &i1x).unwrap();
    x.close().unwrap();
    Three {
        dir,
        f0,
        i1,
        i2,
        i1x,
        f0_end: s0.end,
        i1_end: s1.end,
    }
}

/// Writes the dump `file` again at `out` with the same header and batches, and its `.rdy.yaml`
/// one byte longer: a valid dump whose records differ.
fn rewrite_files(file: &Path, out: &Path) {
    use rdy_store::dump::{DumpWriter, RemovalTally};
    let (mut r, _, header) =
        DumpReader::open(BufReader::new(std::fs::File::open(file).unwrap())).unwrap();
    let mut files = vec![];
    let mut bs = vec![];
    while let Some(item) = r.next().unwrap() {
        match item {
            DumpItem::File { path, mut bytes } => {
                if path == ".rdy.yaml" {
                    bytes.push(b'\n');
                }
                files.push((path, bytes));
            }
            DumpItem::Batch(b) => bs.push(b),
        }
    }
    let mut w = DumpWriter::new(std::fs::File::create(out).unwrap(), &header).unwrap();
    for (p, b) in &files {
        w.file(p, b).unwrap();
    }
    let mut tally = RemovalTally::default();
    for b in &bs {
        tally.observe(b).unwrap();
        w.batch(b).unwrap();
    }
    w.finish(tally.facts().unwrap()).unwrap();
}

/// A target directory that holds a file of its own; its tree before the load.
fn kept_target(dir: &Path, name: &str) -> (PathBuf, BTreeMap<PathBuf, Option<Vec<u8>>>) {
    let to = dir.join(name);
    std::fs::create_dir_all(&to).unwrap();
    std::fs::write(to.join("keep.txt"), b"mine").unwrap();
    let before = tree(&to);
    (to, before)
}

#[test]
fn hc7_later_bad_file_or_changed_input_restores_target() {
    let _clock = pin_clock(1_000_000);
    let t = three();
    let d = t.dir.path();

    // a file wholly after the stop is checked whole before any target write
    let bad_i2 = d.join("bad-i2.rdydump");
    flip_frame(&t.i2, &bad_i2, kinds::BLOCK);
    let (to, before) = kept_target(d, "after-stop");
    let w = watch(SECRET);
    let e = chain_err(
        &[t.f0.clone(), t.i1.clone(), bad_i2.clone()],
        &to,
        Some(pos(t.i1_end)),
    );
    assert_eq!(e.code, E_DUMP, "{e:?}");
    assert!(
        e.message
            .starts_with(&format!("{} is damaged at byte ", bad_i2.display())),
        "{e:?}"
    );
    assert_eq!(w.writes.get(), 0, "no target write before the refusal");
    drop(w);
    assert_eq!(tree(&to), before);

    // a retained payload is hashed by the ingest: its refusal undoes the store
    let bad_f0 = d.join("bad-f0.rdydump");
    let at = flip_frame(&t.f0, &bad_f0, kinds::BLOCK);
    let (to, before) = kept_target(d, "retained");
    let e = chain_err(&[bad_f0.clone(), t.i1.clone()], &to, None);
    assert_eq!(e.code, E_DUMP, "{e:?}");
    assert_eq!(
        e.message,
        format!(
            "{} is damaged at byte {at}: a record's hash does not match its bytes",
            bad_f0.display()
        )
    );
    assert_eq!(e.fix, DAMAGED_FIX);
    assert_eq!(tree(&to), before);

    // a retained file that changes after the pre-pass: another increment of the same base has
    // another header, refused at it
    let i1 = d.join("swap-i1.rdydump");
    std::fs::copy(&t.i1, &i1).unwrap();
    let (to, before) = kept_target(d, "changed-header");
    let (from, into) = (t.i1x.clone(), i1.clone());
    let g = at_first_write(move || {
        std::fs::copy(&from, &into).unwrap();
    });
    let e = chain_err(
        &[t.f0.clone(), i1.clone(), t.i2.clone()],
        &to,
        Some(pos(t.i1_end)),
    );
    drop(g);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_DUMP,
            format!(
                "{} is damaged at byte 12: dump changed during load",
                i1.display()
            )
            .as_str(),
            DAMAGED_FIX
        )
    );
    assert_eq!(tree(&to), before);

    // the same header over other records: the trailer is not the one the pre-pass read
    let i1 = d.join("swap-i1-body.rdydump");
    std::fs::copy(&t.i1, &i1).unwrap();
    let other = d.join("i1-other-file.rdydump");
    rewrite_files(&t.i1, &other);
    let (to, before) = kept_target(d, "changed-retained");
    let (from, into) = (other.clone(), i1.clone());
    let g = at_first_write(move || {
        std::fs::copy(&from, &into).unwrap();
    });
    let e = chain_err(
        &[t.f0.clone(), i1.clone(), t.i2.clone()],
        &to,
        Some(pos(t.i1_end)),
    );
    drop(g);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_DUMP,
            format!(
                "{} is damaged at byte {}: dump changed during load",
                i1.display(),
                trailer_at(&i1)
            )
            .as_str(),
            DAMAGED_FIX
        )
    );
    assert_eq!(tree(&to), before);

    // a file wholly after the stop that changes after the pre-pass: its envelope is read again
    let i2 = d.join("swap-i2.rdydump");
    std::fs::copy(&t.i2, &i2).unwrap();
    let other = d.join("i2-other-file.rdydump");
    rewrite_files(&t.i2, &other);
    let (to, before) = kept_target(d, "changed-after");
    let (from, into) = (other.clone(), i2.clone());
    let g = at_first_write(move || {
        std::fs::copy(&from, &into).unwrap();
    });
    let e = chain_err(
        &[t.f0.clone(), t.i1.clone(), i2.clone()],
        &to,
        Some(pos(t.f0_end)),
    );
    drop(g);
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_DUMP,
            format!(
                "{} is damaged at byte {}: dump changed during load",
                i2.display(),
                trailer_at(&i2)
            )
            .as_str()
        )
    );
    assert_eq!(tree(&to), before);

    // a retained file that is gone when the ingest reopens it: its path's own error
    let gone = d.join("gone-i1.rdydump");
    std::fs::copy(&t.i1, &gone).unwrap();
    let (to, before) = kept_target(d, "gone");
    let path = gone.clone();
    let g = at_first_write(move || {
        std::fs::remove_file(&path).unwrap();
    });
    let e = chain_err(&[t.f0.clone(), gone.clone()], &to, None);
    drop(g);
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_INPUT,
            format!("{} does not exist", gone.display()).as_str()
        )
    );
    assert_eq!(tree(&to), before);
    // and the unchanged chain loads into the same target
    let (to, _) = kept_target(d, "ok");
    chain_load(&[t.f0.clone(), t.i1.clone(), t.i2.clone()], &to, None);
}

#[test]
fn hc7_failures_at_each_placement_step_restore_only_owned_paths() {
    let _clock = pin_clock(1_000_000);
    let t = three();
    let d = t.dir.path();
    let files = vec![t.f0.clone(), t.i1.clone()];

    // a project file another process writes during the ingest: the write refuses, and only
    // what the load made is taken back
    let (to, before) = kept_target(d, "file");
    let foreign = to.join(".rdy.yaml");
    let at = foreign.clone();
    let g = at_first_write(move || std::fs::write(&at, b"name: theirs\n").unwrap());
    let e = chain_err(&files, &to, None);
    drop(g);
    assert_eq!(e.code, E_IO, "{e:?}");
    let mut want = before.clone();
    want.insert(".rdy.yaml".into(), Some(b"name: theirs\n".to_vec()));
    assert_eq!(tree(&to), want);

    // a store another process puts in place during the ingest: the rename refuses, and that
    // store stays
    let (to, before) = kept_target(d, "store");
    let theirs = to.join(".rdy/store");
    let at = theirs.clone();
    let g = at_first_write(move || std::fs::create_dir(&at).unwrap());
    let e = chain_err(&files, &to, None);
    drop(g);
    assert_eq!(e.code, E_ALREADY_EXISTS, "{e:?}");
    let mut want = before.clone();
    want.insert(".rdy".into(), None);
    want.insert(".rdy/store".into(), None);
    assert_eq!(tree(&to), want);

    // the ingest's first segment write fails: nothing of the load stays
    let (to, before) = kept_target(d, "write");
    let g = fail_write(|_, _| true);
    let e = chain_err(&files, &to, None);
    assert!(g.seen.get(), "the fault fired");
    drop(g);
    assert_eq!(e.code, E_IO, "{e:?}");
    assert!(
        e.message.contains("injected segment write failure"),
        "{e:?}"
    );
    assert_eq!(tree(&to), before);

    // a write that fails half way through a frame: the removed frame's header is in the
    // segment, its zeros are refused; the torn store.loading goes, and the target is as it was
    let c = removed();
    let (to, before) = kept_target(c.dir.path(), "torn");
    let g = fail_write(|b, before| before == HEAD && b.iter().all(|x| *x == 0));
    let e = chain_err(&c.files(), &to, None);
    assert!(g.seen.get(), "the fault fired after a frame header");
    drop(g);
    assert_eq!(e.code, E_IO, "{e:?}");
    assert_eq!(tree(&to), before);

    // (the ingest's failed sync of the segments directory is in `load_chain_unix.rs`)

    // the ingest cannot write FORMAT (a directory has its name): the same
    let (to, before) = kept_target(d, "format");
    let at = to.join(".rdy/store.loading/FORMAT");
    let g = at_first_write(move || std::fs::create_dir(&at).unwrap());
    let e = chain_err(&files, &to, None);
    drop(g);
    assert_eq!(e.code, E_IO, "{e:?}");
    assert_eq!(tree(&to), before);

    // the rename of store.loading to store fails: the store, the project files and the
    // directories the load made are gone, in a target that existed and in one that did not
    let (to, before) = kept_target(d, "rename");
    fail_next_placement_for_tests(Some(PlaceFault::Rename));
    let e = chain_err(&files, &to, None);
    assert_eq!(e.code, E_IO, "{e:?}");
    assert!(e.message.contains("injected rename failure"), "{e:?}");
    assert_eq!(tree(&to), before);
    let new = d.join("new-rename").join("deeper");
    fail_next_placement_for_tests(Some(PlaceFault::Rename));
    chain_err(&files, &new, None);
    assert!(!d.join("new-rename").exists());

    // the sync of `.rdy` after the rename fails: the placed store goes back to its loading
    // name and is removed with everything else the load made
    let (to, before) = kept_target(d, "placed");
    fail_next_placement_for_tests(Some(PlaceFault::SyncAfterRename));
    let e = chain_err(&files, &to, None);
    assert_eq!(e.code, E_IO, "{e:?}");
    assert!(e.message.contains("injected sync failure"), "{e:?}");
    assert_eq!(tree(&to), before);
    // a disarmed fault does nothing: the next load passes
    chain_load(&files, &d.join("after-faults"), None);

    // a load that fails its fsck after the rename: the store, the files and the directories
    // it made are gone, and a target that did not exist does not exist
    let src = d.join("forged");
    let mut r = Rdy::init(&src, "rdy-standard").unwrap();
    add(&mut r, 0, "a block");
    land(&mut r);
    let f = d.join("forged-f0.rdydump");
    let s0 = r.read().dump(&f).unwrap();
    r.store_mut()
        .set_refs_unchecked_for_tests(vec![("view/forged".into(), vec![0u8; 32])])
        .unwrap();
    r.close().unwrap();
    let r = Rdy::open(&src).unwrap();
    let i = d.join("forged-i1.rdydump");
    r.read().dump_after(&s0.chain_end(), &i).unwrap();
    r.close().unwrap();
    let (to, before) = kept_target(d, "fsck");
    let e = chain_err(&[f.clone(), i.clone()], &to, None);
    assert_eq!(e.code, E_DUMP, "{e:?}");
    assert!(
        e.message
            .starts_with("the dump loads, but the project fails fsck: "),
        "{e:?}"
    );
    assert_eq!(tree(&to), before);
    let new = d.join("new").join("deeper");
    chain_err(&[f.clone(), i.clone()], &new, None);
    assert!(!d.join("new").exists());
    // the same chain stopped before the forged ref loads
    chain_load(&[f, i], &d.join("stopped"), Some(pos(s0.end)));
}

#[test]
fn hc7_chain_pending_limits_do_not_reset() {
    // the chain load runs one ingest for every file: its pending-drop bounds are pinned at the
    // store (`rdy-store/tests/ingest_chain.rs`); here, the load of a chain with drops in two
    // files, each released by its apply, takes them all through one ingest
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let a = add(&mut r, 0, SECRET);
    land(&mut r);
    let b = add(&mut r, 1, "The second code is ZEBRA-1234.");
    land(&mut r);
    remove(&mut r, &a);
    let f0 = d.path().join("f0.rdydump");
    let s0 = r.read().dump(&f0).unwrap();
    remove(&mut r, &b);
    let i1 = d.path().join("i1.rdydump");
    r.read().dump_after(&s0.chain_end(), &i1).unwrap();
    r.close().unwrap();
    let to = d.path().join("to");
    let s = chain_load(&[f0, i1], &to, None);
    assert_eq!(segs(&to), segs(&root));
    assert!(s.warnings.is_empty());
}

#[test]
fn hc7_last_contributing_file_is_the_only_file_set() {
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r);
    let f0 = d.path().join("f0.rdydump");
    let s0 = r.read().dump(&f0).unwrap();
    let config = root.join(".rdy.yaml");
    let first_config = std::fs::read(&config).unwrap();
    let mut second_config = first_config.clone();
    second_config.extend_from_slice(b"# changed after the full dump\n");
    std::fs::write(&config, &second_config).unwrap();
    r.close().unwrap();
    let mut r = Rdy::open(&root).unwrap();
    add(&mut r, 1, "a later block");
    land(&mut r);
    let i1 = d.path().join("i1.rdydump");
    r.read().dump_after(&s0.chain_end(), &i1).unwrap();
    r.close().unwrap();
    let files = vec![f0, i1];
    // a stop inside the full dump: its files only
    let early = d.path().join("early");
    let s = chain_load(&files, &early, Some(pos(s0.end)));
    assert!(s.files_written.contains(&".rdy.yaml".to_string()));
    assert_eq!(
        std::fs::read(early.join(".rdy.yaml")).unwrap(),
        first_config
    );
    // no stop: the increment's files only
    let late = d.path().join("late");
    let s = chain_load(&files, &late, None);
    assert!(s.files_written.contains(&".rdy.yaml".to_string()));
    assert_eq!(
        std::fs::read(late.join(".rdy.yaml")).unwrap(),
        second_config
    );
    // a target that already holds the stop file's bytes keeps them; the other file's bytes
    // are a conflict
    let kept = d.path().join("kept");
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join(".rdy.yaml"), &first_config).unwrap();
    let s = chain_load(&files, &kept, Some(pos(s0.end)));
    assert!(s.files_kept.contains(&".rdy.yaml".to_string()));
    let clash = d.path().join("clash");
    std::fs::create_dir_all(&clash).unwrap();
    std::fs::write(clash.join(".rdy.yaml"), &first_config).unwrap();
    let e = chain_err(&files, &clash, None);
    assert_eq!(e.code, E_ALREADY_EXISTS, "{e:?}");
}

#[test]
fn hc7_prefix_only_input_has_no_future_warning() {
    let _clock = pin_clock(1_000_000);
    let c = removed();
    // a chain cut before the file of the apply knows nothing of it
    let to = c.to("prefix");
    let s = chain_load(std::slice::from_ref(&c.f0), &to, None);
    assert!(s.warnings.is_empty());
    assert!(first_of(&to, kinds::REDACTION_CARRIED).is_none());
    assert!(
        holds(&to, SECRET),
        "the prefix holds the secret, as its source did"
    );
    // a whole load through the apply carries nothing
    let to = c.to("whole");
    let s = chain_load(&c.files(), &to, None);
    assert!(s.warnings.is_empty());
    assert!(first_of(&to, kinds::REDACTION_CARRIED).is_none());
    assert!(!holds(&to, SECRET));
}

/// Writes `bytes` as one batch file under `dir` and opens it.
fn batch_file(dir: &Path, name: &str, bytes: &[u8]) -> Vec<std::fs::File> {
    let p = dir.join(format!("{name}.rdysync"));
    std::fs::write(&p, bytes).unwrap();
    vec![std::fs::File::open(&p).unwrap()]
}

/// The whole history of tag `name` of `r`, as one stream batch, and the tag's landing.
fn export(r: &Rdy, name: &str) -> (Vec<u8>, Hash) {
    let (_, hash, _) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap();
    let landing = r.store().read_tag(&hash).unwrap().landing;
    let mut out = Vec::new();
    stream_export::write(&r.read(), hash, &BTreeSet::new(), &mut out).unwrap();
    (out, landing)
}

fn take(h: &mut Rdy, dir: &Path, src: &Rdy, name: &str) {
    let (bytes, landing) = export(src, name);
    stream_take::take(
        h,
        &batch_file(dir, name, &bytes),
        UploadTarget {
            tag: name.into(),
            landing,
        },
    )
    .unwrap();
}

fn tag_names(r: &Rdy) -> Vec<String> {
    let mut v: Vec<String> = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    v.sort();
    v
}

#[test]
fn hc7_holder_chain_preserves_divergent_tags_and_local_removal() {
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    // two projects of one repo: they share v1.0.0, then diverge (v2.0.0 on the right); the left
    // then removes the secret and tags v1.1.0
    let mut left = Rdy::init(&root.join("left"), "rdy-standard").unwrap();
    let secret = add(&mut left, 0, SECRET);
    land(&mut left);
    tag(&mut left, "v1.0.0");
    let shared = root.join("shared.rdydump");
    left.read().dump(&shared).unwrap();
    load(&shared, &root.join("right")).unwrap();
    let mut right = Rdy::open(&root.join("right")).unwrap();
    add(&mut right, 1, "the right side");
    land(&mut right);
    tag(&mut right, "v2.0.0");
    let repo: RepoId = left.store().repo_id().unwrap().unwrap();
    let hdir = root.join("holder");
    let mut h = Rdy::create_holder(&hdir, repo).unwrap();
    take(&mut h, root, &left, "v1.0.0");
    take(&mut h, root, &right, "v2.0.0");
    let f0 = root.join("h0.rdydump");
    let s0 = h.read().dump(&f0).unwrap();
    remove(&mut left, &secret);
    tag(&mut left, "v1.1.0");
    take(&mut h, root, &left, "v1.1.0");
    let i1 = root.join("h1.rdydump");
    h.read().dump_after(&s0.chain_end(), &i1).unwrap();
    h.close().unwrap();
    left.close().unwrap();
    right.close().unwrap();
    assert!(
        !holds(&hdir, SECRET),
        "the take removed the holder's copies"
    );
    let files = vec![f0, i1];

    // the whole chain: the holder's log byte for byte, its local removal included
    let whole = root.join("whole");
    let s = chain_load(&files, &whole, None);
    assert_eq!(segs(&whole), segs(&hdir));
    assert!(s.warnings.is_empty() && s.files_written.is_empty());
    assert_eq!(s.tags, 3);
    let got = Rdy::open_holder(&whole).unwrap();
    assert_eq!(got.store().mode().unwrap(), StoreMode::Holder);
    assert_eq!(
        tag_names(&got),
        vec!["v1.0.0", "v1.1.0", "v2.0.0"],
        "the divergent tags"
    );
    got.fsck().unwrap();
    got.close().unwrap();

    // stopped before the take of the removal: the divergent tags, the secret held, and the
    // holder's warning, which persists
    let stopped = root.join("stopped");
    let s = chain_load(&files, &stopped, Some(pos(s0.end)));
    assert_eq!(s.next, "read the holder through the adapter at a tag");
    assert_eq!(s.warnings.len(), 1);
    assert_eq!(s.warnings[0].fix, HOLDER_FIX);
    assert!(holds(&stopped, SECRET));
    // the carried record names the holder's own physical spans: its local frames of the secret
    let tail = batches(&stopped, s0.end, None);
    let carried: RedactionCarried =
        rdy_store::bounded::decode(tail[0].frames.last().unwrap().present_payload().unwrap())
            .unwrap();
    let local: BTreeSet<(u32, u64)> = batches(&stopped, ORIGIN, Some(s0.end))
        .iter()
        .flat_map(|b| {
            let mut at = b.start;
            b.frames
                .iter()
                .map(|f| {
                    let here = at;
                    at.offset += HEAD as u64 + u64::from(f.original_len());
                    (
                        here,
                        f.present_payload().is_some_and(|p| contains(p, SECRET)),
                    )
                })
                .collect::<Vec<_>>()
        })
        .filter(|(_, secret)| *secret)
        .map(|(l, _)| (l.segment, l.offset))
        .collect();
    let named: BTreeSet<(u32, u64)> = carried
        .spans
        .iter()
        .map(|s| (s.place.segment, s.place.offset))
        .collect();
    assert_eq!(named, local);
    assert!(carried.spans.iter().all(|s| s.held));
    for _ in 0..2 {
        let got = Rdy::open_holder(&stopped).unwrap();
        assert_eq!(tag_names(&got), vec!["v1.0.0", "v2.0.0"]);
        let ws = got.fsck().unwrap().warnings;
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].fix, HOLDER_FIX);
        got.close().unwrap();
    }
}

#[test]
fn hc7_legacy_loads_keep_hashes_and_one_upgrade() {
    let d = tempfile::tempdir().unwrap();
    for name in [
        "v1-s10",
        "v1-s11",
        "v1-s12",
        "v2-s13",
        "v2-s14",
        "v2-s15",
        "v2-s15-holder",
    ] {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../rdy-qa/fixtures/dump/{name}.rdydump"));
        let (log_end, source) = dump_batches(&file);
        for (how, to) in [
            ("load", d.path().join(name)),
            ("chain", d.path().join(format!("{name}-c"))),
        ] {
            let s = if how == "load" {
                load(&file, &to).unwrap()
            } else {
                chain_load(std::slice::from_ref(&file), &to, None)
            };
            assert_eq!(s.chain.len(), 1, "{name}");
            let row = &s.chain[0];
            assert_eq!(
                (row.file.as_str(), row.kind, row.base, row.end),
                (
                    file.display().to_string().as_str(),
                    DumpKind::Full,
                    ORIGIN,
                    log_end
                ),
                "{name}"
            );
            assert!(s.stopped_at.is_none() && s.warnings.is_empty(), "{name}");
            assert_eq!(s.batches, source.len() as u64, "{name}");
            // the source batches with their hashes, then one upgrade batch
            assert_eq!(batches(&to, ORIGIN, Some(log_end)), source, "{name}");
            let tail = batches(&to, log_end, None);
            assert_eq!(tail.len(), 1, "{name}: {tail:?}");
            assert!(
                tail[0].frames.iter().all(|f| f.kind() == kinds::REF_UPDATE),
                "{name}"
            );
        }
    }
}

/// Opens the store under `dir` in its own mode.
fn open_any(dir: &Path) -> Rdy {
    if dir.join(".rdy.yaml").exists() {
        Rdy::open(dir).unwrap()
    } else {
        Rdy::open_holder(dir).unwrap()
    }
}

#[test]
fn hc7_stop_before_a_migration_upgrade_keeps_the_prefix_format() {
    // a released store moved by this rdy holds an upgrade batch at the end of its source log;
    // its dump can still stop at any batch before that upgrade: the stopped prefix holds the
    // old store format, so the load appends its own upgrade batch last, and keeps
    // the chain's repo id when the prefix set none
    let d = tempfile::tempdir().unwrap();
    for name in ["v1-s10", "v2-s15", "v2-s15-holder"] {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../rdy-qa/fixtures/dump/{name}.rdydump"));
        let (migrated_at, _) = dump_batches(&file);
        let moved = d.path().join(format!("{name}-moved"));
        load(&file, &moved).unwrap();
        let src = open_any(&moved);
        let repo = src.store().repo_id().unwrap().unwrap();
        let f0 = d.path().join(format!("{name}-3.rdydump"));
        src.read().dump(&f0).unwrap();
        src.close().unwrap();
        let landings = batches(&moved, ORIGIN, Some(migrated_at))
            .iter()
            .filter(|b| b.frames.iter().any(|f| f.kind() == kinds::LANDING))
            .count();
        assert!(landings > 0, "{name}: the fixture lands");
        for (n, at) in [
            pos(migrated_at),
            stop(&format!("landing:{}", last_landing(&moved, migrated_at))),
        ]
        .into_iter()
        .enumerate()
        {
            let to = d.path().join(format!("{name}-stop{n}"));
            let s = chain_load(std::slice::from_ref(&f0), &to, Some(at.clone()));
            let end = s.stopped_at.unwrap().position;
            assert!(
                end <= migrated_at,
                "{name} {at:?}: stopped before the upgrade"
            );
            assert_eq!(
                batches(&to, ORIGIN, Some(end)),
                batches(&moved, ORIGIN, Some(end)),
                "{name} {at:?}: the source prefix"
            );
            let tail = batches(&to, end, None);
            let last = tail.last().expect("an upgrade batch");
            assert!(
                last.frames.iter().all(|f| f.kind() == kinds::REF_UPDATE),
                "{name} {at:?}: the upgrade batch is last: {tail:?}"
            );
            let got = open_any(&to);
            assert_eq!(got.store().repo_id().unwrap(), Some(repo), "{name} {at:?}");
            got.fsck().unwrap();
            got.close().unwrap();
        }
    }
}

/// The hash of the last landing of the store under `dir` before `end`.
fn last_landing(dir: &Path, end: Location) -> Hash {
    batches(dir, ORIGIN, Some(end))
        .iter()
        .flat_map(|b| b.frames.iter())
        .filter(|f| f.kind() == kinds::LANDING)
        .map(|f| *f.hash())
        .next_back()
        .unwrap()
}

#[test]
fn hc7_damaged_released_dump_keeps_the_released_refusals() {
    // a single file of dump format 1 or 2 with no stop is the released load: decided from its
    // prelude alone, so a cut file or one with bytes after its trailer gets the released texts
    // and positions, not a reading of its last bytes as a trailer
    let d = tempfile::tempdir().unwrap();
    for name in ["v1-s10", "v2-s13", "v2-s15"] {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../rdy-qa/fixtures/dump/{name}.rdydump"));
        let bytes = std::fs::read(&file).unwrap();
        let mut extra = bytes.clone();
        extra.push(0);
        let half = bytes[..bytes.len() / 2].to_vec();
        let ten = bytes[..bytes.len() - 10].to_vec();
        let mut flipped = bytes.clone();
        let mid = flipped.len() / 2;
        flipped[mid] ^= 1;
        for (how, b) in [
            ("extra", extra),
            ("half", half),
            ("ten", ten),
            ("flip", flipped),
        ] {
            let p = d.path().join(format!("{name}-{how}.rdydump"));
            std::fs::write(&p, &b).unwrap();
            // the released reader's refusal of these bytes, read in order
            let (at, what) = released_refusal(&b);
            let want = format!("{} is damaged at byte {at}: {what}", p.display());
            for to in [
                d.path().join(format!("{name}-{how}-a")),
                d.path().join(format!("{name}-{how}-b")),
            ] {
                let got = if to.ends_with(format!("{name}-{how}-a")) {
                    load(&p, &to).unwrap_err()
                } else {
                    chain_err(std::slice::from_ref(&p), &to, None)
                };
                assert_eq!(
                    (got.code, got.message.as_str(), got.fix.as_str()),
                    ("E_DUMP", want.as_str(), DAMAGED_FIX),
                    "{name} {how}"
                );
                assert!(!to.exists(), "{name} {how}");
            }
        }
    }
}

/// Where and why the dump reader refuses `bytes`, read in order through their end.
fn released_refusal(bytes: &[u8]) -> (u64, String) {
    let e = (|| {
        let (mut r, _, _) = DumpReader::open(BufReader::new(bytes))?;
        while r.next()?.is_some() {}
        Ok::<_, rdy_store::StoreError>(())
    })()
    .expect_err("the damaged bytes are refused");
    match e {
        rdy_store::StoreError::Dump { at, what } => (at, what),
        e => panic!("not a damaged dump: {e:?}"),
    }
}
