//! hc5: the engine side of a chain load's pre-pass (D-2FMRR, D-QF2HQ, O11). The input count is
//! checked before any path is opened, then the stop's spelling, then each file's envelope and
//! link in argument order; the scanner's refusals become the chain, stop and path errors; the
//! last file that contributes a retained batch owns the project files; and the pre-pass makes
//! no target, store or spool. A valid source whose later removal names more physical spans than
//! one carried record holds is refused before any target exists.
use rdy_engine::load_chain::{InspectFacts, inspect_facts};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::Role;
use serde_json::json;
use std::path::{Path, PathBuf};

const SECRET: &str = "The launch code is ZEBRA-7731.";
const LOAD_CHAIN_FIX: &str = "pass the full dump, then each increment in chain order";
const STOP_FIX: &str = "name a landing, a tag or a batch end that the dump chain holds";

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer)
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver)
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
    let p = r.propose(&writer(), "work", "Work", None).unwrap();
    r.land(&approver(), &p.id).unwrap()
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
        .propose(&writer(), "redact", "Remove the code", None)
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
}

fn err(r: Result<InspectFacts, RdyError>) -> RdyError {
    match r {
        Ok(f) => panic!("the pre-pass passed: {f:?}"),
        Err(e) => e,
    }
}

fn rows(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../rdy-qa/fixtures/dump/{name}"))
}

/// Every path under `dir`, relative, with each file's length.
fn tree(dir: &Path) -> Vec<(String, u64)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(dir).unwrap().display().to_string();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(p);
                out.push((rel, 0));
            } else {
                out.push((rel, m.len()));
            }
        }
    }
    out.sort();
    out
}

/// A project with landed writes, a full dump `full`, more writes and a pack file, and the
/// increment `inc` after it.
struct Chain {
    dir: tempfile::TempDir,
    full: PathBuf,
    inc: PathBuf,
    full_end: rdy_store::segment::Location,
    first_landing: String,
    late_landing: String,
}

fn chain() -> Chain {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    add(&mut r, 0, "a first block");
    let first_landing = land(&mut r);
    add(&mut r, 1, "a second block");
    land(&mut r);
    let full = dir.path().join("full.rdydump");
    let f = r.read().dump(&full).unwrap();
    std::fs::write(root.join(".rdy/packs/extra.yaml"), "name: extra\n").unwrap();
    add(&mut r, 2, "a later block");
    let late_landing = land(&mut r);
    let inc = dir.path().join("inc.rdydump");
    r.read().dump_after(&f.chain_end(), &inc).unwrap();
    Chain {
        dir,
        full,
        inc,
        full_end: f.end,
        first_landing,
        late_landing,
    }
}

