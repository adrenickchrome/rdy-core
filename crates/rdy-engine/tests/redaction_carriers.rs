#![cfg(unix)]
// Unix only: the fixtures write real project directories (seams rule, FW-XP1 D-1RXEA).
//! ok8: a redaction report scans every carried text and reports the retained copies (D-EPK1Z:
//! "In this release a redaction reports, and does not erase, the stored records that quote the
//! redacted content"; D-B9S45 fixes the warning text). The carried records are ASSESSMENT,
//! DISPUTE, DISPUTE_RESOLUTION and CHECK_REQUEST, and the Check REVIEW of a request with its
//! three verdicts. No product path writes one, so the fixtures put them through the store. Every
//! row about a carried record is uneraseable: a removal reports them and never drops them.
use rdy_engine::redaction::presentation::report_text;
use rdy_engine::redaction::{
    ASSIGNED_KINDS, Limits, RedactInput, RedactionReport, TextPolicy, dry_run,
    override_limits_for_tests, require_text_policies, text_policy,
};
use rdy_engine::{
    E_REDACTION_LIMIT, Rdy, RdyError, Scope, Session, WriteRequest, WriteResult, pin_clock,
};
use rdy_model::assessment::*;
use rdy_model::check_request::{CheckCode, CheckMode, CheckRequest, CheckSubject};
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::redaction::{
    AssessmentSupport, ImpactKind, ImpactRelation, ImpactRow, REPORT_DOMAIN, encode_into,
};
use rdy_model::{
    ActorRun, Answer, AnswerAct, BlockId, Citation, Hash, Provenance, RelayEvidence, Review,
    ReviewSubject, Role, Verdict,
};
use rdy_store::kinds;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// The warning of every report (D-B9S45), word for word.
const QUOTES_WARNING: &str = "Quote matches use complete source strings. Partial quotes may still hold content. Snapshot issue lists and summaries, review citations, tension advice, answer notes and change-plan versions are not erased.";

const SECRET: &str = "The launch code is ZEBRA-7731.";

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
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

fn prov(actor: &str) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role: Role::Writer,
        at_ms: 7,
    }
}

fn input(targets: &[String], dry_run: bool) -> RedactInput {
    RedactInput {
        targets: targets.to_vec(),
        reason_class: "secret".into(),
        reason: "a leaked code".into(),
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

/// The carried kinds' impact kinds, and the Check review's.
fn carried_kind(k: ImpactKind) -> bool {
    matches!(
        k,
        ImpactKind::Assessment
            | ImpactKind::Dispute
            | ImpactKind::DisputeResolution
            | ImpactKind::CheckRequest
    )
}

fn put(r: &mut Rdy, kind: u8, v: &(impl serde::Serialize + 'static)) -> Hash {
    let hash = r.store_mut().put(kind, v).unwrap();
    r.invalidate();
    hash
}

// ---------------------------------------------------------------------------------------------
// the neutral carried records: each holds no source string and names only the neutral block
// ---------------------------------------------------------------------------------------------

/// An assessment that names `blocks` in every block list and holds no free text of interest. The
/// `tag` makes its content (and so its hash) its own.
fn assessment(tag: u8, blocks: &[BlockId]) -> Assessment {
    let proposal = Hash::of(&[b'p', tag]);
    let dismissal = Hash::of(b"dismissal");
    let scores = AssessmentScores {
        overall: 90,
        grounded: 91,
        consistent: 92,
        current: 93,
        connected: 94,
        complete: 95,
        governed: 96,
    };
    let issue = AssessmentIssue {
        block: blocks[0].clone(),
        dimension: "grounded".into(),
        code: "untied_content".into(),
        subject: Some("a neutral subject".into()),
        fix: "tie it".into(),
        dismissal: Some(dismissal),
    };
    Assessment {
        proposal,
        base: None,
        method: 1,
        pack: AssessmentPack {
            name: "rdy-qa".into(),
            version: "1.0.0".into(),
            digest: Hash::of(b"pack digest"),
        },
        config: Hash::of(&[b'c', tag]),
        policy: Some(AssessedPolicy {
            policy: None,
            verdict: PolicyVerdict::NeedsPerson {
                role: Some("maintainer".into()),
                reasons: vec!["no authority policy is set".into()],
            },
        }),
        facts: AssessmentFacts {
            changes: 1,
            consequences: 2,
            plan: AssessmentPlanCounts {
                open: 1,
                closed: 2,
                deferred: 3,
                by_kind: vec![(
                    "tension".into(),
                    AssessmentCounts {
                        open: 1,
                        closed: 0,
                        deferred: 0,
                    },
                )],
            },
            supersedes: vec![],
            reaffirms: vec![],
            reversibility: vec![],
            exceptions: vec![AssessmentException {
                kind: AssessmentExceptionKind::OneWay,
                reference: "D-00001".into(),
            }],
        },
        tensions: vec![AssessmentTension {
            id: Hash::of(b"tension id"),
            kind: "edit_collision".into(),
            severity: AssessmentSeverity::Blocking,
            status: AssessmentTensionStatus::Dismissed,
            answer: None,
            dismissal: Some(dismissal),
            summary: "two edits".into(),
            blocks: blocks.to_vec(),
            vkey: Hash::of(b"tension vkey"),
        }],
        tensions_omitted: 0,
        suspects: vec![],
        suspects_omitted: 0,
        coverage: AssessmentCoverage {
            pages: vec![],
            corpus: AssessmentScoreChange {
                before: scores.clone(),
                after: scores,
            },
        },
        confidence: AssessmentConfidence {
            level: AssessmentLevel::Medium,
            reasons: vec![issue.clone()],
        },
        reach: blocks.to_vec(),
        card: AssessmentCard {
            hash: Hash::of(b"card"),
            facts: vec!["a neutral fact".into()],
        },
        scope: AssessmentScope {
            proposal,
            blocks: blocks.to_vec(),
        },
        issues: vec![issue],
    }
}

fn dispute(landing: Hash, reason: &str) -> Dispute {
    Dispute {
        landing,
        opened_by: prov("human:alice"),
        reason: reason.into(),
        at_ms: 11,
    }
}

fn resolution(dispute: Hash, correction: Option<Hash>) -> DisputeResolution {
    DisputeResolution {
        dispute,
        outcome: DisputeOutcome::Upheld,
        by: prov("human:bob"),
        correction,
    }
}

fn request(tag: u8, blocks: &[BlockId]) -> CheckRequest {
    CheckRequest {
        check: "consistency".into(),
        subject: CheckSubject {
            blocks: blocks.to_vec(),
            change: None,
            version: None,
            code: Some(CheckCode {
                repo: "app".into(),
                from: "a".repeat(40),
                to: "b".repeat(40),
            }),
        },
        scope: "docs".into(),
        case_hash: Hash::of(&[b'k', tag]),
        mode: CheckMode::Independent,
        reason: Some("a second look".into()),
        requested_by: prov("agent:w"),
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: None,
        }),
        relay: Some(RelayEvidence {
            quoted: "check it".into(),
            session_ref: "sess-1".into(),
            question: None,
        }),
    }
}

fn check_review(request: Hash, case: Hash, verdict: Verdict, cite: &BlockId, note: &str) -> Review {
    let (reason, citations) = match verdict {
        Verdict::Pass => (String::new(), vec![]),
        _ => (
            "a neutral reason".to_string(),
            vec![Citation {
                block: cite.clone(),
                note: note.into(),
            }],
        ),
    };
    Review {
        subject: ReviewSubject::Check { request },
        case_hash: Some(case),
        verdict,
        reason,
        citations,
        provenance: prov("agent:checker"),
    }
}

// ---------------------------------------------------------------------------------------------
// the scan fixture: many selected blocks, one distinct source string each
// ---------------------------------------------------------------------------------------------

/// A project with `n` prose blocks that each hold one distinct source string, and a term the
/// neutral records name instead.
struct Scan {
    _dir: tempfile::TempDir,
    r: Rdy,
    targets: Vec<String>,
    tokens: Vec<String>,
    term: BlockId,
}

fn scan_project(n: usize) -> Scan {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}}),
        json!({"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}),
    ];
    let tokens: Vec<String> = (0..n).map(|k| format!("scan-token-{k:03}-end")).collect();
    for (k, t) in tokens.iter().enumerate() {
        ops.push(json!({"op": "create", "type": "prose", "id": format!("$p{k}"), "parent": "$s", "body": t}));
    }
    let w = write(&mut r, "work", serde_json::Value::Array(ops));
    Scan {
        targets: (0..n)
            .map(|k| w.created[&format!("$p{k}")].clone())
            .collect(),
        term: id(&w.created["$t"]),
        tokens,
        r,
        _dir: dir,
    }
}

