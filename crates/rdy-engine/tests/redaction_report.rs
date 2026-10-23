#![cfg(unix)]
// Unix only: the report fixtures make symlinks and set file modes (seams rule, FW-XP1 D-1RXEA).
//! rdx5: the complete redaction blast radius and the request (D-6B1W1, D-14SAY, D-EPK1Z, D-33JR0,
//! D-ST423). A report reads the whole log: every stored version, closed views and withdrawn
//! proposals included; every embedding change and shared chunk; every landing, tag, view,
//! proposal, snapshot, review, tension and answer that references or quotes the content. Quote
//! text is transient report output, never a field of a stored record. A dry run changes nothing;
//! recording writes one preparation batch that erases nothing.
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::quarantine;
use rdy_engine::redaction::presentation::{check_budget, report_text};
use rdy_engine::redaction::report::{outside_rows, warnings};
use rdy_engine::redaction::{
    ASSIGNED_KINDS, Limits, QuoteRow, RedactInput, RedactResult, RedactionReport, ReportCopies,
    TextPolicy, compute_report, dry_run, override_limits_for_tests, prepare_request,
    report_computations_for_tests, require_text_policies, text_policy,
};
use rdy_engine::{
    E_BAD_INPUT, E_REDACTION_BUDGET, E_REDACTION_LIMIT, E_REDACTION_REASON, E_REDACTION_STALE,
    E_REDACTION_TARGET, E_SIG_INVALID, Rdy, RdyError, Scope, Session, WriteRequest, WriteResult,
    pin_clock,
};
use rdy_model::redaction::{
    AssessmentSupport, CARRIER_MESSAGE, CopyKind, CopyRow, CopyStatus, ImpactKind, ImpactRelation,
    ImpactRow, ReasonClass, RedactTarget, encode_into,
};
use rdy_model::signature::Signature;
use rdy_model::{
    ActorRun, AltSet, AltStatus, Answer, AnswerAct, BlockId, Citation, Hash, Intent, IntentStatus,
    PairItem, Provenance, RelayEvidence, Review, ReviewSubject, Role, SemanticTension, Verdict,
};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const SECRET: &str = "The launch code is ZEBRA-7731.";
const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer)
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver)
}

fn reader() -> Session {
    Session::human("human:t".into(), Role::Reader)
}

fn agent(run: &str) -> Session {
    Session::agent("agent:a".into(), Some(run.into()), Role::Writer).unwrap()
}

fn write(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    ops: serde_json::Value,
    message: &str,
) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: message.into(),
        },
    )
    .unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// A project with a folder, a doc, a section, a prose block holding `SECRET` and a term, all in
/// view `work`.
struct P {
    dir: tempfile::TempDir,
    r: Rdy,
    section: BlockId,
    prose: BlockId,
    term: BlockId,
    change: Hash,
}

fn base() -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        &writer(),
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}
        ]),
        "",
    );
    P {
        section: id(&w.created["$s"]),
        prose: id(&w.created["$p"]),
        term: id(&w.created["$t"]),
        change: h(&w.change),
        dir,
        r,
    }
}

fn version(r: &Rdy, view: &str, b: &BlockId) -> Hash {
    r.read()
        .block(&Scope::View(view.into()), b)
        .unwrap()
        .version_hash()
}

fn input(targets: &[String], dry_run: bool) -> RedactInput {
    RedactInput {
        targets: targets.to_vec(),
        reason_class: "secret".into(),
        reason: "  a leaked code  ".into(),
        dry_run,
    }
}

fn dry(r: &Rdy, targets: &[String]) -> RedactionReport {
    r.read().redaction_report(&input(targets, true)).unwrap()
}

fn row(
    kind: ImpactKind,
    record: impl ToString,
    relation: ImpactRelation,
    erasable: bool,
) -> ImpactRow {
    ImpactRow {
        kind,
        record: record.to_string(),
        relation,
        erasable,
    }
}

fn quote(kind: ImpactKind, record: impl ToString, text: &str, erasable: bool) -> QuoteRow {
    QuoteRow {
        kind,
        record: record.to_string(),
        quote: text.into(),
        erasable,
    }
}

fn store_dir(root: &Path) -> PathBuf {
    root.join(".rdy/store")
}

fn prov(actor: &str) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role: Role::Writer,
        at_ms: 7,
    }
}

/// Every frame from `from` to the log end: kind, hash, payload (None when dropped).
fn frames_from(r: &Rdy, from: Location) -> Vec<(u8, Hash, Option<Vec<u8>>)> {
    let mut out = vec![];
    r.read()
        .reader()
        .for_each_record_from(from, r.store().log_end(), |_, f, _| {
            out.push(match f {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => (kind, hash, Some(payload.to_vec())),
                StoredFrame::Dropped { kind, hash, .. } => (kind, hash, None),
            });
            Ok(())
        })
        .unwrap();
    out
}

/// Every file under `dir`, with its bytes (a link, with its target).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.file_type().is_symlink() {
                // a link is recorded by its target, never followed
                let target = std::fs::read_link(&p).unwrap();
                out.insert(p, target.to_string_lossy().as_bytes().to_vec());
            } else if meta.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Closes the project, applies `targets` through the store fixture (a local human approval),
/// and reopens it.
fn apply_fixture(p: P, targets: Vec<RedactTarget>) -> P {
    let P {
        dir,
        r,
        section,
        prose,
        term,
        change,
    } = p;
    r.close().unwrap();
    authorized_batch_fixture(
        &store_dir(dir.path()),
        &[RequestSpec::new("work", targets)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let r = Rdy::open(dir.path()).unwrap();
    P {
        dir,
        r,
        section,
        prose,
        term,
        change,
    }
}

#[test]
fn rdx5_quarantine_listing_reuses_both_existing_name_rules() {
    let dir = tempfile::tempdir().unwrap();
    // missing directories contribute nothing
    assert!(quarantine::names(dir.path()).is_empty());
    let store = store_dir(dir.path());
    std::fs::create_dir_all(&store).unwrap();
    assert!(quarantine::names(dir.path()).is_empty());
    std::fs::create_dir_all(store.join("segments")).unwrap();
    std::fs::write(store.join("segments/seg-000000.log.torn-1"), b"x").unwrap();
    std::fs::create_dir(store.join("segments/seg-000002.log.torn-3")).unwrap();
    std::fs::write(store.join("segments/seg-000003.log"), b"x").unwrap();
    std::fs::write(store.join("segments/index.redb.damaged-9"), b"x").unwrap();
    std::fs::write(store.join("index.redb.damaged-1"), b"x").unwrap();
    std::fs::create_dir(store.join("index.redb.damaged-2")).unwrap();
    std::fs::write(store.join("index.redb"), b"x").unwrap();
    std::fs::write(store.join("x.torn-1"), b"x").unwrap();
    let mut expected = vec![
        "index.redb.damaged-1",
        "index.redb.damaged-2",
        "segments/seg-000000.log.torn-1",
        "segments/seg-000002.log.torn-3",
    ];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            "/nonexistent/rdx5",
            store.join("segments/seg-000001.log.torn-2"),
        )
        .unwrap();
        std::os::unix::fs::symlink("/nonexistent/rdx5", store.join("index.redb.damaged-3"))
            .unwrap();
        expected.extend(["index.redb.damaged-3", "segments/seg-000001.log.torn-2"]);
        expected.sort();
    }
    let before = tree(&store);
    assert_eq!(quarantine::names(dir.path()), expected);
    // listing reads directory entries only: nothing is removed or rewritten
    assert_eq!(tree(&store), before);
    assert!(store.join("segments/seg-000002.log.torn-3").is_dir());
    assert!(store.join("index.redb.damaged-2").is_dir());
}

#[test]
fn rdx5_outside_kinds_are_fixed_and_unknown() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let six = vec![
        CopyKind::Render,
        CopyKind::Export,
        CopyKind::Dump,
        CopyKind::Clone,
        CopyKind::Published,
        CopyKind::WorkingCopy,
    ]
    .into_iter()
    .map(|kind| CopyRow {
        kind,
        status: CopyStatus::Unknown,
    })
    .collect::<Vec<_>>();
    assert_eq!(outside_rows(), six);
    let t = vec![p.prose.to_string()];
    let first = dry(&p.r, &t);
    assert_eq!(
        first.outside, six,
        "exactly six rows, every location unknown"
    );
    let before = tree(&p.dir.path().join(".rdy"));
    // completed outputs: a dump and an export
    let out = tempfile::tempdir().unwrap();
    p.r.read().dump(&out.path().join("p.rdydump")).unwrap();
    p.r.read()
        .export(&Scope::View("work".into()), &out.path().join("export"))
        .unwrap();
    let after = dry(&p.r, &t);
    assert_eq!(after.outside, six);
    assert_eq!(
        after.report_hash, first.report_hash,
        "outputs never change the digest"
    );
    assert_eq!(after, first);
    // no output path list, hook or other file appears in the project
    assert_eq!(tree(&p.dir.path().join(".rdy")), before);
    // recording keeps the rows; the stored manifest carries them, and no path
    let res = p.r.redact(&writer(), input(&t, false)).unwrap();
    assert_eq!(res.report.outside, six);
    let req =
        p.r.read()
            .reader()
            .redaction_request(&res.redaction.unwrap())
            .unwrap();
    assert_eq!(req.manifest.outside, six);
    let text = serde_json::to_string(&req).unwrap();
    assert!(!text.contains(&out.path().display().to_string()), "{text}");
}

#[test]
fn rdx5_public_wrappers_use_the_one_report_source() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    std::fs::write(
        store_dir(p.dir.path()).join("segments/seg-000000.log.torn-77"),
        b"tail",
    )
    .unwrap();
    let names = quarantine::names(p.dir.path());
    assert_eq!(names, vec!["segments/seg-000000.log.torn-77".to_string()]);
    let t = vec![p.prose.to_string()];
    // the dry run: a reader may run it, and it names the current quarantine and the fixed rows
    let d = p.r.redact(&reader(), input(&t, true)).unwrap();
    assert!(d.dry_run);
    assert_eq!((d.change, d.redaction), (None, None));
    assert_eq!(d.report.quarantine, names);
    assert_eq!(d.report.outside, outside_rows());
    assert!(
        d.warnings
            .iter()
            .any(|w| w.code == "W_REDACTION_QUARANTINE")
    );
    assert_eq!(d.warnings, d.report.warnings);
    let st = p.r.read();
    assert_eq!(st.redaction_report(&input(&t, true)).unwrap(), d.report);
    assert_eq!(
        compute_report(&st, &input(&t, true), ReportCopies { quarantine: &names }).unwrap(),
        d.report
    );
    assert_eq!(dry_run(&st, &input(&t, true)).unwrap(), d);
    drop(st);
    // recording uses the same source: the same report, and its manifest holds the names
    let rec = p.r.redact(&writer(), input(&t, false)).unwrap();
    assert!(!rec.dry_run);
    assert_eq!(rec.report, d.report);
    let req =
        p.r.read()
            .reader()
            .redaction_request(&rec.redaction.unwrap())
            .unwrap();
    assert_eq!(req.manifest.quarantine, names);
    // preparation with explicit names records those names
    let explicit = vec!["segments/explicit.torn-9".to_string()];
    let term = vec![p.term.to_string()];
    let rep = compute_report(
        &p.r.read(),
        &input(&term, false),
        ReportCopies {
            quarantine: &explicit,
        },
    )
    .unwrap();
    assert_eq!(rep.quarantine, explicit);
    // a report whose digest no longer matches is stale
    let mut stale = rep.clone();
    stale.report_hash = Hash([9; 32]);
    let e = prepare_request(&mut p.r, &writer(), input(&term, false), stale).unwrap_err();
    assert_eq!(e.code, E_REDACTION_STALE, "{e}");
    let res = prepare_request(&mut p.r, &writer(), input(&term, false), rep.clone()).unwrap();
    p.r.seal_open().unwrap();
    p.r.invalidate();
    let req =
        p.r.read()
            .reader()
            .redaction_request(&res.redaction.unwrap())
            .unwrap();
    assert_eq!(req.manifest.quarantine, explicit);
    assert_eq!(res.report, rep);
}

