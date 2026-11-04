//! The M3 gate's durable-records corpus (Task 4): one small disposable project, built through
//! the engine and the existing fixture writers, that holds a PLAN chain with its seal, an older
//! method 1 ASSESSMENT beside a method 2 one, a policy landing, a relayed ANSWER, DISPUTEs and
//! their resolutions, a CHECK_REQUEST, Check REVIEWs and an accepted verdict, and a person's
//! approved removal of a claim version and a blob whose text an assessment, a check request, a
//! review and an answer quote. Its full dump holds a ready version's method 2 assessment and
//! the baseline features only. Increment 1 holds the first use of the `view_replay`, `dispute`
//! and `check_request` features, the plan chain and the relayed answer; increment 2 the other
//! disputes, the verdicts, the quoting records and the removal.
//!
//! The store and io test binaries both use this module (the io one by `#[path]`), and each uses
//! only some of its parts.
#![allow(dead_code)]

use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::redaction::RedactionReport;
use rdy_engine::{Rdy, ReadState, Scope, Session};
use rdy_model::redaction::ImpactKind;
use rdy_model::{BlockId, Hash, Role, Verdict};
use rdy_qa::every_kind::{self, CarriedFixture, PlanFixture};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::{ag, created, put};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The body of the claim the removal selects: the assessment, the check request, the review and
/// the answer quote it whole.
pub const CANARY: &str = "Osprey gate canary.";
/// The blob the removal selects.
pub const BLOB: &[u8] = b"mg4 removable blob bytes";
/// The segment size of the corpus's store: its log rolls.
pub const SEGMENT_MAX: u64 = 4096;

/// A built corpus: the closed source project at `root`, its dump chain, the source's full dump
/// at the end of increment 1 (`prefix`) and the records it holds.
pub struct Corpus {
    pub dir: tempfile::TempDir,
    pub dumps: tempfile::TempDir,
    pub root: PathBuf,
    pub full: PathBuf,
    pub inc1: PathBuf,
    pub inc2: PathBuf,
    /// The source's full dump at the end of increment 1.
    pub prefix: PathBuf,
    /// [`facts`] of the source at the end of increment 1.
    pub prefix_facts: Value,
    /// The store features at the end of the full dump, of increment 1 and of increment 2.
    pub features: [Value; 3],
    pub section: BlockId,
    /// The claim the carried records and checks name.
    pub subject: BlockId,
    pub carried: CarriedFixture,
    pub plan: PlanFixture,
    /// The method 2 assessment of a ready version, and that version's landing.
    pub method2: Hash,
    pub method2_landing: Hash,
    /// The person's relayed answer, whose note quotes the canary.
    pub relayed: Hash,
    pub canary: BlockId,
    pub canary_version: Hash,
    pub blob: Hash,
    /// The records that quote the canary: their impact kind, object kind, hash and stored bytes
    /// before the removal.
    pub quoted: Vec<(ImpactKind, u8, Hash, Vec<u8>)>,
    /// The removal request's report.
    pub report: RedactionReport,
}

/// The agent that relays the person's words from chat.
pub fn relaying_agent() -> Session {
    Session::agent("agent:relay".into(), Some("chat".into()), Role::Writer).unwrap()
}

/// The person's words `quote`, relayed from chat session `session_ref`.
pub fn relay_words(quote: &str, session_ref: &str) -> RelayInput {
    RelayInput {
        evidence: Some(quote.into()),
        question: Some("Shall I record your answer?".into()),
        session_ref: Some(session_ref.into()),
        ..Default::default()
    }
}

/// The person's approve item of proposal version `r#ref` in their queue.
pub fn approve_item(r: &Rdy, r#ref: &str) -> rdy_engine::pending::Item {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    q.items
        .iter()
        .find(|i| i.kind == ItemKind::Approve && i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("the approve item {ref}: {q:#?}"))
        .clone()
}

fn seal(r: &mut Rdy) {
    r.seal_open().unwrap();
    r.invalidate();
}