/// Builds the record that holds a token in one field (the token and a distinct tag).
type Build = Box<dyn Fn(&str, u8) -> Rec>;

/// The rows of a scan table: the label of a text field, and its builder.
type Table = Vec<(&'static str, Build)>;

/// A table row: the label of a text field, and the record that holds `token` in it.
enum Rec {
    Assessment(Box<Assessment>),
    Dispute(Dispute),
    Resolution(DisputeResolution),
    Request(CheckRequest),
    Review(Review),
}

/// Puts the record of each table row with its own token, then checks that the dry run reports
/// exactly one uneraseable quote of that token on that record, and a Quotes impact row.
fn check_table(s: &mut Scan, table: Table) {
    assert!(table.len() <= s.tokens.len(), "enough source strings");
    let mut want: BTreeMap<(u8, String), (ImpactKind, String)> = BTreeMap::new();
    for (k, (label, build)) in table.iter().enumerate() {
        let token = s.tokens[k].clone();
        let (kind, hash) = match build(&token, k as u8) {
            Rec::Assessment(a) => (
                ImpactKind::Assessment,
                put(&mut s.r, kinds::ASSESSMENT, &*a),
            ),
            Rec::Dispute(d) => (ImpactKind::Dispute, put(&mut s.r, kinds::DISPUTE, &d)),
            Rec::Resolution(d) => (
                ImpactKind::DisputeResolution,
                put(&mut s.r, kinds::DISPUTE_RESOLUTION, &d),
            ),
            Rec::Request(c) => (
                ImpactKind::CheckRequest,
                put(&mut s.r, kinds::CHECK_REQUEST, &c),
            ),
            Rec::Review(v) => (ImpactKind::Review, put(&mut s.r, kinds::REVIEW, &v)),
        };
        assert!(
            want.insert((kind as u8, hash.to_string()), (kind, token))
                .is_none(),
            "{label}: the record is its own"
        );
    }
    let d = dry(&s.r, &s.targets);
    for ((_, record), (kind, token)) in &want {
        let quotes: Vec<_> = d
            .quotes
            .iter()
            .filter(|q| q.kind == *kind && q.record == *record)
            .collect();
        assert_eq!(quotes.len(), 1, "{kind:?} {record}: {:?}", d.quotes);
        assert_eq!(quotes[0].quote, *token, "{kind:?} {record}");
        assert!(!quotes[0].erasable, "{kind:?} {record}");
        assert!(
            d.inside
                .contains(&row(*kind, record, ImpactRelation::Quotes, false)),
            "{kind:?} {record}"
        );
    }
    // nothing else quotes: every quote row is one of the table's
    assert_eq!(d.quotes.len(), want.len(), "{:?}", d.quotes);
    // every row about a carried record or a Check review is uneraseable
    for r in &d.inside {
        if want
            .keys()
            .any(|(k, rec)| *k == r.kind as u8 && *rec == r.record)
        {
            assert!(!r.erasable, "{r:?}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// guards
// ---------------------------------------------------------------------------------------------

#[test]
fn ok8_every_carried_kind_has_a_text_policy() {
    // the assigned-kind registry: every code 1..=28 but the unassigned 12
    let want: BTreeSet<u8> = (1..=28u8).filter(|k| *k != 12).collect();
    assert_eq!(
        ASSIGNED_KINDS.iter().copied().collect::<BTreeSet<u8>>(),
        want
    );
    assert_eq!(ASSIGNED_KINDS.len(), want.len(), "no kind is listed twice");
    for k in ASSIGNED_KINDS {
        assert!(text_policy(k).is_some(), "kind {k} has no text policy");
    }
    for k in [
        kinds::ASSESSMENT,
        kinds::DISPUTE,
        kinds::DISPUTE_RESOLUTION,
        kinds::CHECK_REQUEST,
    ] {
        assert!((25..=28).contains(&k));
        assert_eq!(text_policy(k), Some(TextPolicy::Strings), "kind {k}");
    }
    require_text_policies(&ASSIGNED_KINDS).unwrap();
    // an unassigned or future kind is refused, never read as absent
    for k in [12u8, 29, 127] {
        let e = require_text_policies(&[k]).unwrap_err();
        assert_eq!(
            e.message,
            format!("the redaction report has no text policy for object kind {k}")
        );
    }
}

// ---------------------------------------------------------------------------------------------
// quote scans through the shared visitor
// ---------------------------------------------------------------------------------------------

#[test]
fn ok8_every_assessment_text_is_scanned() {
    let _clock = pin_clock(1_000_000);
    let mut s = scan_project(16);
    let neutral = vec![s.term.clone()];
    type Edit = fn(&mut Assessment, &str);
    let edits: Vec<(&'static str, Edit)> = vec![
        ("pack name", |a, t| a.pack.name = t.into()),
        ("pack version", |a, t| a.pack.version = t.into()),
        ("policy role", |a, t| {
            a.policy = Some(AssessedPolicy {
                policy: None,
                verdict: PolicyVerdict::NeedsPerson {
                    role: Some(t.into()),
                    reasons: vec!["no authority policy is set".into()],
                },
            })
        }),
        ("policy reason", |a, t| {
            a.policy = Some(AssessedPolicy {
                policy: None,
                verdict: PolicyVerdict::NeedsPerson {
                    role: None,
                    reasons: vec![t.into()],
                },
            })
        }),
        ("policy rule", |a, t| {
            a.policy = Some(AssessedPolicy {
                policy: Some(PolicyRef {
                    block: id("p-00009"),
                    version: Hash::of(b"unheld"),
                }),
                verdict: PolicyVerdict::Accepts { rule: t.into() },
            })
        }),
        ("exception reference", |a, t| {
            a.facts.exceptions[0].reference = t.into()
        }),
        ("tension summary", |a, t| a.tensions[0].summary = t.into()),
        ("issue subject", |a, t| a.issues[0].subject = Some(t.into())),
        ("issue fix", |a, t| a.issues[0].fix = t.into()),
        ("confidence subject", |a, t| {
            a.confidence.reasons[0].subject = Some(t.into())
        }),
        ("confidence fix", |a, t| {
            a.confidence.reasons[0].fix = t.into()
        }),
        ("card fact", |a, t| a.card.facts = vec![t.into()]),
    ];
    let table = edits
        .into_iter()
        .map(|(label, edit)| {
            let blocks = neutral.clone();
            let build: Build = Box::new(move |t, tag| {
                let mut a = assessment(tag, &blocks);
                edit(&mut a, t);
                Rec::Assessment(Box::new(a))
            });
            (label, build)
        })
        .collect();
    check_table(&mut s, table);
}

#[test]
fn ok8_dispute_reason_and_provenance_are_scanned() {
    let _clock = pin_clock(1_000_000);
    let mut s = scan_project(8);
    let landing = Hash::of(b"a landing");
    let dispute_hash = Hash::of(b"a dispute");
    let table: Table = vec![
        (
            "dispute reason",
            Box::new(move |t, _| Rec::Dispute(dispute(landing, t))),
        ),
        (
            "dispute actor",
            Box::new(move |t, _| {
                let mut d = dispute(landing, "a reason");
                d.opened_by.actor = t.into();
                Rec::Dispute(d)
            }),
        ),
        (
            "dispute run",
            Box::new(move |t, _| {
                let mut d = dispute(landing, "a reason");
                d.opened_by.run = Some(t.into());
                Rec::Dispute(d)
            }),
        ),
        (
            "resolution actor",
            Box::new(move |t, _| {
                let mut r = resolution(dispute_hash, Some(landing));
                r.by.actor = t.into();
                Rec::Resolution(r)
            }),
        ),
        (
            "resolution run",
            Box::new(move |t, _| {
                let mut r = resolution(dispute_hash, None);
                r.by.run = Some(t.into());
                Rec::Resolution(r)
            }),
        ),
    ];
    check_table(&mut s, table);
}

#[test]
fn ok8_check_requests_and_all_verdict_text_are_scanned() {
    let _clock = pin_clock(1_000_000);
    let mut s = scan_project(24);
    let neutral = vec![s.term.clone()];
    type Edit = fn(&mut CheckRequest, &str);
    let edits: Vec<(&'static str, Edit)> = vec![
        ("check name", |c, t| c.check = t.into()),
        ("scope", |c, t| c.scope = t.into()),
        ("code repo", |c, t| {
            c.subject.code.as_mut().unwrap().repo = t.into()
        }),
        ("code from", |c, t| {
            c.subject.code.as_mut().unwrap().from = t.into()
        }),
        ("code to", |c, t| {
            c.subject.code.as_mut().unwrap().to = t.into()
        }),
        ("reason", |c, t| c.reason = Some(t.into())),
        ("requester actor", |c, t| c.requested_by.actor = t.into()),
        ("requester run", |c, t| c.requested_by.run = Some(t.into())),
        ("relayer actor", |c, t| {
            c.relayed_by.as_mut().unwrap().actor = t.into()
        }),
        ("relayer run", |c, t| {
            c.relayed_by.as_mut().unwrap().run = Some(t.into())
        }),
        ("relay quote", |c, t| {
            c.relay.as_mut().unwrap().quoted = t.into()
        }),
        ("relay session", |c, t| {
            c.relay.as_mut().unwrap().session_ref = t.into()
        }),
        ("relay question", |c, t| {
            c.relay.as_mut().unwrap().question = Some(t.into())
        }),
    ];
    let mut table: Table = edits
        .into_iter()
        .map(|(label, edit)| {
            let blocks = neutral.clone();
            let build: Build = Box::new(move |t, tag| {
                let mut c = request(tag, &blocks);
                edit(&mut c, t);
                Rec::Request(c)
            });
            (label, build)
        })
        .collect();
    // each verdict's reason and each verdict's citation note, on a request that exists
    let case = request(200, &neutral).case_hash;
    let req = Hash::of(b"a request");
    for (label, verdict) in [
        ("pass citation", Verdict::Pass),
        ("concern citation", Verdict::Concern),
        ("fail citation", Verdict::Fail),
    ] {
        let term = s.term.clone();
        let note: Build = Box::new(move |t, _| {
            let mut v = check_review(req, case, verdict, &term, t);
            if verdict == Verdict::Pass {
                v.citations.push(Citation {
                    block: term.clone(),
                    note: t.into(),
                });
            }
            Rec::Review(v)
        });
        table.push((label, note));
    }
    for (label, verdict) in [
        ("concern reason", Verdict::Concern),
        ("fail reason", Verdict::Fail),
    ] {
        let term = s.term.clone();
        let reason: Build = Box::new(move |t, _| {
            let mut v = check_review(req, case, verdict, &term, "a neutral note");
            v.reason = t.into();
            Rec::Review(v)
        });
        table.push((label, reason));
    }
    check_table(&mut s, table);
    // an AcceptVerdict answer is an ANSWER: its note is scanned by the existing answer rows, and
    // it brings no relationship of its own
    let concern = check_review(req, case, Verdict::Concern, &s.term, "a neutral note");
    let review = put(&mut s.r, kinds::REVIEW, &concern);
    let token = s.tokens[23].clone();
    let answer = Answer {
        item: format!("verdict:{review}"),
        act: AnswerAct::AcceptVerdict,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: prov("human:t"),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: Some(token.clone()),
    };
    let ah = put(&mut s.r, kinds::ANSWER, &answer);
    let d = dry(&s.r, &s.targets);
    let got: Vec<_> = d
        .quotes
        .iter()
        .filter(|q| q.kind == ImpactKind::Answer)
        .collect();
    assert_eq!(got.len(), 1, "{:?}", d.quotes);
    assert_eq!(
        (got[0].record.as_str(), got[0].quote.as_str()),
        (ah.to_string().as_str(), token.as_str())
    );
    assert!(!got[0].erasable);
    assert!(
        d.inside
            .iter()
            .filter(|r| r.record == ah.to_string())
            .all(|r| r.relation == ImpactRelation::Quotes && !r.erasable),
        "no relationship besides the quote"
    );
}

// ---------------------------------------------------------------------------------------------
// the reference fixture: three landings, one of which stays clear of the removed version
// ---------------------------------------------------------------------------------------------

struct Refs {
    dir: tempfile::TempDir,
    r: Rdy,
    prose: BlockId,
    term: BlockId,
    /// The first version of the prose block, the one a removal targets.
    v1: Hash,
    /// Its second version.
    v2: Hash,
    c1: Hash,
    c2: Hash,
    c3: Hash,
    l1: Hash,
    l2: Hash,
    l3: Hash,
    p1: Hash,
    p2: Hash,
    p3: Hash,
}

impl Refs {
    fn target(&self) -> Vec<String> {
        vec![format!("{}@{}", self.prose, self.v1)]
    }
}

fn version(r: &Rdy, b: &BlockId) -> Hash {
    r.read()
        .block(&Scope::View("work".into()), b)
        .unwrap()
        .version_hash()
}

/// Landing 1 creates the prose block (`v1`). Landing 2 edits it (`v2`, based on `v1`). Landing 3
/// creates an independent folder: it names neither version and depends on neither change.
fn refs_project() -> Refs {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w1 = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}
        ]),
    );
    let prose = id(&w1.created["$p"]);
    let term = id(&w1.created["$t"]);
    let v1 = version(&r, &prose);
    let propose = |r: &mut Rdy, m: &str| {
        let pi = r
            .propose(&writer(), "work", m, Some("test summary"))
            .unwrap();
        let landing = h(&r.land(&approver(), &pi.id).unwrap());
        (h(&pi.id), landing)
    };
    let (p1, l1) = propose(&mut r, "seed");
    let w2 = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": prose.to_string(), "base": v1.to_string(), "body": "second"}]),
    );
    let v2 = version(&r, &prose);
    let (p2, l2) = propose(&mut r, "second");
    let w3 = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "folder", "fields": {"name": "other", "title": "Other"}}]),
    );
    let (p3, l3) = propose(&mut r, "third");
    Refs {
        c1: h(&w1.change),
        c2: h(&w2.change),
        c3: h(&w3.change),
        dir,
        r,
        prose,
        term,
        v1,
        v2,
        l1,
        l2,
        l3,
        p1,
        p2,
        p3,
    }
}