/// A review, a tension and an answer that each quote `SECRET`, and reference the prose block.
fn issue_records(p: &mut P) -> (Hash, Hash, Hash) {
    let review = Review {
        subject: ReviewSubject::Mention {
            block: p.prose.clone(),
            entity: p.term.clone(),
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: "fine".into(),
        citations: vec![Citation {
            block: p.prose.clone(),
            note: format!("Quoted: {SECRET}"),
        }],
        provenance: prov("agent:reviewer"),
    };
    let rh = p.r.store_mut().put(kinds::REVIEW, &review).unwrap();
    let tension = SemanticTension {
        key: Hash([3; 32]),
        items: vec![PairItem {
            block: p.term.clone(),
            content: Hash([4; 32]),
        }],
        review: rh,
        advice: Some(SECRET.into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: prov("agent:reviewer"),
    };
    let th = p.r.store_mut().put(kinds::TENSION, &tension).unwrap();
    let answer = Answer {
        item: "tag:v9.0.0".into(),
        act: AnswerAct::Later,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: prov("human:t"),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: Some(SECRET.into()),
    };
    let ah = p.r.store_mut().put(kinds::ANSWER, &answer).unwrap();
    p.r.invalidate();
    (rh, th, ah)
}

#[test]
fn rdx5_quotes_are_reported_but_never_persisted() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let (rh, th, ah) = issue_records(&mut p);
    let t = vec![p.prose.to_string()];
    let d = p.r.redact(&writer(), input(&t, true)).unwrap();
    assert_eq!(
        d.report.quotes,
        vec![
            quote(ImpactKind::Review, rh, SECRET, false),
            quote(ImpactKind::Tension, th, SECRET, false),
            quote(ImpactKind::Answer, ah, SECRET, false),
        ]
    );
    let issue_rows: Vec<&ImpactRow> = d
        .report
        .inside
        .iter()
        .filter(|r| {
            matches!(
                r.kind,
                ImpactKind::Review | ImpactKind::Tension | ImpactKind::Answer
            )
        })
        .collect();
    assert_eq!(
        issue_rows,
        vec![
            &row(ImpactKind::Review, rh, ImpactRelation::References, false),
            &row(ImpactKind::Review, rh, ImpactRelation::Quotes, false),
            &row(ImpactKind::Tension, th, ImpactRelation::References, false),
            &row(ImpactKind::Tension, th, ImpactRelation::Quotes, false),
            &row(ImpactKind::Answer, ah, ImpactRelation::Quotes, false),
        ]
    );
    // JSON carries the quote rows; text names each one on its own line
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["report"]["quotes"][0]["quote"], SECRET);
    assert_eq!(v["report"]["quotes"][0]["erasable"], false);
    let text = report_text(&d, 4000).unwrap().text;
    assert!(
        text.contains(&format!("review: {rh}: \"{SECRET}\" (erasable: false).")),
        "{text}"
    );
    assert!(text.contains(&format!("answer: {ah}: \"{SECRET}\" (erasable: false).")));
    // recording writes no copy of the quoted text, and no quote field
    let start = p.r.store().log_end();
    let rec = p.r.redact(&writer(), input(&t, false)).unwrap();
    assert_eq!(rec.report.quotes, d.report.quotes);
    let batch = frames_from(&p.r, start);
    assert!(batch.iter().any(|(k, _, _)| *k == kinds::REDACTION));
    for (kind, hash, payload) in &batch {
        assert!(
            !contains(payload.as_deref().unwrap(), SECRET.as_bytes()),
            "frame {kind} {hash} of the preparation batch holds the quoted text"
        );
        assert!(!contains(payload.as_deref().unwrap(), b"ZEBRA"));
    }
    let req =
        p.r.read()
            .reader()
            .redaction_request(&rec.redaction.unwrap())
            .unwrap();
    let json = serde_json::to_string(&req).unwrap();
    assert!(!json.contains("quote\""), "{json}");
    assert!(!json.contains("ZEBRA"), "{json}");
    // the quoted record identities are bound by the manifest instead
    assert!(req.manifest.inside.contains(&row(
        ImpactKind::Review,
        rh,
        ImpactRelation::Quotes,
        false
    )));
    // the issue records themselves stay readable
    let r: Review = p.r.read().reader().get_typed(kinds::REVIEW, &rh).unwrap();
    assert!(r.citations[0].note.contains(SECRET));
}

fn sample_result(dry_run: bool, recorded: bool, class: ReasonClass, rows: usize) -> RedactResult {
    let hx = |n: u8| Hash([n; 32]);
    let pid = id("p-AAAAA");
    let mut inside = vec![
        row(
            ImpactKind::BlockVersion,
            format!("{pid}@{}", hx(2)),
            ImpactRelation::Removed,
            true,
        ),
        row(ImpactKind::Review, hx(5), ImpactRelation::Quotes, false),
    ];
    for i in 0..rows {
        inside.push(row(
            ImpactKind::View,
            format!("view-{i:05}"),
            ImpactRelation::References,
            false,
        ));
    }
    let quarantine = vec!["segments/x.torn-1".to_string()];
    let report = RedactionReport {
        report_hash: hx(1),
        targets: vec![
            RedactTarget::AllVersions(pid.clone()),
            RedactTarget::Version {
                id: id("q-BBBBB"),
                version: hx(6),
            },
            RedactTarget::Blob(hx(3)),
        ],
        already_redacted: if recorded || dry_run {
            vec![]
        } else {
            vec![
                RedactTarget::AllVersions(pid.clone()),
                RedactTarget::Version {
                    id: id("q-BBBBB"),
                    version: hx(6),
                },
                RedactTarget::Blob(hx(3)),
            ]
        },
        versions: vec![(pid, hx(2))],
        blobs: vec![hx(3)],
        shared_blobs: vec![hx(4)],
        drop_frames: 3,
        drop_bytes: 120,
        inside,
        quotes: vec![quote(ImpactKind::Review, hx(5), "a \"b\"\nc", false)],
        outside: outside_rows(),
        quarantine: quarantine.clone(),
        assessment: AssessmentSupport::NotImplemented,
        warnings: warnings(class, true),
    };
    RedactResult {
        dry_run,
        reason_class: class,
        reason: "line one\nline two".into(),
        change: recorded.then_some(hx(7)),
        redaction: recorded.then_some(hx(8)),
        warnings: report.warnings.clone(),
        report,
    }
}

#[test]
fn rdx5_report_presentation_has_one_exact_contract() {
    let hx = |n: u8| Hash([n; 32]);
    let r = sample_result(true, false, ReasonClass::Secret, 0);
    let expected = [
        "Redaction dry run".to_string(),
        "Target: p-AAAAA".into(),
        format!("Target: q-BBBBB@{}", hx(6)),
        format!("Target: blob:{}", hx(3)),
        "Versions: 1; blobs: 1; shared blobs: 1.".into(),
        "Payloads: 3; bytes: 120.".into(),
        format!("block_version: p-AAAAA@{} (removed)", hx(2)),
        format!("review: {} (quotes)", hx(5)),
        format!("review: {}: \"a \\\"b\\\"\\nc\" (erasable: false).", hx(5)),
        "render: location unknown.".into(),
        "export: location unknown.".into(),
        "dump: location unknown.".into(),
        "clone: location unknown.".into(),
        "published: location unknown.".into(),
        "working_copy: location unknown.".into(),
        "Quarantine: \"segments/x.torn-1\" (possible copy; not erased).".into(),
        "Recorded assessments: not implemented.".into(),
        "Reason (secret): \"line one\\nline two\"".into(),
        "W_REDACTION_COPIES: This removes bytes from this store. It cannot erase exports, renders, dumps, clones, published files, working copies, backups, or filesystem snapshots.".into(),
        "W_REDACTION_INVENTORY: Dumps, clones, exports, renders, and published versions may hold copies. Their locations are unknown to rdy.".into(),
        "W_REDACTION_QUOTES: Quote matches use complete source strings. Partial quotes may still hold content. Snapshot issue lists and summaries, review citations, tension advice, answer notes and change-plan versions are not erased.".into(),
        "W_REDACTION_REUSE: This store will refuse these object and chunk hashes on later writes. This can block an identical restore or a file that shares a removed chunk.".into(),
        "W_REDACTION_QUARANTINE: Quarantine files may hold the content. Redaction does not remove them. A person can run rdy fsck --clean-quarantine.".into(),
        "W_REDACTION_SECRET: Rotate or revoke the secret. Redaction does not undo its exposure.".into(),
        "No request was recorded.".into(),
    ]
    .join("\n");
    let t = report_text(&r, 4000).unwrap();
    assert_eq!(t.text, expected);
    assert!(!t.truncated);
    // a recorded request and an all-removed request end with their own result lines
    let rec = report_text(&sample_result(false, true, ReasonClass::Secret, 0), 4000)
        .unwrap()
        .text;
    assert!(rec.starts_with("Redaction request\n"), "{rec}");
    assert!(rec.ends_with(&format!(
        "Recorded redaction {} in change {}.\n\nPropose the view to ask for approval.",
        hx(8),
        hx(7)
    )));
    let noop = report_text(&sample_result(false, false, ReasonClass::Secret, 0), 4000)
        .unwrap()
        .text;
    assert!(noop.starts_with("Redaction request\n"));
    assert!(noop.ends_with("\nThe selected content was already removed. No request was recorded."));
    // every reason class: its wire name, and the secret warning only for secret
    for (class, name) in [
        (ReasonClass::Secret, "secret"),
        (ReasonClass::Personal, "personal"),
        (ReasonClass::Legal, "legal"),
        (ReasonClass::Retraction, "retraction"),
    ] {
        let text = report_text(&sample_result(true, false, class, 0), 4000)
            .unwrap()
            .text;
        assert!(text.contains(&format!("\nReason ({name}): \"line one\\nline two\"\n")));
        assert_eq!(
            text.contains("W_REDACTION_SECRET"),
            class == ReasonClass::Secret,
            "{name}"
        );
    }
    // budget boundaries: a token is 4 bytes, rounded down
    let big = sample_result(true, false, ReasonClass::Legal, 400);
    let full = report_text(&big, 20_000).unwrap();
    assert!(!full.truncated);
    let need = full.text.len() / 4;
    assert!(need > 256, "{need}");
    assert_eq!(report_text(&big, need).unwrap(), full);
    let short = report_text(&big, need - 1).unwrap();
    assert!(short.truncated);
    assert!(
        short
            .text
            .contains("\nThe row list was omitted. Increase max_tokens to read it.\n")
    );
    assert!(!short.text.contains("view-00000"));
    assert!(
        short
            .text
            .contains("Versions: 1; blobs: 1; shared blobs: 1.")
    );
    assert!(short.text.contains("W_REDACTION_QUARANTINE: "));
    assert!(short.text.ends_with("No request was recorded."));
    assert!(short.text.len() / 4 < need);
    // rows within one section keep their order
    let lines: Vec<&str> = full.text.lines().collect();
    let first = lines
        .iter()
        .position(|l| l.starts_with("view: view-00000"))
        .unwrap();
    assert_eq!(lines[first + 1], "view: view-00001 (references)");
    // the retained lines alone over the budget: refused, no warning dropped
    let mut long = sample_result(true, false, ReasonClass::Secret, 0);
    long.reason = "r".repeat(2000);
    let e = report_text(&long, 256).unwrap_err();
    assert_eq!(e.code, E_REDACTION_BUDGET);
    assert_eq!(
        e.message,
        "The output budget is too small for the redaction warning."
    );
    // the budget range
    for bad in [0, 255, 20_001] {
        let e = report_text(&r, bad).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(e.message, "max_tokens must be from 256 to 20000.");
        assert_eq!(e.fix, "Choose a budget in that range.");
        assert_eq!(check_budget(bad).unwrap_err().message, e.message);
    }
    for good in [256, 20_000] {
        check_budget(good).unwrap();
    }
}