/// Builds the corpus (see the module).
pub fn build() -> Corpus {
    let (dir, mut r) = qa_project();
    let root = dir.path().to_path_buf();
    let dumps_dir = tempfile::tempdir().unwrap();
    let dumps = dumps_dir.path().to_path_buf();
    r.store_mut().set_segment_max(SEGMENT_MAX);

    // before the full dump: a doc with the subject and the canary claims, landed, and the blob
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "mg4", "title": "MG4", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Records"}},
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "The gate subject holds."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "after": "$k", "body": CANARY}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let (section, subject, canary) = (created(&w, "$s"), created(&w, "$k"), created(&w, "$c"));
    let blob = r.store_mut().put_blob(BLOB).unwrap();
    seal(&mut r);
    let full = dumps.join("full.rdydump");
    let s0 = r.read().dump(&full).unwrap();
    let f0 = features_of(&r.read());

    // increment 1: the carried stage (a replay with its briefing, the method 1 Accepts and
    // NeedsPerson assessments, the policy landing, an open dispute and a check request), the plan
    // chain and its seal, and a ready version (its method 2 assessment) that the person snoozes
    // through a relay, quoting the canary, and then lands
    let carried = every_kind::add_carried_fixture_records(&mut r);
    let plan = every_kind::add_plan_fixture_records(&mut r, &section, &subject);
    seal(&mut r);
    put(
        &mut r,
        "mg4-m2",
        json!([{"op": "create", "type": "claim", "parent": section.to_string(), "body": "A ready claim."}]),
    );
    let p = r
        .propose(&ag("mg4-m2"), "mg4-m2", "mg4-m2", Some(TEST_SUMMARY))
        .unwrap();
    let version: Hash = p.id.parse().unwrap();
    let method2 = {
        let st = r.read();
        st.reader()
            .assessment_of_proposal(&version, st.seq())
            .unwrap()
            .expect("a ready version stores its assessment")
    };
    let item = approve_item(&r, &p.r#ref);
    let later = r
        .answer(
            &relaying_agent(),
            PERSON,
            AnswerRequest {
                r#ref: item.r#ref.clone(),
                option: "later".into(),
                card_hash: Some(item.card_hash.clone()),
                note: Some(format!("Ask again once {CANARY}")),
                relay: relay_words("Not yet.", "chat-mg4"),
                brief: None,
            },
        )
        .unwrap();
    let relayed: Hash = later
        .answer
        .expect("the snooze writes an ANSWER")
        .parse()
        .unwrap();
    let method2_landing: Hash = r.land(&approver(), &p.id).unwrap().parse().unwrap();
    seal(&mut r);
    let inc1 = dumps.join("inc1.rdydump");
    let s1 = r.read().dump_after(&s0.chain_end(), &inc1).unwrap();
    let prefix = dumps.join("prefix.rdydump");
    r.read().dump(&prefix).unwrap();
    let prefix_facts = facts(&r);
    let f1 = features_of(&r.read());

    // increment 2: three disputes and the Check verdicts, the resolutions and the accepted
    // verdict, the records that quote the canary, and the person's approved removal of the
    // canary's version and the blob
    let mut carried = carried;
    let first = r
        .read()
        .landings()
        .unwrap()
        .last()
        .map(|(h, _)| *h)
        .unwrap();
    let disputed = [carried.landing, method2_landing, first];
    every_kind::dispute_carried_fixture(&mut r, &mut carried, disputed);
    every_kind::resolve_carried_fixture(&mut r, &carried);
    let canary_version: Hash = rdy_qa::fixtures::version_of(&r, &Scope::Canon, &canary.to_string())
        .parse()
        .unwrap();
    let quoted = quote_records(&mut r, &carried, &canary, relayed);
    let rq = ag("rq-mg4");
    let res = r
        .redact_in(
            &rq,
            "rq-mg4",
            RedactInput {
                targets: vec![format!("{canary}@{canary_version}"), format!("blob:{blob}")],
                reason_class: "personal".into(),
                reason: "the claim names a person".into(),
                dry_run: false,
            },
        )
        .unwrap();
    res.redaction.expect("the request is recorded");
    let p = r
        .propose(&rq, "rq-mg4", "rq-mg4", Some("Removes the canary claim."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    seal(&mut r);
    let inc2 = dumps.join("inc2.rdydump");
    r.read().dump_after(&s1.chain_end(), &inc2).unwrap();
    let f2 = features_of(&r.read());
    r.close().unwrap();
    Corpus {
        dir,
        dumps: dumps_dir,
        root,
        full,
        inc1,
        inc2,
        prefix,
        prefix_facts,
        features: [f0, f1, f2],
        section,
        subject,
        carried,
        plan,
        method2,
        method2_landing,
        relayed,
        canary,
        canary_version,
        blob,
        quoted,
        report: res.report,
    }
}

/// Writes an assessment, a check request and its Check review that quote the canary, through
/// the store's writers, and returns them with the relayed answer and each one's stored bytes.
fn quote_records(
    r: &mut Rdy,
    fx: &CarriedFixture,
    canary: &BlockId,
    answer: Hash,
) -> Vec<(ImpactKind, u8, Hash, Vec<u8>)> {
    let dismissal = r
        .store_mut()
        .put(
            kinds::REVIEW,
            &every_kind::carried_tension_review(vec![canary.clone()]),
        )
        .unwrap();
    let mut a =
        every_kind::carried_assessment(fx.proposal, None, None, dismissal, vec![canary.clone()]);
    a.card.facts.push(format!("the claim reads {CANARY}"));
    let assessment = r.store_mut().put(kinds::ASSESSMENT, &a).unwrap();
    let q = every_kind::carried_check_request(fx.replay, fx.proposal, canary);
    let q = rdy_model::check_request::CheckRequest {
        case_hash: Hash::of(b"mg4 quote case"),
        reason: Some(format!("check {CANARY}")),
        ..q
    };
    let request = r.store_mut().put_check_request(&q).unwrap();
    let mut review =
        every_kind::carried_check_review(request, q.case_hash, Verdict::Concern, canary);
    review.reason = format!("{CANARY} has no source");
    let review = r.store_mut().put_check_review(&review).unwrap();
    seal(r);
    let st = r.read();
    [
        (ImpactKind::Assessment, kinds::ASSESSMENT, assessment),
        (ImpactKind::CheckRequest, kinds::CHECK_REQUEST, request),
        (ImpactKind::Review, kinds::REVIEW, review),
        (ImpactKind::Answer, kinds::ANSWER, answer),
    ]
    .into_iter()
    .map(|(i, k, h)| (i, k, h, st.reader().get_kind(k, &h).unwrap()))
    .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The kinds whose every frame [`facts_of`] keeps whole: the M3 records and the records they
/// tie to.
const KEPT: [u8; 9] = [
    kinds::PLAN,
    kinds::ASSESSMENT,
    kinds::DISPUTE,
    kinds::DISPUTE_RESOLUTION,
    kinds::CHECK_REQUEST,
    kinds::REVIEW,
    kinds::ANSWER,
    kinds::LANDING,
    kinds::PROPOSAL,
];

/// The store features of `st`, as its manifest reads.
pub fn features_of(st: &ReadState) -> Value {
    serde_json::to_value(st.reader().store_features().unwrap()).unwrap()
}

/// What a reopen, a reindex, a load and a follower must read the same of `r`: [`facts_of`].
pub fn facts(r: &Rdy) -> Value {
    facts_of(&r.read())
}

/// The durable M3 facts of `st`, by pure reads: every frame of a [`KEPT`] kind with its place,
/// hash and payload (a dropped one as dropped), the count of every other kind, every ref, the
/// first briefed change's place, each landing's ties (its assessment, policy, assurance, relay,
/// seal, disputes and their resolutions), every assessment's method and proposal, and the
/// features.
pub fn facts_of(st: &ReadState) -> Value {
    let reader = st.reader();
    let end = st.seq();
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    let mut kept: BTreeMap<String, String> = BTreeMap::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut assessments = vec![];
    reader
        .for_each_record_from(origin, end, |at, f, _| {
            let place = format!("{}.{}", at.segment, at.offset);
            match f {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => {
                    let logical = rdy_store::codec::logical_kind(kind);
                    if KEPT.contains(&logical) {
                        kept.insert(format!("{place} {logical}:{hash}"), hex(payload));
                    } else if kind != kinds::BATCH_COMMIT {
                        *counts.entry(kind.to_string()).or_default() += 1;
                    }
                    if rdy_store::codec::logical_kind(kind) == kinds::ASSESSMENT {
                        assessments.push(hash);
                    }
                }
                StoredFrame::Dropped { kind, hash, .. } => {
                    let logical = rdy_store::codec::logical_kind(kind);
                    kept.insert(format!("{place} {logical}:{hash}"), "dropped".into());
                }
            }
            Ok(())
        })
        .unwrap();
    let assessments: BTreeMap<String, Value> = assessments
        .into_iter()
        .map(|h| {
            let a = reader.assessment(&h).unwrap();
            (
                h.to_string(),
                json!({"method": a.method, "proposal": a.proposal.to_string()}),
            )
        })
        .collect();
    let refs: BTreeMap<String, String> = reader
        .refs_with_prefix("")
        .unwrap()
        .into_iter()
        .map(|(n, v)| (n, hex(&v)))
        .collect();
    let mut landings = vec![];
    for (h, l) in st.landings().unwrap() {
        let disputes: Vec<Value> = reader
            .disputes_of(&h, end)
            .unwrap()
            .into_iter()
            .map(|d| {
                let res = reader.resolution_of(&d, end).unwrap();
                json!({"dispute": d.to_string(), "resolution": res.map(|x| x.to_string())})
            })
            .collect();
        let seal = reader.plan_of_landing(&h, end).unwrap();
        let assessed = match l.approved_version {
            Some(v) => reader
                .assessment_of_proposal(&v, end)
                .unwrap()
                .map(|x| x.to_string()),
            None => None,
        };
        landings.push(json!({
            "landing": h.to_string(),
            "assessment": l.assessment.map(|x| x.to_string()),
            "policy": l.policy.as_ref().map(|p| format!("{}@{}", p.block, p.version)),
            "assurance": format!("{:?}", l.assurance),
            "relayed_by": l.relayed_by.as_ref().map(|a| a.actor.clone()),
            "checks": l.checks.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
            "seal": seal.map(|x| x.to_string()),
            "sealed": seal.map(|x| format!("{:?}", reader.plan(&x).unwrap())),
            "version_assessment": assessed,
            "disputes": disputes,
        }));
    }
    let briefed = reader
        .first_briefed_change()
        .unwrap()
        .map(|l| format!("{}.{}", l.segment, l.offset));
    json!({
        "kept": kept,
        "counts": counts,
        "assessments": assessments,
        "refs": refs,
        "landings": landings,
        "first_briefed": briefed,
        "features": features_of(st),
        "end": format!("{}.{}", end.segment, end.offset),
    })
}

/// Checks that `facts` (of the built corpus `c`) holds what the corpus promises, so that a later
/// equality is not an equality of empty reads.
pub fn assert_holds_every_m3_record(c: &Corpus, facts: &Value) {
    let kept = facts["kept"].as_object().unwrap();
    for kind in KEPT {
        assert!(
            kept.keys().any(|k| k.contains(&format!(" {kind}:"))),
            "a frame of kind {kind}: {:#?}",
            kept.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        kept.values().any(|v| v == "dropped"),
        "the removal dropped payloads"
    );
    let methods: Vec<u64> = facts["assessments"]
        .as_object()
        .unwrap()
        .values()
        .map(|a| a["method"].as_u64().unwrap())
        .collect();
    assert!(
        methods.contains(&1),
        "an older method 1 assessment: {methods:?}"
    );
    assert!(methods.contains(&2), "a method 2 assessment: {methods:?}");
    assert_eq!(
        facts["assessments"][c.method2.to_string()]["method"],
        2,
        "the ready version's assessment"
    );
    assert!(facts["first_briefed"].is_string(), "a briefed change");
    let landings = facts["landings"].as_array().unwrap();
    let find = |h: &Hash| {
        landings
            .iter()
            .find(|l| l["landing"] == h.to_string())
            .unwrap_or_else(|| panic!("landing {h}"))
    };
    let policy = find(&c.carried.landing);
    assert_eq!(policy["assurance"], "Policy");
    assert_eq!(policy["assessment"], c.carried.assessment.to_string());
    assert!(policy["policy"].is_string());
    assert_eq!(policy["disputes"].as_array().unwrap().len(), 2, "{policy}");
    assert_eq!(
        find(&c.plan.landing)["seal"],
        c.plan.seal.to_string(),
        "the sealed plan"
    );
    assert!(
        landings
            .iter()
            .flat_map(|l| l["disputes"].as_array().unwrap())
            .any(|d| d["resolution"].is_string()),
        "a resolved dispute"
    );
    let refs = facts["refs"].as_object().unwrap();
    assert!(refs.keys().any(|k| k.starts_with("check/")), "a check ref");
    assert!(refs.keys().any(|k| k.starts_with("plan/")), "a plan ref");
    let verdict = format!("answer/{}", rdy_store::carried_records::VERDICT_PREFIX);
    assert!(
        refs.keys().any(|k| k.starts_with(&verdict)),
        "the accepted verdict's answer: {:#?}",
        refs.keys().collect::<Vec<_>>()
    );
    let dispute = format!("answer/{}", rdy_store::carried_records::DISPUTE_PREFIX);
    assert!(
        refs.keys().any(|k| k.starts_with(&dispute)),
        "a dispute answer"
    );
    assert!(
        facts["kept"]
            .as_object()
            .unwrap()
            .keys()
            .any(|k| k.ends_with(&format!("{}:{}", kinds::ANSWER, c.relayed))),
        "the relayed answer"
    );
}

/// The bytes of a full dump of `r`, written to `out`.
pub fn dump_bytes(r: &Rdy, out: &Path) -> Vec<u8> {
    r.read().dump(out).unwrap();
    std::fs::read(out).unwrap()
}

/// The segment files of the project at `root`, by name.
pub fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}