/// The carried records of the reference fixture and the rows each must give, by name.
struct Carriers {
    /// (name, kind, hash, whether it references the removed version)
    all: Vec<(&'static str, ImpactKind, Hash, bool)>,
}

fn carriers(f: &mut Refs) -> Carriers {
    let prose = [f.prose.clone()];
    let term = [f.term.clone()];
    let mut all: Vec<(&'static str, ImpactKind, Hash, bool)> = vec![];
    let mut add = |name, kind, hash, hit| all.push((name, kind, hash, hit));
    // assessments: each block list, the suspect pair, the policy version, and nonmatching ones
    let mut tag = 0u8;
    let mut next = |edit: &dyn Fn(&mut Assessment)| {
        tag += 1;
        let mut a = assessment(tag, &term);
        edit(&mut a);
        a
    };
    let list: Vec<(&'static str, bool, Assessment)> = vec![
        ("a scope", true, {
            next(&|a| a.scope.blocks = prose.to_vec())
        }),
        ("a reach", true, next(&|a| a.reach = prose.to_vec())),
        (
            "a tension",
            true,
            next(&|a| a.tensions[0].blocks = prose.to_vec()),
        ),
        (
            "an issue",
            true,
            next(&|a| a.issues[0].block = prose[0].clone()),
        ),
        (
            "a confidence reason",
            true,
            next(&|a| a.confidence.reasons[0].block = prose[0].clone()),
        ),
        (
            "a suspect block",
            true,
            next(&|a| {
                a.suspects = vec![AssessmentSuspect {
                    block: prose[0].clone(),
                    cause: term[0].clone(),
                }]
            }),
        ),
        (
            "a suspect cause",
            true,
            next(&|a| {
                a.suspects = vec![AssessmentSuspect {
                    block: term[0].clone(),
                    cause: prose[0].clone(),
                }]
            }),
        ),
        ("the removed policy version", true, {
            let v1 = f.v1;
            let b = f.prose.clone();
            next(&move |a| {
                a.policy = Some(AssessedPolicy {
                    policy: Some(PolicyRef {
                        block: b.clone(),
                        version: v1,
                    }),
                    verdict: PolicyVerdict::Accepts {
                        rule: "maintainers-approve".into(),
                    },
                })
            })
        }),
        // the same block, another version: the version is the identity
        ("another policy version", false, {
            let v2 = f.v2;
            let b = f.prose.clone();
            next(&move |a| {
                a.policy = Some(AssessedPolicy {
                    policy: Some(PolicyRef {
                        block: b.clone(),
                        version: v2,
                    }),
                    verdict: PolicyVerdict::Accepts {
                        rule: "maintainers-approve".into(),
                    },
                })
            })
        }),
        ("no matching block", false, next(&|_| {})),
        (
            "unrelated suspects",
            false,
            next(&|a| {
                a.suspects = vec![AssessmentSuspect {
                    block: term[0].clone(),
                    cause: id("b-00099"),
                }]
            }),
        ),
    ];
    for (name, hit, a) in list {
        let hash = put(&mut f.r, kinds::ASSESSMENT, &a);
        add(name, ImpactKind::Assessment, hash, hit);
    }
    // disputes: an affected landing, the other affected landing, and the clear one
    let d1 = put(&mut f.r, kinds::DISPUTE, &dispute(f.l1, "d on l1"));
    add(
        "a dispute of an affected landing",
        ImpactKind::Dispute,
        d1,
        true,
    );
    let d2 = put(&mut f.r, kinds::DISPUTE, &dispute(f.l2, "d on l2"));
    add(
        "a dispute of the edit landing",
        ImpactKind::Dispute,
        d2,
        true,
    );
    let d3 = put(&mut f.r, kinds::DISPUTE, &dispute(f.l3, "d on l3"));
    add(
        "a dispute of the clear landing",
        ImpactKind::Dispute,
        d3,
        false,
    );
    // resolutions: by dispute, by correction, and neither
    let r1 = put(&mut f.r, kinds::DISPUTE_RESOLUTION, &resolution(d1, None));
    add(
        "a resolution of an affected dispute",
        ImpactKind::DisputeResolution,
        r1,
        true,
    );
    let r2 = put(
        &mut f.r,
        kinds::DISPUTE_RESOLUTION,
        &resolution(d3, Some(f.l1)),
    );
    add(
        "a resolution with an affected correction",
        ImpactKind::DisputeResolution,
        r2,
        true,
    );
    let r3 = put(
        &mut f.r,
        kinds::DISPUTE_RESOLUTION,
        &resolution(d3, Some(f.l3)),
    );
    add(
        "a resolution with a clear correction",
        ImpactKind::DisputeResolution,
        r3,
        false,
    );
    let r4 = put(&mut f.r, kinds::DISPUTE_RESOLUTION, &resolution(d3, None));
    add(
        "a resolution of a clear dispute",
        ImpactKind::DisputeResolution,
        r4,
        false,
    );
    // check requests: by block, by change, by proposal, and nothing
    let mut q1 = request(1, &prose);
    q1.subject.code = None;
    let q1h = put(&mut f.r, kinds::CHECK_REQUEST, &q1);
    add(
        "a request on the block",
        ImpactKind::CheckRequest,
        q1h,
        true,
    );
    let mut q2 = request(2, &term);
    q2.subject.change = Some(f.c2);
    let q2h = put(&mut f.r, kinds::CHECK_REQUEST, &q2);
    add(
        "a request on an affected change",
        ImpactKind::CheckRequest,
        q2h,
        true,
    );
    let mut q3 = request(3, &term);
    q3.subject.version = Some(f.p2);
    let q3h = put(&mut f.r, kinds::CHECK_REQUEST, &q3);
    add(
        "a request on an affected proposal",
        ImpactKind::CheckRequest,
        q3h,
        true,
    );
    let mut q4 = request(4, &term);
    q4.subject.change = Some(f.c3);
    q4.subject.version = Some(f.p3);
    let q4h = put(&mut f.r, kinds::CHECK_REQUEST, &q4);
    add(
        "a request on a clear change and proposal",
        ImpactKind::CheckRequest,
        q4h,
        false,
    );
    // check reviews: through the request, through a citation, and neither
    let v1 = check_review(q1h, q1.case_hash, Verdict::Pass, &term[0], "");
    let v1h = put(&mut f.r, kinds::REVIEW, &v1);
    add(
        "a pass of an affected request",
        ImpactKind::Review,
        v1h,
        true,
    );
    let v2 = check_review(q2h, q2.case_hash, Verdict::Fail, &term[0], "a neutral note");
    let v2h = put(&mut f.r, kinds::REVIEW, &v2);
    add(
        "a fail of an affected request",
        ImpactKind::Review,
        v2h,
        true,
    );
    let v3 = check_review(
        q4h,
        q4.case_hash,
        Verdict::Concern,
        &prose[0],
        "a neutral note",
    );
    let v3h = put(&mut f.r, kinds::REVIEW, &v3);
    add("a concern citing the block", ImpactKind::Review, v3h, true);
    let v4 = check_review(
        q4h,
        q4.case_hash,
        Verdict::Concern,
        &term[0],
        "a neutral note",
    );
    let v4h = put(&mut f.r, kinds::REVIEW, &v4);
    add(
        "a concern of a clear request",
        ImpactKind::Review,
        v4h,
        false,
    );
    Carriers { all }
}

#[test]
fn ok8_carrier_references_are_closed() {
    let _clock = pin_clock(1_000_000);
    let mut f = refs_project();
    let before = dry(&f.r, &f.target());
    let c = carriers(&mut f);
    let d = dry(&f.r, &f.target());
    // the carriers add References rows for the affected ones, and nothing else
    let kinds_of = |k: ImpactKind| carried_kind(k) || k == ImpactKind::Review;
    let got: Vec<ImpactRow> = d
        .inside
        .iter()
        .filter(|r| kinds_of(r.kind))
        .cloned()
        .collect();
    let want: Vec<ImpactRow> = c
        .all
        .iter()
        .filter(|(_, _, _, hit)| *hit)
        .map(|(_, kind, hash, _)| row(*kind, hash, ImpactRelation::References, false))
        .collect();
    assert!(!want.is_empty());
    for (name, kind, hash, hit) in &c.all {
        let r = row(*kind, hash, ImpactRelation::References, false);
        assert_eq!(got.contains(&r), *hit, "{name}: {r:?}");
    }
    assert_eq!(got.len(), want.len(), "{got:?}");
    assert!(d.quotes.is_empty(), "no carrier quotes the source string");
    // the rest of the report is what it was without them
    let rest = |rep: &RedactionReport| -> Vec<ImpactRow> {
        rep.inside
            .iter()
            .filter(|r| !kinds_of(r.kind))
            .cloned()
            .collect()
    };
    assert_eq!(rest(&d), rest(&before));
    assert_eq!(d.versions, before.versions);
    assert_eq!(d.drop_frames, before.drop_frames);
    assert_eq!(d.drop_bytes, before.drop_bytes);
    // the affected landings, proposals and changes are the ones the references were read from
    for want in [
        row(ImpactKind::Landing, f.l1, ImpactRelation::References, false),
        row(ImpactKind::Landing, f.l2, ImpactRelation::References, false),
        row(
            ImpactKind::Proposal,
            f.p1,
            ImpactRelation::References,
            false,
        ),
        row(
            ImpactKind::Proposal,
            f.p2,
            ImpactRelation::References,
            false,
        ),
        row(ImpactKind::Change, f.c2, ImpactRelation::References, false),
    ] {
        assert!(d.inside.contains(&want), "{want:?}");
    }
    for clear in [f.l3, f.p3, f.c3] {
        assert!(
            !d.inside.iter().any(|r| r.record == clear.to_string()),
            "{clear} is clear of the removed version"
        );
    }
    let _ = f.c1;
}

// ---------------------------------------------------------------------------------------------
// removal, reindex and dump/load keep every carried record
// ---------------------------------------------------------------------------------------------

#[test]
fn ok8_removal_retains_carriers_and_warning() {
    let _clock = pin_clock(1_000_000);
    let mut f = refs_project();
    let c = carriers(&mut f);
    // a record that quotes the source string, so the warning has a copy to report
    let mut q = request(9, &[f.term.clone()]);
    q.reason = Some(format!("Quoted: {SECRET}"));
    let qh = put(&mut f.r, kinds::CHECK_REQUEST, &q);
    let mut held: Vec<(u8, Hash)> = c
        .all
        .iter()
        .map(|(_, kind, hash, _)| {
            let k = match kind {
                ImpactKind::Assessment => kinds::ASSESSMENT,
                ImpactKind::Dispute => kinds::DISPUTE,
                ImpactKind::DisputeResolution => kinds::DISPUTE_RESOLUTION,
                ImpactKind::CheckRequest => kinds::CHECK_REQUEST,
                _ => kinds::REVIEW,
            };
            (k, *hash)
        })
        .collect();
    held.push((kinds::CHECK_REQUEST, qh));
    let bytes = |r: &Rdy| -> Vec<Vec<u8>> {
        held.iter()
            .map(|(k, hash)| r.read().reader().get_kind(*k, hash).unwrap())
            .collect()
    };
    let before = bytes(&f.r);
    let target = f.target();
    // the request: the quote is reported and uneraseable, no carrier span is in the manifest
    let res =
        f.r.redact_in(&writer(), "redact", input(&target, false))
            .unwrap();
    assert!(res.report.quotes.iter().any(|q| q.record == qh.to_string()
        && q.kind == ImpactKind::CheckRequest
        && q.quote == SECRET
        && !q.erasable));
    assert_eq!(
        res.warnings
            .iter()
            .filter(|w| w.code == "W_REDACTION_QUOTES")
            .count(),
        1
    );
    let warning = res
        .warnings
        .iter()
        .find(|w| w.code == "W_REDACTION_QUOTES")
        .unwrap();
    assert_eq!(warning.message, QUOTES_WARNING);
    assert_eq!(warning.fix, "");
    let req =
        f.r.read()
            .reader()
            .redaction_request(&res.redaction.unwrap())
            .unwrap();
    for span in &req.manifest.spans {
        assert!(
            [kinds::BLOCK, kinds::CHANGE, kinds::BLOB_CHUNK].contains(&span.kind),
            "a carried record is never dropped: {span:?}"
        );
    }
    for r in req.manifest.inside.iter().filter(|r| carried_kind(r.kind)) {
        assert!(!r.erasable, "{r:?}");
    }
    // the removal lands
    let pi =
        f.r.propose(
            &writer(),
            "redact",
            "Remove the code",
            Some("Removes the leaked code."),
        )
        .unwrap();
    f.r.land(&approver(), &pi.id).unwrap();
    assert!(
        f.r.read()
            .reader()
            .redaction_of(kinds::BLOCK, &f.v1)
            .unwrap()
            .is_some(),
        "the version is removed"
    );
    assert_eq!(bytes(&f.r), before, "the removal keeps every carried byte");
    // reindex
    f.r.reindex().unwrap();
    assert_eq!(bytes(&f.r), before, "reindex keeps every carried byte");
    // the report of the same content now says it is gone, with the same warning
    let again = dry_run(&f.r.read(), &input(&target, true)).unwrap();
    assert_eq!(
        again
            .warnings
            .iter()
            .find(|w| w.code == "W_REDACTION_QUOTES")
            .unwrap()
            .message,
        QUOTES_WARNING
    );
    // dump and load
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.rdydump");
    f.r.read().dump(&file).unwrap();
    let to = tempfile::tempdir().unwrap();
    let dest = to.path().join("copy");
    std::fs::create_dir(&dest).unwrap();
    rdy_engine::load::load(&file, &dest).unwrap();
    let loaded = Rdy::open(&dest).unwrap();
    assert_eq!(
        bytes(&loaded),
        before,
        "dump and load keep every carried byte"
    );
    drop(loaded);
    let _ = &f.dir;
}

// ---------------------------------------------------------------------------------------------
// the status line
// ---------------------------------------------------------------------------------------------

#[test]
fn ok8_current_support_does_not_rewrite_old_reports() {
    let _clock = pin_clock(1_000_000);
    let mut f = refs_project();
    // an empty scan is Carried too: the reader supports the snapshots, whatever the store holds
    let empty = dry(&f.r, &f.target());
    assert_eq!(empty.assessment, AssessmentSupport::Carried);
    let _ = carriers(&mut f);
    let d = dry(&f.r, &f.target());
    assert_eq!(d.assessment, AssessmentSupport::Carried);
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["assessment"], "carried");
    let res = dry_run(&f.r.read(), &input(&f.target(), true)).unwrap();
    let text = report_text(&res, 8000).unwrap().text;
    assert!(
        text.contains("\nRecorded assessments: scanned.\n"),
        "{text}"
    );
    assert!(!text.contains("not implemented"));
    // the shortened text keeps the same line
    let short = report_text(&res, 600).unwrap();
    assert!(short.truncated);
    assert!(short.text.contains("\nRecorded assessments: scanned.\n"));
    // a report that was made before the carried kinds keeps its status and its text
    let mut old = res.clone();
    old.report.assessment = AssessmentSupport::NotImplemented;
    let old_text = report_text(&old, 8000).unwrap().text;
    assert!(old_text.contains("\nRecorded assessments: not implemented.\n"));
    assert!(!old_text.contains("scanned"));
    assert_eq!(
        serde_json::to_value(&old.report).unwrap()["assessment"],
        "not_implemented"
    );
    // a recorded request binds the hash of its manifest. An old manifest keeps its own hash:
    // the postcard bytes end with the status, and NotImplemented is the first variant
    let rec =
        f.r.redact_in(&writer(), "redact", input(&f.target(), false))
            .unwrap();
    let stored =
        f.r.read()
            .reader()
            .redaction_request(&rec.redaction.unwrap())
            .unwrap();
    assert_eq!(stored.manifest.assessment, AssessmentSupport::Carried);
    let mut buf = vec![0u8; rdy_model::redaction::MAX_RECORD_BYTES];
    let current = stored.manifest.report_hash(&mut buf).unwrap();
    assert_eq!(current, rec.report.report_hash);
    let mut old_manifest = stored.manifest.clone();
    old_manifest.assessment = AssessmentSupport::NotImplemented;
    let old_hash = old_manifest.report_hash(&mut buf).unwrap();
    assert_ne!(old_hash, current, "the status is a hash input");
    let mut bytes = encode_into(&stored.manifest, &mut buf).unwrap().to_vec();
    assert_eq!(bytes.pop(), Some(1), "Carried is the appended variant");
    bytes.push(0);
    assert_eq!(old_hash, Hash::of(&[REPORT_DOMAIN, &bytes].concat()));
    // a request recorded under the current code lands under it: the approval hash is the one
    // the request bound
    let pi =
        f.r.propose(
            &writer(),
            "redact",
            "Remove the code",
            Some("Removes the leaked code."),
        )
        .unwrap();
    f.r.land(&approver(), &pi.id).unwrap();
    assert!(
        f.r.read()
            .reader()
            .redaction_of(kinds::BLOCK, &f.v1)
            .unwrap()
            .is_some()
    );
    // every target removed: the report is still Carried
    let gone = dry(&f.r, &f.target());
    assert_eq!(gone.assessment, AssessmentSupport::Carried);
    let _ = &f.dir;
}

// ---------------------------------------------------------------------------------------------
// bounds
// ---------------------------------------------------------------------------------------------

#[test]
fn ok8_scan_bounds_apply_to_carriers() {
    let _clock = pin_clock(1_000_000);
    let mut f = refs_project();
    let _ = carriers(&mut f);
    let t = f.target();
    let all = {
        let mut out = vec![];
        f.r.read()
            .reader()
            .for_each_record_from(
                rdy_store::segment::Location {
                    segment: 0,
                    offset: 0,
                },
                f.r.store().log_end(),
                |_, frame, _| {
                    out.push(match frame {
                        rdy_store::redaction::StoredFrame::Present { payload, .. } => {
                            37 + payload.len() as u64
                        }
                        rdy_store::redaction::StoredFrame::Dropped { len, .. } => {
                            37 + u64::from(len)
                        }
                    });
                    Ok(())
                },
            )
            .unwrap();
        out
    };
    let frames = all.len() as u64;
    let bytes: u64 = all.iter().sum();
    let full = dry(&f.r, &t);
    let rows = full.inside.len() + full.quotes.len() + full.outside.len() + full.quarantine.len();
    let d = Limits::default();
    let refuse = |lim: Limits| -> RdyError {
        let _g = override_limits_for_tests(lim);
        f.r.read().redaction_report(&input(&t, true)).unwrap_err()
    };
    let passes = |lim: Limits| {
        let _g = override_limits_for_tests(lim);
        f.r.read().redaction_report(&input(&t, true)).unwrap()
    };
    // the frames of the carried records count against the frame bound, exactly
    assert_eq!(passes(Limits { frames, ..d }), full);
    let e = refuse(Limits {
        frames: frames - 1,
        ..d
    });
    assert_eq!(e.code, E_REDACTION_LIMIT, "{e}");
    assert!(e.message.contains("1000000 scanned frames"), "{e}");
    // and their bytes against the byte bound
    assert_eq!(passes(Limits { bytes, ..d }), full);
    let e = refuse(Limits {
        bytes: bytes - 1,
        ..d
    });
    assert_eq!(e.code, E_REDACTION_LIMIT, "{e}");
    assert!(e.message.contains("2147483648 scanned bytes"), "{e}");
    // the rows the carriers add count against the row bound
    assert_eq!(passes(Limits { rows, ..d }), full);
    let e = refuse(Limits {
        rows: rows - 1,
        ..d
    });
    assert_eq!(e.code, E_REDACTION_LIMIT, "{e}");
    assert!(e.message.contains("100000 report rows"), "{e}");
    // a carrier holds no field container, so the depth bound reads it whole: no record is
    // reported as too deep, and the rows are the same
    let shallow = passes(Limits { levels: 1, ..d });
    assert_eq!(shallow, full);
    assert!(
        !shallow
            .warnings
            .iter()
            .any(|w| w.message.contains("nest deeper")),
        "{:?}",
        shallow.warnings
    );
}