#[test]
fn rdx5_existing_issue_records_are_in_the_blast_radius() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let r1 = Review {
        subject: ReviewSubject::Mention {
            block: p.prose.clone(),
            entity: p.term.clone(),
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: SECRET.into(),
        citations: vec![],
        provenance: prov("agent:r1"),
    };
    let r2 = Review {
        subject: ReviewSubject::Mention {
            block: p.term.clone(),
            entity: p.section.clone(),
        },
        case_hash: None,
        verdict: Verdict::Restore,
        reason: "ok".into(),
        citations: vec![Citation {
            block: p.prose.clone(),
            note: SECRET.into(),
        }],
        provenance: prov("agent:r2"),
    };
    let r3 = Review {
        subject: ReviewSubject::Tension {
            id: Hash([1; 32]),
            vkey: Hash([2; 32]),
            kind: "drift".into(),
            severity: "warn".into(),
            sides: vec![SECRET.into()],
            blocks: vec![p.prose.clone()],
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: "ok".into(),
        citations: vec![],
        provenance: prov("agent:r3"),
    };
    let r1h = p.r.store_mut().put(kinds::REVIEW, &r1).unwrap();
    let r2h = p.r.store_mut().put(kinds::REVIEW, &r2).unwrap();
    let r3h = p.r.store_mut().put(kinds::REVIEW, &r3).unwrap();
    let t1 = SemanticTension {
        key: Hash([3; 32]),
        items: vec![PairItem {
            block: p.prose.clone(),
            content: Hash([4; 32]),
        }],
        review: Hash([5; 32]),
        advice: Some(SECRET.into()),
        blocking: false,
        relay: Some(RelayEvidence {
            quoted: format!("they said {SECRET}"),
            session_ref: "s1".into(),
            question: None,
        }),
        relayed_by: Some(ActorRun {
            actor: "agent:x".into(),
            run: None,
        }),
        prev: None,
        provenance: prov("agent:t1"),
    };
    let t1h = p.r.store_mut().put(kinds::TENSION, &t1).unwrap();
    // a later version reaches the block only through its prev version
    let t2 = SemanticTension {
        items: vec![PairItem {
            block: p.term.clone(),
            content: Hash([6; 32]),
        }],
        advice: None,
        relay: None,
        relayed_by: None,
        prev: Some(t1h),
        blocking: true,
        ..t1.clone()
    };
    let t2h = p.r.store_mut().put(kinds::TENSION, &t2).unwrap();
    let a1 = Answer {
        item: "tag:v9.0.0".into(),
        act: AnswerAct::Tag,
        option: Some("yes".into()),
        label: Some(SECRET.into()),
        options: vec![SECRET.into(), "no".into()],
        card_hash: None,
        targets: vec![vp],
        person: prov("human:t"),
        relayed_by: None,
        relay_evidence: Some(RelayEvidence {
            quoted: SECRET.into(),
            session_ref: "s2".into(),
            question: Some("q".into()),
        }),
        evidence_kind: None,
        note: Some(SECRET.into()),
    };
    let a1h = p.r.store_mut().put(kinds::ANSWER, &a1).unwrap();
    p.r.invalidate();
    let d = dry(&p.r, &[p.prose.to_string()]);
    let issue_rows: Vec<ImpactRow> =
        d.report_rows_of(&[ImpactKind::Review, ImpactKind::Tension, ImpactKind::Answer]);
    let mut expected = vec![];
    let mut reviews = vec![r1h, r2h, r3h];
    reviews.sort_by_key(|h| h.to_string());
    for r in &reviews {
        expected.push(row(
            ImpactKind::Review,
            r,
            ImpactRelation::References,
            false,
        ));
        expected.push(row(ImpactKind::Review, r, ImpactRelation::Quotes, false));
    }
    let mut tensions = vec![(t1h, true), (t2h, false)];
    tensions.sort_by_key(|(h, _)| h.to_string());
    for (t, quotes) in &tensions {
        expected.push(row(
            ImpactKind::Tension,
            t,
            ImpactRelation::References,
            false,
        ));
        if *quotes {
            expected.push(row(ImpactKind::Tension, t, ImpactRelation::Quotes, false));
        }
    }
    expected.push(row(
        ImpactKind::Answer,
        a1h,
        ImpactRelation::References,
        false,
    ));
    expected.push(row(ImpactKind::Answer, a1h, ImpactRelation::Quotes, false));
    assert_eq!(issue_rows, expected);
    let mut quotes: Vec<QuoteRow> = reviews
        .iter()
        .map(|r| quote(ImpactKind::Review, r, SECRET, false))
        .collect();
    quotes.push(quote(ImpactKind::Tension, t1h, SECRET, false));
    quotes.push(quote(ImpactKind::Answer, a1h, SECRET, false));
    assert_eq!(d.quotes, quotes);
    assert!(
        d.inside
            .iter()
            .chain(issue_rows.iter())
            .all(|r| !r.record.contains("ZEBRA"))
    );
}

/// The rows of `kinds` in a report, in report order.
trait RowsOf {
    fn report_rows_of(&self, kinds: &[ImpactKind]) -> Vec<ImpactRow>;
}

impl RowsOf for RedactionReport {
    fn report_rows_of(&self, kinds: &[ImpactKind]) -> Vec<ImpactRow> {
        self.inside
            .iter()
            .filter(|r| kinds.contains(&r.kind))
            .cloned()
            .collect()
    }
}

#[test]
fn rdx5_all_current_kinds_have_an_explicit_text_policy() {
    let _clock = pin_clock(1_000_000);
    // the registry equals the closed inventory: 1 to 11 and 13 to 24; 12 is unassigned
    let expected: Vec<u8> = (1..=11).chain(13..=24).collect();
    assert_eq!(ASSIGNED_KINDS.to_vec(), expected);
    assert_eq!(
        [
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::ANSWER,
            kinds::SIGNATURE,
            kinds::VIEW_OP,
            kinds::BATCH_COMMIT,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
            kinds::REDACTION,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_APPLY,
            kinds::REDACTION_CARRIED,
            kinds::PLAN,
        ],
        ASSIGNED_KINDS
    );
    for k in ASSIGNED_KINDS {
        let want = match k {
            kinds::SNAPSHOT
            | kinds::BLOB_CHUNK
            | kinds::BLOB_MANIFEST
            | kinds::BATCH_COMMIT
            | kinds::REDACTION_APPLY => TextPolicy::NoText,
            kinds::REF_UPDATE => TextPolicy::NameOnly,
            kinds::REDACTION_RESIDUE => TextPolicy::OriginalIdentity,
            _ => TextPolicy::Strings,
        };
        assert_eq!(text_policy(k), Some(want), "kind {k}");
    }
    assert_eq!(text_policy(kinds::SIGNATURE), Some(TextPolicy::Strings));
    // the carried removal: its approver's and selectors' text (it holds no reason text)
    assert_eq!(kinds::REDACTION_CARRIED, 23);
    assert_eq!(text_policy(23), Some(TextPolicy::Strings));
    // the change plan: every string of the decoded version
    assert_eq!(kinds::PLAN, 24);
    assert_eq!(text_policy(24), Some(TextPolicy::Strings));
    for k in [0, 12, 25, 255] {
        assert_eq!(text_policy(k), None, "kind {k}");
    }
    // a literal quote case for each retained text type
    const TOKEN: &str = "QQ7tok";
    let mut p = base();
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "create", "type": "term", "id": "$k", "parent": p.section.to_string(), "fields": {"name": TOKEN}, "body": "A token term."}]),
        "",
    );
    let k = id(&w.created["$k"]);
    // a proposal title, a landing by an approver whose actor holds the token, and a tag message
    let token_approver = Session::human(format!("human:{TOKEN}"), Role::Approver);
    let pi =
        p.r.propose(&writer(), "work", &format!("{TOKEN} proposal"), None)
            .unwrap();
    let landing = p.r.land(&token_approver, &pi.id).unwrap();
    p.r.tag(&approver(), "v1.0.0", None, Some(format!("tag {TOKEN}")))
        .unwrap();
    // a view whose name holds it: its ref update, view op and change
    write(
        &mut p.r,
        &writer(),
        TOKEN,
        json!([{"op": "create", "type": "prose", "parent": p.section.to_string(), "body": "elsewhere"}]),
        "",
    );
    // an intent and an alternative set
    let intent = Intent {
        stable: None,
        prev: None,
        actor: "agent:a".into(),
        run: None,
        view: "work".into(),
        area: None,
        blocks: vec![],
        angle: format!("angle {TOKEN}"),
        expires_at_ms: 9,
        status: IntentStatus::Withdrawn,
        at_ms: 8,
    };
    let ih = p.r.store_mut().put(kinds::INTENT, &intent).unwrap();
    let set = AltSet {
        stable: None,
        prev: None,
        title: format!("set {TOKEN}"),
        question: None,
        members: vec![],
        status: AltStatus::Abandoned {
            reason: "none".into(),
        },
        provenance: prov("agent:a"),
    };
    let sh = p.r.store_mut().put(kinds::ALT_SET, &set).unwrap();
    // a signature of the landing whose signer holds it
    let gh =
        p.r.store_mut()
            .put_signature(&Signature {
                subject: h(&landing),
                namespace: "rdy-approval-v1".into(),
                statement: b"a statement".to_vec(),
                sig: vec![1, 2, 3],
                signed_by: Provenance {
                    actor: format!("agent:{TOKEN}"),
                    run: None,
                    role: Role::Writer,
                    at_ms: 9,
                },
            })
            .unwrap();
    p.r.invalidate();
    // a redaction request whose reason holds it
    let mut other = input(&[p.term.to_string()], false);
    other.reason = format!("reason {TOKEN}");
    let req = p.r.redact(&writer(), other).unwrap().redaction.unwrap();
    let d = dry(&p.r, &[k.to_string()]);
    let quoted: BTreeSet<(u8, String)> = d
        .quotes
        .iter()
        .inspect(|q| assert_eq!(q.quote, TOKEN))
        .map(|q| (q.kind as u8, q.record.clone()))
        .collect();
    let has = |kind: ImpactKind, record: &str| quoted.contains(&(kind as u8, record.to_string()));
    assert!(has(ImpactKind::Landing, &landing), "{quoted:?}");
    assert!(has(ImpactKind::Tag, "v1.0.0"), "{quoted:?}");
    assert!(has(ImpactKind::Proposal, &pi.id), "{quoted:?}");
    assert!(has(ImpactKind::AuditText, &ih.to_string()), "{quoted:?}");
    assert!(has(ImpactKind::AuditText, &sh.to_string()), "{quoted:?}");
    assert!(has(ImpactKind::AuditText, &req.to_string()), "{quoted:?}");
    assert!(has(ImpactKind::AuditText, &gh.to_string()), "{quoted:?}");
    // the view's ref update and view op are audit text; its change quotes through its view
    let st = p.r.read();
    let mut ref_or_op = 0;
    for (kind, record) in &quoted {
        if *kind != ImpactKind::AuditText as u8 {
            continue;
        }
        let hh: Hash = record.parse().unwrap();
        if st.reader().has_kind(kinds::REF_UPDATE, &hh).unwrap()
            || st.reader().has_kind(kinds::VIEW_OP, &hh).unwrap()
        {
            ref_or_op += 1;
        }
    }
    assert!(ref_or_op >= 2, "{quoted:?}");
    assert!(
        quoted.iter().any(|(k, _)| *k == ImpactKind::Change as u8),
        "{quoted:?}"
    );
    // the audit records are never erasable
    assert!(
        d.quotes
            .iter()
            .all(|q| !q.erasable || q.kind == ImpactKind::BlockVersion)
    );
}