#[test]
fn hc5_input_precedence_and_4096_limit() {
    let d = tempfile::tempdir().unwrap();
    let missing = |n: usize| -> Vec<PathBuf> {
        (0..n)
            .map(|i| d.path().join(format!("missing-{i}.rdydump")))
            .collect()
    };
    // no file
    let e = err(inspect_facts(&[], None));
    assert_eq!(
        rows(&e),
        (
            E_BAD_INPUT,
            "A dump chain needs a full dump.",
            "Pass a full dump file first."
        )
    );
    // the count before the stop's spelling and before any path: a full dump and 4096
    // increments at most
    let e = err(inspect_facts(&missing(4098), Some("bogus")));
    assert_eq!(
        rows(&e),
        (
            E_LIMIT,
            "The dump chain exceeds the limit of 4096 increments.",
            "Load from a newer full dump of the store."
        )
    );
    // the stop's spelling before any path
    let e = err(inspect_facts(&missing(4097), Some("bogus")));
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, "Invalid load stop \"bogus\".");
    // then the first path
    let e = err(inspect_facts(&missing(4097), Some("pos:0.1")));
    let first = d.path().join("missing-0.rdydump");
    assert_eq!(
        rows(&e),
        (
            E_BAD_INPUT,
            format!("{} does not exist", first.display()).as_str(),
            "check the path of the dump"
        )
    );
    // envelopes and links in argument order: the first file's link before the second path
    let c = chain();
    let gone = c.dir.path().join("gone.rdydump");
    let e = err(inspect_facts(
        &[c.inc.clone(), gone.clone()],
        Some("tag:v9"),
    ));
    assert_eq!(
        rows(&e),
        (
            E_DUMP_CHAIN,
            format!(
                "{}: it is an increment; a dump chain starts with a full dump",
                c.inc.display()
            )
            .as_str(),
            LOAD_CHAIN_FIX
        )
    );
    let e = err(inspect_facts(
        &[c.full.clone(), gone.clone()],
        Some("tag:v9"),
    ));
    assert_eq!(e.message, format!("{} does not exist", gone.display()));
    let e = err(inspect_facts(&[c.full.clone(), c.full.clone()], None));
    assert_eq!(
        rows(&e),
        (
            E_DUMP_CHAIN,
            format!(
                "{}: it is a full dump; only the first file of a dump chain is one",
                c.full.display()
            )
            .as_str(),
            LOAD_CHAIN_FIX
        )
    );
    // a released file: alone it is the single-file load's; with a stop or in a chain, refused
    let legacy = fixture("v2-s15.rdydump");
    let alone = inspect_facts(std::slice::from_ref(&legacy), None).unwrap();
    assert_eq!(alone.legacy, Some(2));
    let e = err(inspect_facts(
        std::slice::from_ref(&legacy),
        Some("pos:0.1"),
    ));
    assert_eq!(
        rows(&e),
        (
            E_LOAD_STOP,
            format!(
                "{}: dump format 2 holds no chain facts; a stop needs dump format 3",
                legacy.display()
            )
            .as_str(),
            STOP_FIX
        )
    );
    let e = err(inspect_facts(&[legacy.clone(), c.inc.clone()], None));
    assert_eq!(
        rows(&e),
        (
            E_DUMP_CHAIN,
            format!(
                "{}: dump format 2 holds no chain facts; a dump chain starts with a full dump of format 3",
                legacy.display()
            )
            .as_str(),
            LOAD_CHAIN_FIX
        )
    );
    // the stop is resolved after the links and the framing: its own rows
    let files = vec![c.full.clone(), c.inc.clone()];
    let e = err(inspect_facts(&files, Some("tag:v9")));
    assert_eq!(
        rows(&e),
        (E_LOAD_STOP, "the dump chain holds no tag v9", STOP_FIX)
    );
    let e = err(inspect_facts(&files, Some("landing:abcdefabcdef")));
    assert_eq!(
        rows(&e),
        (
            E_LOAD_STOP,
            "the dump chain holds no landing abcdefabcdef",
            STOP_FIX
        )
    );
    let e = err(inspect_facts(&files, Some("pos:0.1")));
    assert_eq!(
        rows(&e),
        (
            E_LOAD_STOP,
            "0.1 is not the end of a batch in the dump chain",
            STOP_FIX
        )
    );
    // damage in a file's body is the positioned E_DUMP of that file
    let mut bytes = std::fs::read(&c.inc).unwrap();
    let n = bytes.len();
    bytes[n - 30] ^= 0x01;
    let bad = c.dir.path().join("bad.rdydump");
    std::fs::write(&bad, &bytes).unwrap();
    let e = err(inspect_facts(&[c.full.clone(), bad.clone()], None));
    assert_eq!(e.code, E_DUMP);
    assert!(
        e.message
            .starts_with(&format!("{} is damaged at byte ", bad.display())),
        "{e:?}"
    );
    assert_eq!(e.fix, "dump the project again; rdy never repairs a dump");
}

#[test]
fn hc5_last_contributing_file_owns_project_files() {
    let c = chain();
    let files = vec![c.full.clone(), c.inc.clone()];
    let base = vec![".rdy.yaml".to_string()];
    let with_extra = vec![".rdy.yaml".to_string(), ".rdy/packs/extra.yaml".to_string()];
    let packs = |f: &InspectFacts| -> Vec<String> {
        f.owner_files
            .iter()
            .filter(|p| *p == ".rdy.yaml" || p.contains("extra"))
            .cloned()
            .collect()
    };
    // no stop: the last file
    let all = inspect_facts(&files, None).unwrap();
    assert_eq!(all.owner, Some(1));
    assert_eq!(packs(&all), with_extra);
    assert_eq!(all.roles, vec!["retained", "stop"]);
    assert!(!all.explicit_stop);
    // a stop inside the full dump: its files, not the increment's
    let early = inspect_facts(&files, Some(&format!("landing:{}", c.first_landing))).unwrap();
    assert_eq!(early.owner, Some(0));
    assert_eq!(packs(&early), base);
    assert_eq!(early.roles, vec!["stop", "after"]);
    // a stop at the full dump's own end: that file still owns the files
    let at_end = inspect_facts(
        &files,
        Some(&format!("pos:{}.{}", c.full_end.segment, c.full_end.offset)),
    )
    .unwrap();
    assert_eq!(at_end.owner, Some(0));
    assert_eq!(packs(&at_end), base);
    assert_eq!(at_end.end, Some(c.full_end));
    // a stop inside the increment: the increment's files
    let late = inspect_facts(&files, Some(&format!("landing:{}", c.late_landing))).unwrap();
    assert_eq!(late.owner, Some(1));
    assert_eq!(packs(&late), with_extra);
    assert!(late.explicit_stop);
}