#[test]
fn rdx5_quarantine_names_are_reported_without_erasure() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let store = store_dir(p.dir.path());
    std::fs::write(store.join("segments/seg-000000.log.torn-5"), b"torn bytes").unwrap();
    std::fs::write(store.join("index.redb.damaged-5"), b"damaged bytes").unwrap();
    std::fs::create_dir(store.join("index.redb.damaged-6")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // unreadable and dangling: reading either for content would fail
        std::fs::set_permissions(
            store.join("index.redb.damaged-5"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "/nonexistent/rdx5",
            store.join("segments/seg-000001.log.torn-6"),
        )
        .unwrap();
    }
    let names = quarantine::names(p.dir.path());
    assert!(names.len() >= 3, "{names:?}");
    let t = vec![p.prose.to_string()];
    let d = dry(&p.r, &t);
    assert_eq!(d.quarantine, names);
    let w = d
        .warnings
        .iter()
        .find(|w| w.code == "W_REDACTION_QUARANTINE")
        .unwrap();
    assert_eq!(
        w.message,
        "Quarantine files may hold the content. Redaction does not remove them. A person can run rdy fsck --clean-quarantine."
    );
    assert_eq!(w.fix, "");
    // the warning comes after reuse and before secret
    let codes: Vec<&str> = d.warnings.iter().map(|w| w.code.as_str()).collect();
    assert_eq!(
        codes,
        [
            "W_REDACTION_COPIES",
            "W_REDACTION_INVENTORY",
            "W_REDACTION_QUOTES",
            "W_REDACTION_REUSE",
            "W_REDACTION_QUARANTINE",
            "W_REDACTION_SECRET"
        ]
    );
    let text = report_text(&p.r.redact(&writer(), input(&t, true)).unwrap(), 4000)
        .unwrap()
        .text;
    for n in &names {
        assert!(text.contains(&format!("Quarantine: \"{n}\" (possible copy; not erased).")));
    }
    // recording erases none of them
    p.r.redact(&writer(), input(&t, false)).unwrap();
    for n in &names {
        assert!(
            std::fs::symlink_metadata(store.join(n)).is_ok(),
            "{n} is gone"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            store.join("index.redb.damaged-5"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    assert_eq!(
        std::fs::read(store.join("index.redb.damaged-5")).unwrap(),
        b"damaged bytes"
    );
    assert_eq!(
        std::fs::read(store.join("segments/seg-000000.log.torn-5")).unwrap(),
        b"torn bytes"
    );
    // an empty list has no quarantine warning
    let q = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(q.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        &writer(),
        "work",
        json!([{"op": "create", "type": "folder", "fields": {"name": "x", "title": "X"}}]),
        "",
    );
    let folder = w.created.values().next().unwrap().clone();
    let d = dry(&r, &[folder]);
    assert!(d.quarantine.is_empty());
    assert!(
        d.warnings
            .iter()
            .all(|w| w.code != "W_REDACTION_QUARANTINE")
    );
}

#[test]
fn rdx5_an_answer_target_reaches_a_block_through_a_review() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let review = Review {
        subject: ReviewSubject::Mention {
            block: p.term.clone(),
            entity: p.section.clone(),
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: "ok".into(),
        citations: vec![Citation {
            block: p.prose.clone(),
            note: "about the code".into(),
        }],
        provenance: prov("agent:r"),
    };
    let rh = p.r.store_mut().put(kinds::REVIEW, &review).unwrap();
    let answer = |item: String, targets: Vec<Hash>, note: &str| Answer {
        item,
        act: AnswerAct::Dismiss,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: prov("human:t"),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: Some(note.into()),
    };
    // through the review, twice over the same edge
    let a1 =
        p.r.store_mut()
            .put(
                kinds::ANSWER,
                &answer("tag:v1.0.0".into(), vec![rh, rh], "a1"),
            )
            .unwrap();
    // through its item: id@12-digit version prefix
    let item = format!("{}@{}", p.prose, vp.short());
    assert!(rdy_engine::answer::is_item_ref(&item));
    let a2 =
        p.r.store_mut()
            .put(kinds::ANSWER, &answer(item, vec![], "a2"))
            .unwrap();
    // neither: another block's version prefix, and an unrelated target
    let vt = version(&p.r, "work", &p.term);
    let a3 =
        p.r.store_mut()
            .put(
                kinds::ANSWER,
                &answer(
                    format!("{}@{}", p.term, vt.short()),
                    vec![Hash([7; 32])],
                    "a3",
                ),
            )
            .unwrap();
    p.r.invalidate();
    let d = dry(&p.r, &[p.prose.to_string()]);
    let answers = d.report_rows_of(&[ImpactKind::Answer]);
    let mut expected = vec![
        row(ImpactKind::Answer, a1, ImpactRelation::References, false),
        row(ImpactKind::Answer, a2, ImpactRelation::References, false),
    ];
    expected.sort_by(|a, b| a.record.cmp(&b.record));
    assert_eq!(answers, expected, "a3 {a3} is not affected");
    assert_eq!(
        d.report_rows_of(&[ImpactKind::Review]),
        vec![row(
            ImpactKind::Review,
            rh,
            ImpactRelation::References,
            false
        )]
    );
}

#[test]
fn rdx5_selectors_have_one_closed_grammar() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let blob = p.r.store_mut().put_blob(b"blob bytes").unwrap();
    let chunk = Hash::of(b"blob bytes");
    p.r.invalidate();
    let pi = p.r.propose(&writer(), "work", "seed", None).unwrap();
    let landing = p.r.land(&approver(), &pi.id).unwrap();
    let target_err = |p: &P, sel: &str| -> RdyError {
        p.r.read()
            .redaction_report(&input(&[sel.to_string()], true))
            .unwrap_err()
    };
    let refused = |p: &P, sel: &str, shown: &str, cause: &str| {
        let e = target_err(p, sel);
        assert_eq!(e.code, E_REDACTION_TARGET, "{sel}: {e}");
        assert_eq!(
            e.message,
            format!("Cannot redact {shown}: {cause}."),
            "{sel}"
        );
        assert_eq!(
            e.fix,
            "Use a block id, id@full-version-hash, or blob:full-manifest-hash."
        );
    };
    // the three valid forms
    for sel in [
        p.prose.to_string(),
        format!("{}@{vp}", p.prose),
        format!("blob:{blob}"),
    ] {
        let d = dry(&p.r, std::slice::from_ref(&sel));
        assert_eq!(d.targets.len(), 1, "{sel}");
    }
    // forbidden forms: keys, paths, globs, short hashes, bare hashes, upper case, refs, tags
    let q = |s: &str| serde_json::to_string(s).unwrap();
    for sel in [
        "design/doc".to_string(),
        "design".to_string(),
        "p-*".to_string(),
        format!("{}@{}", p.prose, vp.short()),
        vp.to_string(),
        format!("{}@{}", p.prose, vp.to_string().to_uppercase()),
        format!("blob:{}", &blob.to_string()[..63]),
        format!("blob:{}", blob.short()),
        format!("{}@", p.prose),
        String::new(),
        " ".into(),
        format!("{} ", p.prose),
        "a\nb".into(),
    ] {
        refused(&p, &sel, &q(&sel), "invalid selector");
    }
    for sel in [
        "meta/repo_id".to_string(),
        "tag:v1.0.0".to_string(),
        format!("landing:{landing}"),
        format!("proposal:{}", pi.r#ref),
        "view:work".to_string(),
    ] {
        refused(&p, &sel, &q(&sel), "metadata is not content");
    }
    // full-hash rule over stored objects
    refused(
        &p,
        &format!("{}@{vp}", p.term),
        &format!("{}@{vp}", p.term),
        "version belongs to another block",
    );
    refused(
        &p,
        &format!("{}@{}", p.prose, Hash([1; 32])),
        &format!("{}@{}", p.prose, Hash([1; 32])),
        "unknown target",
    );
    refused(&p, "p-ZZZZZ", "p-ZZZZZ", "unknown target");
    refused(
        &p,
        &format!("blob:{chunk}"),
        &format!("blob:{chunk}"),
        "target is not a blob manifest",
    );
    refused(
        &p,
        &format!("blob:{vp}"),
        &format!("blob:{vp}"),
        "target is not a blob manifest",
    );
    refused(
        &p,
        &format!("blob:{landing}"),
        &format!("blob:{landing}"),
        "metadata is not content",
    );
    refused(
        &p,
        &format!("blob:{}", Hash([2; 32])),
        &format!("blob:{}", Hash([2; 32])),
        "unknown target",
    );
    // none: an empty list
    let e = p.r.read().redaction_report(&input(&[], true)).unwrap_err();
    assert_eq!(e.code, E_REDACTION_TARGET);
    assert_eq!(e.message, "Cannot redact []: invalid selector.");
    // duplicates fold; the order is all-versions, then versions, then blobs
    let d = dry(
        &p.r,
        &[
            format!("blob:{blob}"),
            format!("{}@{vp}", p.prose),
            p.prose.to_string(),
            p.term.to_string(),
            p.prose.to_string(),
            format!("{}@{vp}", p.prose),
        ],
    );
    let mut ids = [p.prose.clone(), p.term.clone()];
    ids.sort();
    assert_eq!(
        d.targets,
        vec![
            RedactTarget::AllVersions(ids[0].clone()),
            RedactTarget::AllVersions(ids[1].clone()),
            RedactTarget::Version {
                id: p.prose.clone(),
                version: vp
            },
            RedactTarget::Blob(blob),
        ]
    );
    // the first bad selector in input order is the one named
    let e =
        p.r.read()
            .redaction_report(&input(&["p-*".into(), "q-*".into()], true))
            .unwrap_err();
    assert_eq!(e.message, "Cannot redact \"p-*\": invalid selector.");
}

#[test]
fn rdx5_all_versions_includes_closed_and_unlanded_history() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let v1 = version(&p.r, "work", &p.prose);
    let pi = p.r.propose(&writer(), "work", "seed", None).unwrap();
    p.r.land(&approver(), &pi.id).unwrap();
    // old canon v1; v2 unlanded in work
    write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "update", "id": p.prose.to_string(), "base": v1.to_string(), "body": "second"}]),
        "",
    );
    let v2 = version(&p.r, "work", &p.prose);
    // v3 in a view that is closed
    let side = agent("side");
    write(
        &mut p.r,
        &side,
        "side",
        json!([{"op": "update", "id": p.prose.to_string(), "base": v1.to_string(), "body": "third"}]),
        "",
    );
    let v3 = version(&p.r, "side", &p.prose);
    p.r.view_close(&side, &["side".into()], "done").unwrap();
    // v4 in a proposal that its view's close withdrew
    let wv = agent("wd");
    write(
        &mut p.r,
        &wv,
        "wd",
        json!([{"op": "update", "id": p.prose.to_string(), "base": v1.to_string(), "body": "fourth"}]),
        "",
    );
    let v4 = version(&p.r, "wd", &p.prose);
    let withdrawn = p.r.propose(&wv, "wd", "fourth", None).unwrap();
    p.r.view_close(&wv, &["wd".into()], "withdrawn").unwrap();
    // a duplicate frame of v1, appended raw while the store is closed
    let v1_bytes = p.r.read().reader().get_kind(kinds::BLOCK, &v1).unwrap();
    let dir = p.dir;
    p.r.close().unwrap();
    {
        let (mut log, q) = rdy_store::segment::SegmentLog::open_marked(
            &store_dir(dir.path()).join("segments"),
            64 * 1024 * 1024,
            None,
        )
        .unwrap();
        assert!(q.is_empty());
        log.append_batch(&[(kinds::BLOCK, v1_bytes)]).unwrap();
        log.sync().unwrap();
    }
    let r = Rdy::open(dir.path()).unwrap();
    let d = dry(&r, &[p.prose.to_string()]);
    let mut versions = vec![v1, v2, v3, v4];
    versions.sort();
    assert_eq!(
        d.versions,
        versions
            .iter()
            .map(|v| (p.prose.clone(), *v))
            .collect::<Vec<_>>()
    );
    // every frame of every version, the duplicate too, and every embedding change
    let reader = r.read().reader().clone();
    let mut producers = BTreeSet::new();
    for v in &versions {
        producers.extend(reader.producers(v).unwrap());
    }
    let all = frames_from(&r, ORIGIN);
    let expect = all
        .iter()
        .filter(|(k, h, _)| {
            (*k == kinds::BLOCK && versions.contains(h))
                || (*k == kinds::CHANGE && producers.contains(h))
        })
        .count();
    assert_eq!(
        all.iter()
            .filter(|(k, h, _)| *k == kinds::BLOCK && *h == v1)
            .count(),
        2
    );
    assert_eq!(d.drop_frames, expect as u64);
    assert_eq!(producers.len(), 4);
    // the closed view and the withdrawn proposal are named
    let has = |kind: ImpactKind, rec: &str| {
        d.inside
            .iter()
            .any(|r| r.kind == kind && r.record == rec && r.relation == ImpactRelation::References)
    };
    assert!(has(ImpactKind::View, "side"));
    assert!(has(ImpactKind::View, "wd"));
    assert!(has(ImpactKind::View, "work"));
    assert!(has(ImpactKind::Proposal, &withdrawn.id));
    assert!(has(ImpactKind::Proposal, &pi.id));
    // the request records, so the store's own closure check agrees
    let mut r = r;
    let res = r
        .redact(&writer(), input(&[p.prose.to_string()], false))
        .unwrap();
    assert!(res.redaction.is_some());
}

#[test]
fn rdx5_report_names_landings_tags_views_proposals_and_snapshots() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let pi = p.r.propose(&writer(), "work", "seed", None).unwrap();
    let landing = h(&p.r.land(&approver(), &pi.id).unwrap());
    p.r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let state = p.r.read().reader().landing(&landing).unwrap().state;
    let d = dry(&p.r, &[format!("{}@{vp}", p.prose)]);
    let refs = ImpactRelation::References;
    assert_eq!(
        d.inside,
        vec![
            row(
                ImpactKind::BlockVersion,
                format!("{}@{vp}", p.prose),
                ImpactRelation::Removed,
                true
            ),
            row(ImpactKind::Change, p.change, ImpactRelation::Embeds, true),
            row(ImpactKind::Landing, landing, refs, false),
            row(ImpactKind::Tag, "v1.0.0", refs, false),
            row(ImpactKind::View, "work", refs, false),
            row(ImpactKind::Proposal, &pi.id, refs, false),
            row(ImpactKind::StateSnapshot, state, refs, false),
        ]
    );
    assert!(d.quotes.is_empty());
    assert_eq!(d.versions, vec![(p.prose.clone(), vp)]);
    // a state snapshot is a reference, not a copy: only the block and change frames drop
    let all = frames_from(&p.r, ORIGIN);
    let payloads: Vec<&(u8, Hash, Option<Vec<u8>>)> = all
        .iter()
        .filter(|(k, h, _)| {
            (*k == kinds::BLOCK && *h == vp) || (*k == kinds::CHANGE && *h == p.change)
        })
        .collect();
    assert_eq!(d.drop_frames, 2);
    assert_eq!(payloads.len(), 2);
    assert_eq!(
        d.drop_bytes,
        payloads
            .iter()
            .map(|(_, _, b)| b.as_ref().unwrap().len() as u64)
            .sum::<u64>()
    );
    // a tag reaches the version through its landing's history, not only its snapshot: v1 is
    // replaced in a second landing, and a third landing of an independent change is tagged
    write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "update", "id": p.prose.to_string(), "base": vp.to_string(), "body": "second"}]),
        "",
    );
    let pi2 = p.r.propose(&writer(), "work", "second", None).unwrap();
    let l2 = h(&p.r.land(&approver(), &pi2.id).unwrap());
    write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "create", "type": "folder", "fields": {"name": "other", "title": "Other"}}]),
        "",
    );
    let pi3 = p.r.propose(&writer(), "work", "third", None).unwrap();
    let l3 = h(&p.r.land(&approver(), &pi3.id).unwrap());
    p.r.tag(&approver(), "v3.0.0", None, None).unwrap();
    let st = p.r.read();
    let rec3 = st.reader().landing(&l3).unwrap();
    assert_eq!(rec3.prev, Some(l2));
    let snap3: rdy_model::StateSnapshot =
        st.reader().get_typed(kinds::SNAPSHOT, &rec3.state).unwrap();
    assert!(
        !snap3.blocks.values().any(|v| *v == vp),
        "l3's snapshot no longer names v1"
    );
    for c in &rec3.changes {
        assert!(
            st.reader().change(c).unwrap().deps.is_empty(),
            "an independent change"
        );
    }
    drop(st);
    let d = dry(&p.r, &[format!("{}@{vp}", p.prose)]);
    assert!(
        d.inside
            .contains(&row(ImpactKind::Tag, "v3.0.0", refs, false))
    );
    assert!(
        d.inside
            .contains(&row(ImpactKind::Landing, l2, refs, false))
    );
    assert!(
        !d.inside
            .iter()
            .any(|r| r.kind == ImpactKind::Landing && r.record == l3.to_string()),
        "l3 neither names v1 nor lands a change that depends on one"
    );
}

#[test]
fn rdx5_embedded_changes_and_shared_chunks_are_mandatory() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let a = vec![b'a'; 1024 * 1024];
    let bytes = |tail: &[u8]| [a.as_slice(), tail].concat();
    let x = p.r.store_mut().put_blob(&bytes(b"bx")).unwrap();
    let y = p.r.store_mut().put_blob(&bytes(b"by")).unwrap();
    let z = p.r.store_mut().put_blob(b"unrelated").unwrap();
    p.r.invalidate();
    // a figure of the shared blob y, landed and tagged on its own first: everything it reaches
    // is in the report only because y shares a chunk with x
    let wy = write(
        &mut p.r,
        &writer(),
        "work",
        json!([
            {"op": "create", "type": "figure", "id": "$y", "parent": p.section.to_string(), "fields": {"blob": y.to_string(), "media_type": "application/octet-stream", "alt": "y"}}
        ]),
        "",
    );
    let fy = id(&wy.created["$y"]);
    let fy_change = h(&wy.change);
    let vfy = version(&p.r, "work", &fy);
    let py = p.r.propose(&writer(), "work", "figure y", None).unwrap();
    let landing_y = h(&p.r.land(&approver(), &py.id).unwrap());
    p.r.tag(&approver(), "v0.9.0", None, None).unwrap();
    let state_y = p.r.read().reader().landing(&landing_y).unwrap().state;
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([
            {"op": "create", "type": "figure", "id": "$x", "parent": p.section.to_string(), "fields": {"blob": x.to_string(), "media_type": "application/octet-stream", "alt": "x"}},
            {"op": "create", "type": "figure", "id": "$z", "parent": p.section.to_string(), "fields": {"blob": z.to_string(), "media_type": "application/octet-stream", "alt": "z"}}
        ]),
        "",
    );
    let fx = id(&w.created["$x"]);
    let fx_change = h(&w.change);
    let vfx = version(&p.r, "work", &fx);
    let pi = p.r.propose(&writer(), "work", "figures", None).unwrap();
    let landing = h(&p.r.land(&approver(), &pi.id).unwrap());
    p.r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let state = p.r.read().reader().landing(&landing).unwrap().state;
    let d = dry(&p.r, &[format!("blob:{x}")]);
    assert_eq!(d.blobs, vec![x]);
    assert_eq!(d.shared_blobs, vec![y], "y shares x's first chunk");
    assert!(d.versions.is_empty());
    assert_eq!(d.drop_frames, 2, "both chunks of x");
    assert_eq!(d.drop_bytes, 1024 * 1024 + 2);
    let refs = ImpactRelation::References;
    let mut expected = vec![
        row(ImpactKind::BlockVersion, format!("{fx}@{vfx}"), refs, false),
        row(ImpactKind::BlockVersion, format!("{fy}@{vfy}"), refs, false),
        row(ImpactKind::Change, fx_change, refs, false),
        row(ImpactKind::Change, fy_change, refs, false),
        row(ImpactKind::Landing, landing, refs, false),
        row(ImpactKind::Landing, landing_y, refs, false),
        row(ImpactKind::Tag, "v0.9.0", refs, false),
        row(ImpactKind::Tag, "v1.0.0", refs, false),
        row(ImpactKind::View, "work", refs, false),
        row(ImpactKind::Proposal, &pi.id, refs, false),
        row(ImpactKind::Proposal, &py.id, refs, false),
        row(ImpactKind::StateSnapshot, state, refs, false),
        row(ImpactKind::StateSnapshot, state_y, refs, false),
    ];
    expected.sort_by(|a, b| (a.kind as u8, &a.record).cmp(&(b.kind as u8, &b.record)));
    assert_eq!(
        d.inside, expected,
        "a blob reference, of the selected blob or of one sharing its chunk, reaches the change, \
         landing, tag, proposal and snapshot; no figure is erased"
    );
    // the store's preparation checks the closure: embedding changes and every chunk frame
    let rec =
        p.r.redact(&writer(), input(&[p.prose.to_string()], false))
            .unwrap();
    assert!(rec.redaction.is_some());
    assert!(rec.report.inside.contains(&row(
        ImpactKind::Change,
        p.change,
        ImpactRelation::Embeds,
        true
    )));
    // a blob whose shared chunk an applied request removed keeps its other chunk selectable
    let p = apply_fixture(p, vec![RedactTarget::Blob(y)]);
    let d = dry(&p.r, &[format!("blob:{x}")]);
    assert!(d.already_redacted.is_empty(), "x is only partly removed");
    assert_eq!(d.blobs, vec![x]);
    assert_eq!(d.drop_frames, 1, "only x's remaining chunk");
    assert_eq!(d.drop_bytes, 2);
    let mut p = p;
    let rec =
        p.r.redact(&writer(), input(&[format!("blob:{x}")], false))
            .unwrap();
    assert!(rec.redaction.is_some());
    // y itself is wholly removed now
    let d = dry(&p.r, &[format!("blob:{y}")]);
    assert_eq!(d.already_redacted, vec![RedactTarget::Blob(y)]);
}

#[test]
fn rdx5_literal_quotes_are_candidates_not_automatic_targets() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([
            {"op": "create", "type": "prose", "id": "$q", "parent": p.section.to_string(), "body": format!("Note: {SECRET} Keep it.")},
            {"op": "create", "type": "prose", "id": "$r", "parent": p.section.to_string(), "body": "Only ZEBRA-7731 here."}
        ]),
        &format!("copied {SECRET}"),
    );
    let q = id(&w.created["$q"]);
    let cq = h(&w.change);
    let vq = version(&p.r, "work", &q);
    let d = dry(&p.r, &[p.prose.to_string()]);
    assert_eq!(
        d.versions,
        vec![(p.prose.clone(), vp)],
        "a quote is no target"
    );
    assert_eq!(
        d.quotes,
        vec![
            quote(ImpactKind::BlockVersion, format!("{q}@{vq}"), SECRET, true),
            quote(ImpactKind::Change, cq, SECRET, false),
        ],
        "the partial copy in r is not claimed"
    );
    assert!(d.inside.contains(&row(
        ImpactKind::BlockVersion,
        format!("{q}@{vq}"),
        ImpactRelation::Quotes,
        true
    )));
    assert!(
        d.inside
            .contains(&row(ImpactKind::Change, cq, ImpactRelation::Quotes, false))
    );
    // a quoted block stays until it is selected itself
    let d = dry(&p.r, &[format!("{q}@{vq}")]);
    assert_eq!(d.versions, vec![(q.clone(), vq)]);
    assert!(d.inside.contains(&row(
        ImpactKind::BlockVersion,
        format!("{q}@{vq}"),
        ImpactRelation::Removed,
        true
    )));
}

#[test]
fn rdx5_assessment_support_is_explicitly_not_implemented() {
    let _clock = pin_clock(1_000_000);
    let p = base();
    let d =
        p.r.read()
            .redaction_report(&input(&[p.prose.to_string()], true))
            .unwrap();
    // M3b adds the assessment kind; until then the report says so, with no count
    assert_eq!(d.assessment, AssessmentSupport::NotImplemented);
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["assessment"], "not_implemented");
    let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "report_hash",
            "targets",
            "already_redacted",
            "versions",
            "blobs",
            "shared_blobs",
            "drop_frames",
            "drop_bytes",
            "inside",
            "quotes",
            "outside",
            "quarantine",
            "assessment",
            "warnings"
        ]),
        "no assessment count or zero-row fiction"
    );
    let res = dry_run(&p.r.read(), &input(&[p.prose.to_string()], true)).unwrap();
    let text = report_text(&res, 4000).unwrap().text;
    assert!(text.contains("\nRecorded assessments: not implemented.\n"));
    // the base registry holds no assessment kind: every assigned kind has a policy, and a new
    // content-bearing kind without a visitor is refused, never read as absent
    require_text_policies(&ASSIGNED_KINDS).unwrap();
    let e = require_text_policies(&[kinds::PLAN + 1]).unwrap_err();
    assert_eq!(
        e.message,
        "the redaction report has no text policy for object kind 25"
    );
    assert!(require_text_policies(&[12]).is_err());
}

#[test]
fn rdx5_dry_run_changes_no_store_or_view_state() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let t = vec![p.prose.to_string()];
    let root = p.dir.path().join(".rdy");
    let files = tree(&root);
    let st = p.r.read();
    let refs = st.reader().refs_with_prefix("").unwrap();
    let views = st.view_names();
    let seq = st.seq();
    drop(st);
    rdy_model::seed_ids_for_tests(77);
    let d = p.r.redact(&writer(), input(&t, true)).unwrap();
    let next = BlockId::new("p");
    rdy_model::seed_ids_for_tests(77);
    let fresh = BlockId::new("p");
    rdy_model::unseed_ids_for_tests();
    assert_eq!(next, fresh, "the dry run drew no id");
    assert!(d.dry_run && d.change.is_none() && d.redaction.is_none());
    assert_eq!(tree(&root), files, "the byte tree is unchanged");
    let st = p.r.read();
    assert_eq!(st.reader().refs_with_prefix("").unwrap(), refs);
    assert_eq!(st.view_names(), views);
    assert_eq!(st.seq(), seq);
    assert_eq!(p.r.store().log_end(), seq);
    // a reader's recording is refused, and changes nothing either
    drop(st);
    let e = p.r.redact(&reader(), input(&t, false)).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
    assert_eq!(tree(&root), files);
}

#[test]
fn rdx5_request_commit_keeps_the_report_and_original_bytes() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let t = vec![p.prose.to_string()];
    let d = dry(&p.r, &t);
    let res = p.r.redact(&writer(), input(&t, false)).unwrap();
    assert!(!res.dry_run);
    assert_eq!(res.reason_class, ReasonClass::Secret);
    assert_eq!(res.reason, "a leaked code");
    assert_eq!(res.report, d);
    let (change, redaction) = (res.change.unwrap(), res.redaction.unwrap());
    let st = p.r.read();
    let reader = st.reader();
    let req = reader.redaction_request(&redaction).unwrap();
    assert_eq!(req.actor.actor, "human:t");
    assert_eq!(req.actor.run.as_deref(), Some("cli"));
    assert_eq!(req.actor.role, Role::Writer);
    assert_eq!(req.reason_class, ReasonClass::Secret);
    assert_eq!(req.reason, "a leaked code");
    assert_eq!(
        req.targets,
        vec![RedactTarget::AllVersions(p.prose.clone())]
    );
    assert_eq!(req.carrier, change);
    let mut buf = vec![0u8; rdy_model::redaction::MAX_RECORD_BYTES];
    assert_eq!(req.manifest.report_hash(&mut buf).unwrap(), d.report_hash);
    assert_eq!(req.manifest.versions, d.versions);
    assert_eq!(req.manifest.inside, d.inside);
    assert_eq!(
        reader.redaction_for_carrier(&change).unwrap(),
        Some(redaction)
    );
    let carrier = reader.change(&change).unwrap();
    assert!(carrier.ops.is_empty());
    assert_eq!(carrier.message, CARRIER_MESSAGE);
    assert_eq!(carrier.view, "work");
    assert_eq!(carrier.provenance, req.actor);
    assert!(
        st.included("work").contains(&change),
        "the carrier is in the view"
    );
    // recording advances no redaction epoch: the state stays current for responses
    assert_eq!(st.redaction_epoch(), 0);
    assert_eq!(st.redaction_epoch(), reader.redaction_epoch());
    st.ensure_current_epoch().unwrap();
    // nothing is removed: the original content still reads
    assert_eq!(reader.redaction_of(kinds::BLOCK, &vp).unwrap(), None);
    assert_eq!(
        st.block(&Scope::View("work".into()), &p.prose)
            .unwrap()
            .body,
        SECRET
    );
    assert_eq!(reader.get_block(&vp).unwrap().body, SECRET);
    // the reply serializes hashes as full hex
    let v = serde_json::to_value(&res).unwrap();
    assert_eq!(v["change"], change.to_string());
    assert_eq!(v["redaction"], redaction.to_string());
    assert_eq!(v["report"]["report_hash"], d.report_hash.to_string());
    assert_eq!(v["report"]["versions"][0][1], vp.to_string());
    assert_eq!(serde_json::from_value::<RedactResult>(v).unwrap(), res);
    // the input type takes only its own keys
    let bad = serde_json::from_value::<RedactInput>(
        json!({"targets": [], "reason_class": "secret", "reason": "x", "dryrun": true}),
    );
    assert!(bad.is_err());
    let ok: RedactInput = serde_json::from_value(
        json!({"targets": ["p-AAAAA"], "reason_class": "secret", "reason": "x"}),
    )
    .unwrap();
    assert!(!ok.dry_run);
}