#[test]
fn hc5_scan_creates_no_target_or_spool() {
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r);
    let target = add(&mut r, 1, SECRET);
    land(&mut r);
    let full = d.path().join("full.rdydump");
    let f = r.read().dump(&full).unwrap();
    remove(&mut r, &target);
    let inc = d.path().join("inc.rdydump");
    r.read().dump_after(&f.chain_end(), &inc).unwrap();
    drop(r);
    let before = tree(d.path());
    let files = vec![full, inc];
    // a stop before the removal: one carried record is prepared, in memory only
    let stopped = inspect_facts(
        &files,
        Some(&format!("pos:{}.{}", f.end.segment, f.end.offset)),
    )
    .unwrap();
    assert_eq!(stopped.carried, 1);
    assert_eq!(stopped.residues, 2);
    let whole = inspect_facts(&files, None).unwrap();
    assert_eq!(whole.carried, 0);
    assert_eq!(tree(d.path()), before, "the pre-pass wrote nothing");
    // nothing holds the secret but the source store and the full dump that predates the removal
    let holds = |p: &Path| {
        std::fs::read(p)
            .map(|b| b.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()))
            .unwrap_or(false)
    };
    let mut with_secret: Vec<String> = before
        .iter()
        .filter(|(rel, _)| holds(&d.path().join(rel)))
        .map(|(rel, _)| rel.clone())
        .filter(|rel| !rel.starts_with("p/.rdy/store"))
        .collect();
    with_secret.sort();
    assert_eq!(with_secret, vec!["full.rdydump".to_string()]);
}

#[test]
fn hc5_valid_source_with_too_many_carried_spans_refuses_before_target() {
    const COPIES: usize = 100_001;
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let target = add(&mut r, 0, SECRET);
    land(&mut r);
    drop(r);
    // more physical copies of the selected version, appended while the store is closed: each is
    // one span of the removal
    let store = root.join(".rdy/store");
    let s = rdy_store::Store::open(&store).unwrap();
    let frames = rdy_store_frames(&s);
    drop(s);
    let raw = frames
        .iter()
        .find(|(kind, _, body)| {
            *kind == rdy_store::kinds::BLOCK
                && body.windows(SECRET.len()).any(|w| w == SECRET.as_bytes())
        })
        .map(|(_, _, body)| body.clone())
        .expect("the secret's block frame");
    let (mut log, q) = rdy_store::segment::SegmentLog::open_marked(
        &store.join("segments"),
        64 * 1024 * 1024,
        None,
    )
    .unwrap();
    assert!(q.is_empty());
    log.append_batch(&vec![(rdy_store::kinds::BLOCK, raw); COPIES])
        .unwrap();
    log.sync().unwrap();
    drop(log);
    let mut r = Rdy::open(&root).unwrap();
    let full = d.path().join("full.rdydump");
    let f = r.read().dump(&full).unwrap();
    remove(&mut r, &target);
    let inc = d.path().join("inc.rdydump");
    r.read().dump_after(&f.chain_end(), &inc).unwrap();
    drop(r);
    let files = vec![full, inc];
    let before = tree(d.path());
    // stopped before the removal: one carried record would name every copy
    let e = err(inspect_facts(
        &files,
        Some(&format!("pos:{}.{}", f.end.segment, f.end.offset)),
    ));
    assert_eq!(e.code, E_REDACTION_LIMIT, "{e:?}");
    assert_eq!(e.message, "The redaction exceeds 100000 carried spans.");
    assert_eq!(
        e.fix,
        "Stop at another place, or load the whole chain with no stop."
    );
    assert_eq!(tree(d.path()), before);
    // the whole chain still loads: its apply covers every copy
    let whole = inspect_facts(&files, None).unwrap();
    assert_eq!(whole.carried, 0);
}

/// Every data frame of the store `s`: kind, location and payload.
fn rdy_store_frames(s: &rdy_store::Store) -> Vec<(u8, rdy_store::segment::Location, Vec<u8>)> {
    let mut out = vec![];
    s.reader()
        .for_each_record_from(
            rdy_store::segment::Location {
                segment: 0,
                offset: 0,
            },
            s.log_end(),
            |loc, f, _| {
                if let rdy_store::redaction::StoredFrame::Present { kind, payload, .. } = f {
                    out.push((kind, loc, payload.to_vec()));
                }
                Ok(())
            },
        )
        .unwrap();
    out
}