#[test]
fn rdx5_limits_fail_before_writing() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let t = vec![p.prose.to_string()];
    let end = p.r.store().log_end();
    let refuse = |p: &mut P, i: RedactInput| -> RdyError {
        let e = p.r.redact(&writer(), i).unwrap_err();
        assert_eq!(p.r.store().log_end(), end, "nothing was written");
        e
    };
    let limit = |e: &RdyError, l: &str| {
        assert_eq!(e.code, E_REDACTION_LIMIT, "{e}");
        assert_eq!(e.message, format!("The redaction exceeds {l}."));
        assert_eq!(e.fix, "Select fewer targets, then run the dry run again.");
    };
    // selectors: 256 pass the count (and fail later as unknown), 257 do not
    let many = |n: usize| -> Vec<String> { (0..n).map(|i| format!("p-{:05}", i)).collect() };
    let e = refuse(&mut p, input(&many(256), false));
    assert_eq!(e.code, E_REDACTION_TARGET, "{e}");
    limit(&refuse(&mut p, input(&many(257), false)), "256 targets");
    // one selector: 256 bytes pass the length check (and fail the syntax), 257 do not
    let e = refuse(&mut p, input(&["x".repeat(256)], false));
    assert_eq!(
        e.message,
        format!("Cannot redact \"{}\": invalid selector.", "x".repeat(256))
    );
    limit(
        &refuse(&mut p, input(&["x".repeat(257)], false)),
        "256 bytes per target",
    );
    // the reason: 1 to 4096 bytes after trimming
    for bad in ["", "   ", &"r".repeat(4097)] {
        let mut i = input(&t, false);
        i.reason = bad.into();
        let e = refuse(&mut p, i);
        assert_eq!(e.code, E_REDACTION_REASON);
        assert_eq!(
            e.message,
            "The redaction needs a reason and a reason class."
        );
        assert_eq!(
            e.fix,
            "Use secret, personal, legal, or retraction, with a reason of 1 to 4096 UTF-8 bytes."
        );
    }
    let mut i = input(&t, true);
    i.reason = format!(" {} ", "r".repeat(4096));
    p.r.redact(&writer(), i).unwrap();
    for bad in ["Secret", "", "other"] {
        let mut i = input(&t, false);
        i.reason_class = bad.into();
        assert_eq!(refuse(&mut p, i).code, E_REDACTION_REASON);
    }
    // the store-wide bounds, lowered for the test: each boundary passes, one past it refuses
    let st = p.r.read();
    let all = frames_from(&p.r, ORIGIN);
    let frames = all.len() as u64;
    let bytes: u64 = all
        .iter()
        .map(|(_, _, b)| 37 + b.as_ref().unwrap().len() as u64)
        .sum();
    drop(st);
    let full = dry(&p.r, &t);
    let rows = full.inside.len() + full.quotes.len() + full.outside.len() + full.quarantine.len();
    let mut buf = vec![0u8; rdy_model::redaction::MAX_RECORD_BYTES];
    let record = encode_into(&full, &mut buf).unwrap().len();
    let check = |p: &mut P, lim: Limits, text: Option<&str>| {
        let _g = override_limits_for_tests(lim);
        match text {
            None => {
                dry(&p.r, &t);
            }
            Some(l) => limit(&refuse(p, input(&t, false)), l),
        }
    };
    let d = Limits::default();
    check(&mut p, Limits { frames, ..d }, None);
    check(
        &mut p,
        Limits {
            frames: frames - 1,
            ..d
        },
        Some("1000000 scanned frames"),
    );
    check(&mut p, Limits { bytes, ..d }, None);
    check(
        &mut p,
        Limits {
            bytes: bytes - 1,
            ..d
        },
        Some("2147483648 scanned bytes"),
    );
    check(&mut p, Limits { selected: 1, ..d }, None);
    check(
        &mut p,
        Limits { selected: 0, ..d },
        Some("100000 selected objects"),
    );
    check(&mut p, Limits { rows, ..d }, None);
    check(
        &mut p,
        Limits {
            rows: rows - 1,
            ..d
        },
        Some("100000 report rows"),
    );
    check(&mut p, Limits { record, ..d }, None);
    check(
        &mut p,
        Limits {
            record: record - 1,
            ..d
        },
        Some("16777216 bytes per record"),
    );
    check(&mut p, Limits { sources: 1, ..d }, None);
    check(
        &mut p,
        Limits { sources: 0, ..d },
        Some("4096 source strings"),
    );
    // strings: the bound applies to the source strings only (the selected body); the store's
    // other strings are searched at any length
    check(
        &mut p,
        Limits {
            string: SECRET.len(),
            ..d
        },
        None,
    );
    check(
        &mut p,
        Limits {
            string: SECRET.len() - 1,
            ..d
        },
        Some("1048576 bytes per string"),
    );
    // nested fields: a block whose field nests 3 containers deep
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "create", "type": "table", "id": "$n", "parent": p.section.to_string(), "fields": {"columns": ["a"], "rows": [[["deep"]]]}}]),
        "",
    );
    let n = vec![w.created["$n"].clone()];
    let end2 = p.r.store().log_end();
    {
        let _g = override_limits_for_tests(Limits { levels: 3, ..d });
        p.r.read().redaction_report(&input(&n, true)).unwrap();
    }
    {
        let _g = override_limits_for_tests(Limits { levels: 2, ..d });
        let e = p.r.redact(&writer(), input(&n, false)).unwrap_err();
        limit(&e, "64 field levels");
        assert_eq!(p.r.store().log_end(), end2);
        // the bound refuses only a selected target: elsewhere the search stops at that depth
        let other = dry(&p.r, &t);
        let deep: Vec<String> = other
            .warnings
            .iter()
            .filter(|w| w.message.contains("nest deeper"))
            .map(|w| format!("{}: {}", w.code, w.message))
            .collect();
        assert_eq!(
            deep,
            [
                "W_REDACTION_QUOTES: 1 records nest deeper than 2 levels; rdy did not search below that depth"
            ]
        );
    }
}

#[test]
fn rdx5_an_already_removed_set_is_a_noop() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let prose = p.prose.clone();
    let v1 = version(&p.r, "work", &prose);
    write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "update", "id": prose.to_string(), "base": v1.to_string(), "body": "second"}]),
        "",
    );
    let v2 = version(&p.r, "work", &prose);
    let mut p = apply_fixture(
        p,
        vec![RedactTarget::Version {
            id: prose.clone(),
            version: v1,
        }],
    );
    // wholly removed: success with no change and no request; nothing written
    let end = p.r.store().log_end();
    let one = vec![format!("{prose}@{v1}")];
    let res = p.r.redact(&writer(), input(&one, false)).unwrap();
    assert!(!res.dry_run);
    assert_eq!((res.change, res.redaction), (None, None));
    assert_eq!(
        res.report.already_redacted,
        vec![RedactTarget::Version {
            id: prose.clone(),
            version: v1
        }]
    );
    assert_eq!(res.report.targets, res.report.already_redacted);
    assert!(res.report.versions.is_empty());
    assert!(res.report.quotes.is_empty(), "no source string survives");
    assert_eq!((res.report.drop_frames, res.report.drop_bytes), (0, 0));
    assert_eq!(p.r.store().log_end(), end);
    let text = report_text(&res, 4000).unwrap().text;
    assert!(text.ends_with("The selected content was already removed. No request was recorded."));
    let v = serde_json::to_value(&res).unwrap();
    assert!(v["change"].is_null() && v["redaction"].is_null());
    // mixed: every version of the block, one removed and one present
    let all = vec![prose.to_string()];
    let d = dry(&p.r, &all);
    assert!(d.already_redacted.is_empty());
    let mut both = vec![(prose.clone(), v1), (prose.clone(), v2)];
    both.sort();
    assert_eq!(d.versions, both);
    // only the present payloads drop: the removed version's frames are not erased twice
    let frames = frames_from(&p.r, ORIGIN);
    let reader = p.r.read().reader().clone();
    let producers2: BTreeSet<Hash> = reader.producers(&v2).unwrap().into_iter().collect();
    let producers1: BTreeSet<Hash> = reader.producers(&v1).unwrap().into_iter().collect();
    let present = frames
        .iter()
        .filter(|(k, h, b)| {
            b.is_some()
                && ((*k == kinds::BLOCK && (*h == v1 || *h == v2))
                    || (*k == kinds::CHANGE && (producers1.contains(h) || producers2.contains(h))))
        })
        .count();
    assert_eq!(d.drop_frames, present as u64);
    assert!(
        frames
            .iter()
            .any(|(k, h, b)| *k == kinds::BLOCK && *h == v1 && b.is_none()),
        "v1's frame is already dropped"
    );
    let res = p.r.redact(&writer(), input(&all, false)).unwrap();
    assert!(res.change.is_some() && res.redaction.is_some());
    let req =
        p.r.read()
            .reader()
            .redaction_request(&res.redaction.unwrap())
            .unwrap();
    assert!(
        req.manifest.spans.iter().all(|s| s.hash != v1),
        "no duplicate erasure"
    );
    // the mixed selector and an already-removed one together: only the first is requested
    let d = dry(&p.r, &[prose.to_string(), format!("{prose}@{v1}")]);
    assert_eq!(
        d.already_redacted,
        vec![RedactTarget::Version {
            id: prose.clone(),
            version: v1
        }]
    );
}

#[test]
fn rdx5_long_strings_and_deep_fields_elsewhere_never_block_a_report() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    // a pasted 1.1 MB body that also holds a copy of the secret, and a body at the bound
    let section = p.section.to_string();
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([
            {"op": "create", "type": "prose", "id": "$b", "parent": section, "body": format!("{}{SECRET}", "x".repeat(1_100_000))},
            {"op": "create", "type": "prose", "id": "$m", "parent": section, "body": "y".repeat(1_048_576)}
        ]),
        "",
    );
    let big = id(&w.created["$b"]);
    let vb = version(&p.r, "work", &big);
    let max = id(&w.created["$m"]);
    // the small secret block is a target, and the long body is searched for its copy
    let d = dry(&p.r, &[p.prose.to_string()]);
    assert!(
        d.quotes.contains(&quote(
            ImpactKind::BlockVersion,
            format!("{big}@{vb}"),
            SECRET,
            true
        )),
        "{:?}",
        d.quotes
    );
    assert_eq!(d.warnings, warnings(ReasonClass::Secret, false));
    // a block whose own string is at the bound is a target
    dry(&p.r, &[max.to_string()]);
    // the long block itself: its own body is over the bound
    let e =
        p.r.read()
            .redaction_report(&input(&[big.to_string()], true))
            .unwrap_err();
    assert_eq!(e.code, E_REDACTION_LIMIT, "{e}");
    assert_eq!(e.message, "The redaction exceeds 1048576 bytes per string.");
    // a field nested past the level bound elsewhere: searched down to the bound, with one
    // warning. A write keeps fields within MAX_FIELD_NESTING (29) levels (final review ruling
    // 1), so the report's bound is lowered to 20 for this thread and the field nests 21 lists
    // deep: one past it, as 65 lists were past the default 64.
    let _levels = override_limits_for_tests(Limits {
        levels: 20,
        ..Limits::default()
    });
    let mut rows = json!([SECRET]);
    for _ in 0..20 {
        rows = json!([rows]);
    }
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "create", "type": "table", "id": "$n", "parent": section, "fields": {"columns": [SECRET], "rows": rows}}]),
        "",
    );
    let deep = id(&w.created["$n"]);
    let vd = version(&p.r, "work", &deep);
    let rec =
        p.r.redact(&writer(), input(&[p.prose.to_string()], false))
            .unwrap();
    assert!(rec.redaction.is_some());
    let line = "W_REDACTION_QUOTES: 1 records nest deeper than 20 levels; rdy did not search below \
                that depth";
    let lines: Vec<String> = rec
        .warnings
        .iter()
        .map(|w| format!("{}: {}", w.code, w.message))
        .collect();
    let mut expected: Vec<String> = warnings(ReasonClass::Secret, false)
        .iter()
        .map(|w| format!("{}: {}", w.code, w.message))
        .collect();
    expected.insert(3, line.to_string());
    assert_eq!(lines, expected, "one line, after the quotes warning");
    assert!(rec.warnings.iter().all(|w| w.fix.is_empty()));
    assert!(
        rec.report.quotes.contains(&quote(
            ImpactKind::BlockVersion,
            format!("{deep}@{vd}"),
            SECRET,
            true
        )),
        "the deep record is still searched above the depth"
    );
    let text = report_text(&rec, 20_000).unwrap().text;
    assert!(text.contains(line), "{text}");
    // the deep table as a target is refused: the depth bound holds for selected targets
    let e =
        p.r.read()
            .redaction_report(&input(&[deep.to_string()], true))
            .unwrap_err();
    assert_eq!(e.message, "The redaction exceeds 64 field levels.");
}

#[test]
fn rdx5_id_shaped_strings_are_quoted_like_any_text() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    const PIN: &str = "PIN-7Q2ZZ";
    assert!(
        BlockId::parse(PIN).is_ok(),
        "the text has the shape of an id"
    );
    // a block id's JSON is its plain string, as a value and as a map key
    let pin = id(PIN);
    assert_eq!(serde_json::to_string(&pin).unwrap(), "\"PIN-7Q2ZZ\"");
    assert_eq!(
        serde_json::to_string(&BTreeMap::from([(pin.clone(), 1)])).unwrap(),
        "{\"PIN-7Q2ZZ\":1}"
    );
    assert_eq!(serde_json::to_value(&pin).unwrap(), json!(PIN));
    let section = p.section.to_string();
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([
            {"op": "create", "type": "prose", "id": "$s", "parent": section, "body": PIN},
            {"op": "create", "type": "prose", "id": "$q", "parent": section, "body": PIN},
            {"op": "create", "type": "prose", "id": "$t", "parent": section, "body": format!("see {PIN}")}
        ]),
        "",
    );
    let s = id(&w.created["$s"]);
    let q = id(&w.created["$q"]);
    let t = id(&w.created["$t"]);
    let (vq, vt) = (version(&p.r, "work", &q), version(&p.r, "work", &t));
    let review = Review {
        subject: ReviewSubject::Mention {
            block: q.clone(),
            entity: p.term.clone(),
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: PIN.into(),
        citations: vec![Citation {
            block: q.clone(),
            note: PIN.into(),
        }],
        provenance: prov("agent:r"),
    };
    let rh = p.r.store_mut().put(kinds::REVIEW, &review).unwrap();
    p.r.invalidate();
    let d = dry(&p.r, &[s.to_string()]);
    let mut blocks = vec![
        quote(ImpactKind::BlockVersion, format!("{q}@{vq}"), PIN, true),
        quote(ImpactKind::BlockVersion, format!("{t}@{vt}"), PIN, true),
    ];
    blocks.sort_by(|a, b| a.record.cmp(&b.record));
    blocks.push(quote(ImpactKind::Review, rh, PIN, false));
    assert_eq!(
        d.quotes, blocks,
        "a string is a quote by its type, never by its shape; the review's ids are not"
    );
    // a body that spells an existing id: the id-typed parents and subjects that hold it are
    // identifiers, not quotes
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "create", "type": "prose", "id": "$i", "parent": section, "body": q.to_string()}]),
        "",
    );
    let d = dry(&p.r, &[w.created["$i"].clone()]);
    assert_eq!(d.quotes, vec![], "q's id appears only as a BlockId value");
}

#[test]
fn rdx5_a_pair_review_and_a_tension_through_it_are_affected() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    // the only edge to the target: the review's pair subject
    let review = Review {
        subject: ReviewSubject::Pair {
            key: Hash([9; 32]),
            items: vec![
                PairItem {
                    block: p.term.clone(),
                    content: Hash([4; 32]),
                },
                PairItem {
                    block: p.prose.clone(),
                    content: Hash([5; 32]),
                },
            ],
        },
        case_hash: None,
        verdict: Verdict::Tension,
        reason: "they disagree".into(),
        citations: vec![],
        provenance: prov("agent:r"),
    };
    let rh = p.r.store_mut().put(kinds::REVIEW, &review).unwrap();
    // the only edge to the target: that real review
    let tension = SemanticTension {
        key: Hash([3; 32]),
        items: vec![PairItem {
            block: p.term.clone(),
            content: Hash([6; 32]),
        }],
        review: rh,
        advice: Some("align them".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: prov("agent:t"),
    };
    let th = p.r.store_mut().put(kinds::TENSION, &tension).unwrap();
    // neither: a review and a tension of the term only
    let other = Review {
        subject: ReviewSubject::Pair {
            key: Hash([8; 32]),
            items: vec![PairItem {
                block: p.term.clone(),
                content: Hash([4; 32]),
            }],
        },
        ..review.clone()
    };
    let oh = p.r.store_mut().put(kinds::REVIEW, &other).unwrap();
    let ot =
        p.r.store_mut()
            .put(
                kinds::TENSION,
                &SemanticTension {
                    review: oh,
                    ..tension.clone()
                },
            )
            .unwrap();
    p.r.invalidate();
    let d = dry(&p.r, &[p.prose.to_string()]);
    assert_eq!(
        d.report_rows_of(&[ImpactKind::Review, ImpactKind::Tension]),
        vec![
            row(ImpactKind::Review, rh, ImpactRelation::References, false),
            row(ImpactKind::Tension, th, ImpactRelation::References, false),
        ],
        "review {oh} and tension {ot} name only the term"
    );
}

#[test]
fn rdx5_a_request_computes_its_report_once() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let t = vec![p.prose.to_string()];
    let before = report_computations_for_tests();
    p.r.redact(&writer(), input(&t, true)).unwrap();
    assert_eq!(report_computations_for_tests() - before, 1, "a dry run");
    let before = report_computations_for_tests();
    let rec = p.r.redact(&writer(), input(&t, false)).unwrap();
    assert!(rec.redaction.is_some());
    assert_eq!(
        report_computations_for_tests() - before,
        1,
        "a recorded request computes its report once, under the writer"
    );
}

/// Final review fix round 1, finding 5 (ruling final): the shortened presentation keeps the
/// counts, every warning, the result, the reason and the report hash; the targets are listed in
/// canonical order while they fit, then one line says how many more there are and where the
/// full list is. 256 valid `id@hash` selectors fit the default 4000-token budget that way; the
/// JSON report still lists all 256, and the manifest hash is the one a 20000-token text shows.
#[test]
fn rdx5_the_shortened_text_lists_targets_while_they_fit() {
    use rdy_engine::redaction::presentation::{DEFAULT_TOKENS, MAX_TOKENS, ROWS_OMITTED};
    use rdy_engine::redaction::report_text;
    let mut p = base();
    let ops: Vec<serde_json::Value> = (0..256)
        .map(|i| {
            json!({"op": "create", "type": "prose", "parent": p.section.to_string(), "body": format!("Paragraph {i} of the leak.")})
        })
        .collect();
    let w = write(
        &mut p.r,
        &writer(),
        "work",
        serde_json::Value::Array(ops),
        "",
    );
    let targets: Vec<String> = w
        .created
        .values()
        .map(|i| {
            let b = id(i);
            format!("{b}@{}", version(&p.r, "work", &b))
        })
        .collect();
    assert_eq!(targets.len(), 256);
    let res = rdy_engine::redaction::dry_run(&p.r.read(), &input(&targets, true)).unwrap();
    assert_eq!(
        res.report.targets.len(),
        256,
        "the JSON report lists every target"
    );
    let json = serde_json::to_value(&res).unwrap();
    assert_eq!(json["report"]["targets"].as_array().unwrap().len(), 256);

    let full = report_text(&res, MAX_TOKENS).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.text.matches("\nTarget: ").count(), 256);

    let short = report_text(&res, DEFAULT_TOKENS).unwrap();
    assert!(short.truncated);
    assert!(short.text.len() / 4 <= DEFAULT_TOKENS);
    let lines: Vec<&str> = short.text.lines().collect();
    assert_eq!(lines[0], "Redaction dry run");
    let shown = lines.iter().filter(|l| l.starts_with("Target: ")).count();
    assert!(shown > 0 && shown < 256, "{shown}");
    // the shown targets are the first ones, in canonical order
    let canonical: Vec<String> = full
        .text
        .lines()
        .filter(|l| l.starts_with("Target: "))
        .map(String::from)
        .collect();
    assert_eq!(lines[1..=shown], canonical[..shown]);
    assert_eq!(
        lines[shown + 1],
        format!(
            "… and {} more targets; the full list is in the JSON report (--json) and bound by report {}",
            256 - shown,
            res.report.report_hash
        )
    );
    assert!(lines[shown + 2].starts_with("Versions: 256; blobs: 0;"));
    assert!(lines[shown + 3].starts_with("Payloads: "));
    assert_eq!(lines[shown + 4], ROWS_OMITTED);
    // every mandatory line of the full text is in the shortened one
    for l in full.text.lines().filter(|l| {
        l.starts_with("Reason (")
            || l.starts_with("W_REDACTION_")
            || l.starts_with("Recorded assessments")
    }) {
        assert!(lines.contains(&l), "{l}");
    }
    assert_eq!(*lines.last().unwrap(), "No request was recorded.");
    // one more target would not fit
    let mut one_more = lines[..=shown].to_vec();
    one_more.push(canonical[shown].as_str());
    let longer = [one_more.as_slice(), &lines[shown + 1..]]
        .concat()
        .join("\n");
    assert!(longer.len() / 4 > DEFAULT_TOKENS);
    // the manifest is the same whatever the budget
    let again = rdy_engine::redaction::dry_run(&p.r.read(), &input(&targets, true)).unwrap();
    assert_eq!(again.report.report_hash, res.report.report_hash);

    // the mandatory lines alone over the budget: refused, as before
    let mut long = input(&targets, true);
    long.reason = "r".repeat(4096);
    let res = rdy_engine::redaction::dry_run(&p.r.read(), &long).unwrap();
    let e = report_text(&res, 256).unwrap_err();
    assert_eq!(e.code, E_REDACTION_BUDGET);
}

/// Final review fix round 2, finding 5 (ruling final): the report hash is a mandatory line of
/// every shortened text. When every target fits, the line after the targets is
/// `Report: {report_hash}.`; when some are left out, the omitted-targets line names it. Either
/// line is budgeted before any optional target.
#[test]
fn rdx5_the_shortened_text_always_holds_the_report_hash() {
    use rdy_engine::redaction::presentation::ROWS_OMITTED;
    // every target fits: rows give way, the report hash stays
    let big = sample_result(true, false, ReasonClass::Legal, 400);
    let hash = big.report.report_hash.to_string();
    let full = report_text(&big, 20_000).unwrap();
    assert!(!full.text.contains(&hash), "the full text has no hash line");
    let short = report_text(&big, full.text.len() / 4 - 1).unwrap();
    assert!(short.truncated);
    let lines: Vec<&str> = short.text.lines().collect();
    let targets = lines.iter().filter(|l| l.starts_with("Target: ")).count();
    assert_eq!(targets, big.report.targets.len(), "{}", short.text);
    assert_eq!(
        lines[targets + 1],
        format!("Report: {hash}."),
        "{}",
        short.text
    );
    assert!(lines[targets + 2].starts_with("Versions: "));
    assert!(lines.contains(&ROWS_OMITTED));
    assert_eq!(short.text.matches(&hash).count(), 1);
    // a smaller budget leaves targets out: the omitted-targets line names the hash instead
    let need = short.text.len() / 4;
    let below = (256..need)
        .rev()
        .map(|t| report_text(&big, t))
        .find_map(|r| r.ok().filter(|r| r.text.contains(" more targets;")))
        .expect("a budget that leaves a target out");
    assert!(below.truncated);
    assert!(
        below.text.contains(&format!(
            "more targets; the full list is in the JSON report (--json) and bound by report {hash}"
        )),
        "{}",
        below.text
    );
    assert!(!below.text.contains(&format!("Report: {hash}.")));
    assert_eq!(below.text.matches(&hash).count(), 1);
}

/// Final review fix round 1, finding 7: the validation order of the brief ("reason; expansion;
/// scan"): an unknown all-versions id and a selection over the selected-object bound are found
/// before the scan, so lowered scan bounds never mask them; with valid targets the scan bound
/// still refuses.
#[test]
fn rdx5_expansion_errors_precede_the_scan_bounds() {
    let p = base();
    let st = p.r.read();
    let d = Limits::default();
    let tiny_scan = [
        Limits { frames: 1, ..d },
        Limits { bytes: 1, ..d },
        Limits {
            frames: 1,
            bytes: 1,
            selected: 0,
            ..d
        },
    ];
    for lim in tiny_scan {
        let _g = override_limits_for_tests(lim);
        // an unknown all-versions id, beside a valid target
        let unknown = vec![p.prose.to_string(), "p-ZZZZZ".to_string()];
        let e = st.redaction_report(&input(&unknown, true)).unwrap_err();
        assert_eq!(e.code, E_REDACTION_TARGET, "{lim:?}: {e}");
        assert_eq!(e.message, "Cannot redact p-ZZZZZ: unknown target.");
    }
    // over the selected-object bound, with the scan bounds too small as well
    for (frames, bytes) in [(1, d.bytes), (d.frames, 1), (1, 1)] {
        let _g = override_limits_for_tests(Limits {
            frames,
            bytes,
            selected: 1,
            ..d
        });
        let both = vec![p.prose.to_string(), p.term.to_string()];
        let e = st.redaction_report(&input(&both, true)).unwrap_err();
        assert_eq!(e.code, E_REDACTION_LIMIT, "{e}");
        assert_eq!(e.message, "The redaction exceeds 100000 selected objects.");
        // a selection within the bound meets the scan bound
        let one = vec![p.prose.to_string()];
        let e = st.redaction_report(&input(&one, true)).unwrap_err();
        assert_eq!(e.code, E_REDACTION_LIMIT);
        assert!(e.message.contains("scanned"), "{e}");
    }
}

/// Final review fix round 2, finding 7 and the bare-id breakage: the pre-scan expansion reads
/// every stored BLOCK version, as the walk's expansion does, never only the versions a change
/// produced. A bare id whose only block no change produced (a standalone or imported BLOCK frame)
/// selects it as its `id@hash` form does; an id with a second, unproduced version expands to
/// both, and that selection over the selected-object bound is found before a lowered scan bound.
#[test]
fn rdx5_the_pre_scan_expansion_reads_every_stored_version() {
    let mut p = base();
    let cur =
        p.r.read()
            .block(&Scope::View("work".into()), &p.prose)
            .unwrap();
    let mut second = cur.clone();
    second.body = "A version no change produced.".into();
    let v2 = p.r.store_mut().put(kinds::BLOCK, &second).unwrap();
    let mut lone = cur.clone();
    lone.id = id("p-ABCDE");
    lone.body = "A block no change produced.".into();
    let lv = p.r.store_mut().put(kinds::BLOCK, &lone).unwrap();
    p.r.invalidate();
    let st = p.r.read();
    assert!(st.reader().producers(&lv).unwrap().is_empty());
    assert!(st.reader().producers(&v2).unwrap().is_empty());
    // bare-id and explicit-version parity
    for sel in ["p-ABCDE".to_string(), format!("p-ABCDE@{lv}")] {
        let res = dry_run(&st, &input(std::slice::from_ref(&sel), true))
            .unwrap_or_else(|e| panic!("{sel}: {e}"));
        assert_eq!(res.report.versions, vec![(lone.id.clone(), lv)], "{sel}");
    }
    // the prose's id selects both its versions
    let res = dry_run(&st, &input(&[p.prose.to_string()], true)).unwrap();
    let mut want = vec![(p.prose.clone(), cur.version_hash()), (p.prose.clone(), v2)];
    want.sort();
    assert_eq!(res.report.versions, want);
    // two selected objects over a bound of one: the selected-object error, before the scan's
    let d = Limits::default();
    for (frames, bytes) in [(1, d.bytes), (d.frames, 1)] {
        let _g = override_limits_for_tests(Limits {
            frames,
            bytes,
            selected: 1,
            ..d
        });
        let e = st
            .redaction_report(&input(&[p.prose.to_string()], true))
            .unwrap_err();
        assert_eq!(e.code, E_REDACTION_LIMIT, "{e}");
        assert_eq!(e.message, "The redaction exceeds 100000 selected objects.");
    }
}

/// A signature of `subject` with the given text fields and SSHSIG bytes.
fn signature(
    subject: Hash,
    namespace: &str,
    statement: &[u8],
    actor: &str,
    run: Option<&str>,
    sig: &[u8],
) -> Signature {
    Signature {
        subject,
        namespace: namespace.into(),
        statement: statement.to_vec(),
        sig: sig.to_vec(),
        signed_by: Provenance {
            actor: actor.into(),
            run: run.map(str::to_string),
            role: Role::Approver,
            at_ms: 11,
        },
    }
}

#[test]
fn sig4_redaction_scan_reports_signature_text() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let pi = p.r.propose(&writer(), "work", "seed", None).unwrap();
    let landing = h(&p.r.land(&approver(), &pi.id).unwrap());
    let tag = h(&p.r.tag(&approver(), "v1.0.0", None, None).unwrap().tag);
    let statement = format!("rdy-statement: 1\nkind: approval\nnote: {SECRET}\n");
    let sigs = [
        // the statement quotes the source string
        signature(
            landing,
            "rdy-approval-v1",
            statement.as_bytes(),
            "human:t",
            None,
            &[1],
        ),
        // the namespace does
        signature(tag, SECRET, b"plain", "human:t", None, &[2]),
        // the signer's actor and run both do: one quote row
        signature(
            tag,
            "rdy-tag-v1",
            b"plain",
            &format!("agent:{SECRET}"),
            Some(SECRET),
            &[3],
        ),
        // the run alone does
        signature(
            landing,
            "rdy-relay-v1",
            b"plain",
            "agent:a",
            Some(SECRET),
            &[4],
        ),
        // only the opaque SSHSIG bytes hold it: no quote
        signature(
            landing,
            "rdy-approval-v1",
            b"plain",
            "human:t",
            None,
            SECRET.as_bytes(),
        ),
    ];
    let ids: Vec<Hash> = sigs
        .iter()
        .map(|g| p.r.store_mut().put_signature(g).unwrap())
        .collect();
    p.r.invalidate();
    let d = dry(&p.r, &[format!("{}@{vp}", p.prose)]);
    // every signature names an affected subject: a reference row each; the four with text in a
    // visited string also quote, in the predecessor's row order
    let mut want: Vec<ImpactRow> = vec![];
    let mut order: Vec<(String, usize)> = ids
        .iter()
        .enumerate()
        .map(|(i, g)| (g.to_string(), i))
        .collect();
    order.sort();
    for (record, i) in &order {
        want.push(row(
            ImpactKind::AuditText,
            record,
            ImpactRelation::References,
            false,
        ));
        if *i < 4 {
            want.push(row(
                ImpactKind::AuditText,
                record,
                ImpactRelation::Quotes,
                false,
            ));
        }
    }
    let ids_text: BTreeSet<String> = ids.iter().map(Hash::to_string).collect();
    let got: Vec<ImpactRow> = d
        .inside
        .iter()
        .filter(|r| ids_text.contains(&r.record))
        .cloned()
        .collect();
    assert_eq!(got, want);
    assert!(
        got.iter()
            .all(|r| r.kind == ImpactKind::AuditText && !r.erasable)
    );
    // the transient quote rows: one per quoting signature, the complete source string
    let mut want_quotes: Vec<QuoteRow> = order
        .iter()
        .filter(|(_, i)| *i < 4)
        .map(|(record, _)| quote(ImpactKind::AuditText, record, SECRET, false))
        .collect();
    want_quotes.sort_by(|a, b| a.record.cmp(&b.record));
    let got_quotes: Vec<QuoteRow> = d
        .quotes
        .iter()
        .filter(|q| ids_text.contains(&q.record))
        .cloned()
        .collect();
    assert_eq!(got_quotes, want_quotes);
    // the subjects are still reported as themselves
    assert!(d.inside.contains(&row(
        ImpactKind::Landing,
        landing,
        ImpactRelation::References,
        false
    )));
    assert!(d.inside.contains(&row(
        ImpactKind::Tag,
        "v1.0.0",
        ImpactRelation::References,
        false
    )));
    // a completed removal keeps every signature frame and its bytes
    let target = RedactTarget::Version {
        id: p.prose.clone(),
        version: vp,
    };
    let p = apply_fixture(p, vec![target]);
    let st = p.r.read();
    for (g, id) in sigs.iter().zip(&ids) {
        assert_eq!(&st.reader().signature(id).unwrap(), g);
    }
    drop(st);
    let present: Vec<Hash> = frames_from(&p.r, ORIGIN)
        .into_iter()
        .filter(|(k, _, payload)| *k == kinds::SIGNATURE && payload.is_some())
        .map(|(_, h, _)| h)
        .collect();
    assert_eq!(present.len(), ids.len());
    assert!(ids.iter().all(|g| present.contains(g)));
    // a statement that is not UTF-8 text is an invalid signature for that frame
    let mut p = p;
    let bad =
        p.r.store_mut()
            .put_signature(&signature(
                landing,
                "rdy-approval-v1",
                &[0xff, 0xfe],
                "human:t",
                None,
                &[5],
            ))
            .unwrap();
    p.r.invalidate();
    let e =
        p.r.read()
            .redaction_report(&input(&[p.term.to_string()], true))
            .unwrap_err();
    assert_eq!(e.code, E_SIG_INVALID, "{e}");
    assert_eq!(e.message, format!("signature {bad} is invalid"));
    assert_eq!(
        e.fix,
        "check the source of this signature; it may be damaged or forged"
    );
}

/// The bytes of a carried removal record approved by `actor`, with one span.
fn carried_record(actor: String) -> Vec<u8> {
    let mut buf = vec![0u8; 2 << 20];
    let record = rdy_model::RedactionCarried {
        request: Hash([1; 32]),
        apply: Hash([2; 32]),
        landing: Hash([3; 32]),
        approved_by: Provenance {
            actor,
            run: None,
            role: Role::Approver,
            at_ms: 0,
        },
        reason_class: ReasonClass::Secret,
        targets: vec![RedactTarget::Blob(Hash([5; 32]))],
        spans: vec![rdy_model::redaction::CarriedSpan {
            place: rdy_model::redaction::FramePlace {
                segment: 0,
                offset: 0,
            },
            kind: kinds::BLOB_CHUNK,
            hash: Hash([6; 32]),
            len: 1,
            residue: None,
            held: false,
        }],
    };
    encode_into(&record, &mut buf).unwrap().to_vec()
}

#[test]
fn hc1_the_report_decodes_carried_records_within_bounds() {
    let _clock = pin_clock(1_000_000);
    let report = |payload: Vec<u8>| {
        let mut p = base();
        let h =
            p.r.store_mut()
                .write_guarded_for_tests(vec![(kinds::REDACTION_CARRIED, payload)])
                .unwrap()[0];
        p.r.invalidate();
        p.r.read()
            .redaction_report(&input(&[p.prose.to_string()], true))
            .map(|d| (h, d))
    };
    // its strings are scanned: an approver whose actor quotes the removed text is audit text
    let (h, d) = report(carried_record(format!("human:{SECRET}"))).unwrap();
    assert!(
        d.quotes
            .iter()
            .any(|q| q.kind == ImpactKind::AuditText && q.record == h.to_string()),
        "{:?}",
        d.quotes
    );
    // an approver's text of 1048576 bytes is read; one more byte is the bounded record's refusal
    report(carried_record("a".repeat(1_048_576))).unwrap();
    let e = report(carried_record("a".repeat(1_048_577))).unwrap_err();
    assert!(e.message.contains("1048576 text bytes"), "{e}");
    // an empty or truncated record does not decode
    let full = carried_record("person:ann".into());
    for p in [vec![], full[..full.len() - 1].to_vec()] {
        let e = report(p).unwrap_err();
        assert!(e.message.contains("record does not decode"), "{e}");
    }
}

/// A plan version that quotes `SECRET` in the one string field `field` names.
fn quoting_plan(field: &str) -> rdy_model::plan::Plan {
    use rdy_model::plan::{
        Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem,
    };
    let text = |f: &str, plain: &str| {
        if f == field {
            SECRET.to_string()
        } else {
            plain.to_string()
        }
    };
    let by = Provenance {
        actor: text("by", "agent:a"),
        run: None,
        role: Role::Writer,
        at_ms: 1,
    };
    let sealed = matches!(field, "subject" | "edge");
    Plan {
        stable: None,
        prev: None,
        view: text("view", "work"),
        proposal: sealed.then_some(Hash([4; 32])),
        answers: vec![PlanAnswer {
            item: Hash([1; 32]),
            act: PlanAct::Defer,
            reason: Some(text("reason", "later")),
            subject: None,
            cause: Hash([2; 32]),
            by,
        }],
        heads: vec![PlanHead {
            repo: text("repo", "app"),
            head: None,
        }],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: Some(text("run", "r1")),
            role: Role::Writer,
            at_ms: 1,
        },
        seal: sealed.then(|| PlanSeal {
            landing: Hash([3; 32]),
            version: Hash([4; 32]),
            decisions: vec![],
            items: vec![SealedItem {
                id: Hash([5; 32]),
                kind: PlanKind::Update,
                subject: text("subject", "b-00001"),
                cause: id("D-00002"),
                edge: text("edge", "reflects"),
                outcome: PlanOutcome::Open,
                answer: None,
            }],
        }),
    }
}

#[test]
fn cp3_plan_quotes_use_the_existing_report_only_policy() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let vp = version(&p.r, "work", &p.prose);
    let before = dry(&p.r, &[format!("{}@{vp}", p.prose)]);
    // one version per string field: the view, an answer's reason and person, a head's repo, the
    // provenance, and a sealed item's subject and edge
    let fields = ["view", "reason", "by", "repo", "run", "subject", "edge"];
    let mut plans: Vec<(Hash, Vec<u8>)> = vec![];
    for f in fields {
        let mut buf = vec![0u8; 1 << 16];
        let bytes = encode_into(&quoting_plan(f), &mut buf).unwrap().to_vec();
        let h =
            p.r.store_mut()
                .write_guarded_for_tests(vec![(kinds::PLAN, bytes.clone())])
                .unwrap()[0];
        plans.push((h, bytes));
    }
    p.r.invalidate();
    // the dry run succeeds; each quoting plan is one audit text row that names its full hash and
    // is not erasable, and one transient quote row with the complete source string
    let d = dry(&p.r, &[format!("{}@{vp}", p.prose)]);
    for (h, _) in &plans {
        let record = h.to_string();
        assert!(
            d.inside.contains(&row(
                ImpactKind::AuditText,
                &record,
                ImpactRelation::Quotes,
                false
            )),
            "{record}: {:?}",
            d.inside
        );
        assert!(
            d.quotes
                .contains(&quote(ImpactKind::AuditText, &record, SECRET, false)),
            "{record}: {:?}",
            d.quotes
        );
        assert!(
            !d.inside.iter().any(|r| r.record == record && r.erasable),
            "{record} is never erasable"
        );
    }
    // the warnings keep their base texts: no plan warning, and the quote warning unchanged
    let texts = |r: &RedactionReport| -> Vec<(String, String)> {
        r.warnings
            .iter()
            .map(|w| (w.code.clone(), w.message.clone()))
            .collect()
    };
    assert_eq!(texts(&d), texts(&before));
    let q = d
        .warnings
        .iter()
        .find(|w| w.code == rdy_engine::W_REDACTION_QUOTES)
        .expect("the quote warning");
    assert_eq!(q.message, rdy_engine::W_REDACTION_QUOTES_MESSAGE);
    // a completed removal keeps every plan frame, byte for byte, under its own hash
    let target = RedactTarget::Version {
        id: p.prose.clone(),
        version: vp,
    };
    let p = apply_fixture(p, vec![target]);
    let present: BTreeMap<Hash, Vec<u8>> = frames_from(&p.r, ORIGIN)
        .into_iter()
        .filter(|(k, _, _)| *k == kinds::PLAN)
        .map(|(_, h, payload)| (h, payload.expect("a plan is never removed")))
        .collect();
    for (h, bytes) in &plans {
        assert_eq!(present.get(h), Some(bytes), "{h}");
        assert_eq!(Hash::of(bytes), *h);
        assert_eq!(p.r.read().reader().plan(h).unwrap(), {
            let f = fields[plans.iter().position(|(x, _)| x == h).unwrap()];
            quoting_plan(f)
        });
    }
}
