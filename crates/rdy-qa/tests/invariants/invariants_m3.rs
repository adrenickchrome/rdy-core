//! mg1: the M3 invariants (`rdy_qa::m3_audit`) read stored M3 records and the landings observed
//! between held snapshots, independently of readiness. Positive fixtures go through the normal
//! APIs; each negative witness changes a copy of the fact projection, never the log.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::readiness::readiness_calls_for_tests;
use rdy_engine::{ProposalInfo, Rdy, Scope, Session, WriteRequest};
use rdy_model::assessment::PolicyRef;
use rdy_model::plan::{PlanAct, PlanAnswer, PlanKind, PlanOutcome, SealedItem};
use rdy_model::{ActorRun, Assurance, BlockId, EvidenceKind, Hash, Justification, Role};
use rdy_qa::edit_loop::{brief_of, stable_spec};
use rdy_qa::fixtures::{approver, land, parse_test_ops, project, writer};
use rdy_qa::intake::{
    PERSON, TEST_SUMMARY, accept_deferrals, answer_open_items, batch_project,
    confirm_before_landing, confirm_exceptions, pending, ready_and_not_ready,
};
use rdy_qa::invariants::{Violation, check_all};
use rdy_qa::m3_audit::{
    Hold, HoldKind, M3Facts, ObservedLanding, Shown, SourceFact, SubjectFact, TemporalFacts,
    audit_m3, check_facts, check_m3, check_temporal, holds_of,
};
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::{ag, j_typo, put, put_j, upd};
use rdy_store::segment::Location;
use serde_json::json;

fn rules(v: &[Violation]) -> Vec<&'static str> {
    let mut x: Vec<&'static str> = v.iter().map(|v| v.rule).collect();
    x.sort();
    x
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn other(tag: &str) -> Hash {
    Hash::of(tag.as_bytes())
}

/// The relaying agent over MCP.
fn relay() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).expect("agent")
}

/// The person's "yes" for `option` on item `r#ref`, relayed, bound to `card`.
fn relayed(r: &mut Rdy, r#ref: &str, option: &str, card: &str) {
    r.answer(
        &relay(),
        PERSON,
        AnswerRequest {
            r#ref: r#ref.into(),
            option: option.into(),
            card_hash: Some(card.into()),
            note: None,
            relay: RelayInput {
                relayed_by: None,
                evidence: Some("Yes.".into()),
                question: Some("Shall I record your answer?".into()),
                session_ref: Some("chat-7".into()),
                kind: None,
            },
            brief: None,
        },
    )
    .unwrap_or_else(|e| panic!("relay {option} on {ref}: {e}"));
}

/// The queue's card hash of item `r#ref`.
fn card_of(r: &Rdy, r#ref: &str) -> String {
    pending(r)
        .items
        .into_iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("{ref} in the queue"))
        .card_hash
}

/// A stable spec, then a claim added to its stable section with a briefing, landed. The landing.
fn briefed_stable_landing() -> (tempfile::TempDir, Rdy, Hash) {
    let (d, mut r) = project();
    let s = stable_spec(&mut r);
    let tok = brief_of(&r, &Scope::Canon, &[&s.section]).base.token;
    // an editorial justification: an unjustified change to stable content blocks
    r.write_briefed(
        &writer(),
        WriteRequest {
            view: "add".into(),
            ops: parse_test_ops(json!([{"op": "create", "type": "claim",
                "parent": s.section.to_string(), "body": "D holds too."}]))
            .unwrap(),
            justification: j_typo(),
            message: String::new(),
        },
        Some(tok),
    )
    .expect("a briefed write to stable content");
    let l = h(&land(&mut r, "add").expect("land add"));
    (d, r, l)
}

/// A version ready to approve (the `ready` version of `ready_and_not_ready`), approved by a
/// relay bound to its card: the snapshots before and after, the landing and what the person saw.
fn relayed_landing() -> (
    tempfile::TempDir,
    Rdy,
    ProposalInfo,
    ObservedLanding,
    Facts2,
) {
    let (d, mut r, m) = ready_and_not_ready();
    let card = card_of(&r, &m.ready.r#ref);
    let before = r.read();
    relayed(&mut r, &m.ready.r#ref, "approve", &card);
    let after = r.read();
    let landing = after.canon_head().expect("a landing");
    let observed = ObservedLanding {
        landing,
        shown: Some(Shown {
            version: h(&m.ready.id),
            card_hash: Some(h(&card)),
        }),
    };
    let t = TemporalFacts::read(&before, &after, &observed).expect("the temporal facts");
    let audit = audit_m3(&before, &after, &observed);
    (d, r, m.ready, observed, Facts2 { t, audit })
}

struct Facts2 {
    t: TemporalFacts,
    audit: Vec<Violation>,
}

#[test]
fn mg1_stable_landings_have_briefs_or_a_named_exemption() {
    let (_d, r, l) = briefed_stable_landing();
    assert_eq!(check_m3(&r), vec![], "a briefed stable landing holds");
    check_all(&r).expect("check_all runs the durable M3 rules");
    let facts = M3Facts::read(&r.read()).unwrap();
    let c = &facts.landing(&l).expect("an M3 landing").changes[0];
    assert!(c.briefing.is_some(), "the change records its briefing");
    assert!(!c.stable.is_empty(), "it touches stable content: {c:?}");
    // every landing of the fixture records an assessment; the ones before the first briefing
    // are exempt by (c), and they hold
    assert!(facts.landings.len() >= 3, "{:?}", facts.landings.len());
    let first = facts.first_briefed.expect("a briefed change");
    assert!(
        facts.landings[0]
            .changes
            .iter()
            .all(|c| c.at < first && c.briefing.is_none()),
        "the spec landed before the first briefing"
    );

    let with = |f: &dyn Fn(&mut rdy_qa::m3_audit::ChangeFact, &mut Option<Location>)| {
        let mut x = facts.clone();
        let mut first = x.first_briefed;
        f(&mut x.landing_mut(&l).unwrap().changes[0], &mut first);
        x.first_briefed = first;
        check_facts(&x)
    };
    // the missing link
    let missing = with(&|c, _| c.briefing = None);
    assert_eq!(rules(&missing), vec!["m3_briefing"], "{missing:?}");
    assert_eq!(missing[0].block.as_ref(), c.stable.first());
    // the exemptions of the built rule
    assert_eq!(
        with(&|c, _| {
            c.briefing = None;
            c.imported = true;
        }),
        vec![],
        "import"
    );
    assert_eq!(
        with(&|c, f| {
            c.briefing = None;
            *f = Some(Location {
                segment: u32::MAX,
                offset: 0,
            });
        }),
        vec![],
        "before the first briefing"
    );
    assert_eq!(
        with(&|c, _| {
            c.briefing = None;
            c.stable.clear();
        }),
        vec![],
        "no stable state"
    );
    // a replay chain: an imported source at step 64 exempts; at step 65 it does not
    let chain = |n: usize, at: usize| -> Vec<SourceFact> {
        (0..n)
            .map(|i| SourceFact {
                change: other(&format!("source {i}")),
                imported: i + 1 == at,
                at: Location {
                    segment: u32::MAX,
                    offset: i as u64,
                },
            })
            .collect()
    };
    let replay = |n: usize, at: usize| {
        with(&|c, _| {
            c.briefing = None;
            c.replay = true;
            c.sources = chain(n, at);
        })
    };
    assert_eq!(
        replay(64, 64),
        vec![],
        "a chain of 64 steps reaches the import"
    );
    assert_eq!(
        rules(&replay(65, 65)),
        vec!["m3_briefing"],
        "a chain over 64 steps"
    );
    // a cycle ends the chain before the exempt source
    let cycle = with(&|c, _| {
        c.briefing = None;
        c.replay = true;
        let a = SourceFact {
            change: other("a"),
            imported: false,
            at: Location {
                segment: u32::MAX,
                offset: 0,
            },
        };
        c.sources = vec![
            a.clone(),
            SourceFact {
                imported: true,
                ..a
            },
        ];
    });
    assert_eq!(rules(&cycle), vec!["m3_briefing"], "a cycle");
    // a replay link with no source held is not exempt
    assert_eq!(
        rules(&with(&|c, _| {
            c.briefing = None;
            c.replay = true;
            c.sources.clear();
        })),
        vec!["m3_briefing"]
    );
}

#[test]
fn mg1_landing_binds_the_assessed_version_and_shown_card() {
    let (_d, r, ready, observed, f) = relayed_landing();
    assert_eq!(f.audit, vec![], "the relayed landing holds");
    assert_eq!(check_m3(&r), vec![]);
    let l = &f.t.landing;
    let a = l.assessment.as_ref().expect("its assessment");
    assert_eq!(l.record.approved_version, Some(h(&ready.id)));
    assert_eq!(a.proposal, h(&ready.id));
    assert_eq!(l.record.assessment, l.bound_assessment);
    let shown = observed.shown.clone().unwrap();
    let ev = l.record.evidence.as_ref().expect("evidence");
    assert_eq!(
        (ev.kind, ev.card_hash),
        (EvidenceKind::Card, shown.card_hash)
    );
    assert_eq!(
        Some(a.card.hash),
        shown.card_hash,
        "no verdict: the card is the assessment's"
    );

    let facts = M3Facts::read(&r.read()).unwrap();
    let with = |f: &dyn Fn(&mut rdy_qa::m3_audit::LandingFact)| {
        let mut x = facts.clone();
        f(x.landing_mut(&observed.landing).unwrap());
        check_facts(&x)
    };
    // the wrong version
    let version = with(&|l| l.record.approved_version = Some(other("v")));
    assert!(rules(&version).contains(&"m3_version"), "{version:?}");
    // the wrong assessment: not of the version's newest record, and not these bytes
    let wrong = with(&|l| l.record.assessment = Some(other("a")));
    assert_eq!(
        rules(&wrong),
        vec!["m3_assessment", "m3_assessment"],
        "{wrong:?}"
    );
    // a later method cannot rewrite the old measurement's bytes
    let method = with(&|l| l.assessment.as_mut().unwrap().method += 1);
    assert_eq!(rules(&method), vec!["m3_assessment"], "{method:?}");
    let card = with(&|l| {
        l.assessment
            .as_mut()
            .unwrap()
            .card
            .facts
            .push("a new fact".into())
    });
    assert_eq!(rules(&card), vec!["m3_assessment"], "{card:?}");

    // what the person saw, from the journal
    let saw = |s: Shown| {
        let mut t = f.t.clone();
        t.shown = Some(s);
        check_temporal(&t)
    };
    let hash = saw(Shown {
        card_hash: Some(other("card")),
        ..shown.clone()
    });
    assert_eq!(rules(&hash), vec!["m3_card"], "{hash:?}");
    let seen = saw(Shown {
        version: other("v"),
        ..shown.clone()
    });
    assert_eq!(rules(&seen), vec!["m3_card"], "{seen:?}");
    let none = saw(Shown {
        card_hash: None,
        ..shown
    });
    assert_eq!(rules(&none), vec!["m3_card"], "{none:?}");
}

#[test]
fn mg1_stable_plan_and_tension_answers_are_accounted_for() {
    let (_d, mut r, b) = batch_project();
    let version = r.resolve_proposal(&b.r#ref).unwrap().version.to_string();
    accept_deferrals(&mut r, &approver(), &version, &b.r#ref);
    confirm_exceptions(&mut r, &approver(), &b.r#ref);
    let l = h(&r.land(&approver(), &b.r#ref).expect("the batch lands"));
    assert_eq!(check_m3(&r), vec![], "the batch's seal holds");
    let facts = M3Facts::read(&r.read()).unwrap();
    let seal = facts.landing(&l).unwrap().seal.clone().expect("a seal");
    let s = seal.record.seal.as_ref().unwrap();
    let tension = s
        .items
        .iter()
        .find(|i| i.kind == PlanKind::Tension)
        .expect("the advised tension's item")
        .clone();
    assert_eq!(
        tension.outcome,
        PlanOutcome::Confirmed,
        "the author answered it"
    );
    // a draft item stays open: drafts keep their exception
    assert!(
        s.items
            .iter()
            .filter(|i| i.outcome == PlanOutcome::Open)
            .all(|i| seal.subjects.get(&i.subject).is_none_or(|s| !s.stable)),
        "{s:#?}"
    );

    let with = |f: &dyn Fn(&mut rdy_qa::m3_audit::SealFact)| {
        let mut x = facts.clone();
        f(x.landing_mut(&l).unwrap().seal.as_mut().unwrap());
        check_facts(&x)
    };
    // the missing advisory answer
    let unanswered = with(&|s| s.record.answers.retain(|a| a.item != tension.id));
    assert_eq!(rules(&unanswered), vec!["m3_plan"], "{unanswered:?}");
    let open = with(&|s| {
        let i = s
            .record
            .seal
            .as_mut()
            .unwrap()
            .items
            .iter_mut()
            .find(|i| i.id == tension.id);
        i.unwrap().outcome = PlanOutcome::Open;
    });
    assert_eq!(rules(&open), vec!["m3_plan"], "{open:?}");

    // a synthetic item on a subject no sealed item names
    let subject = "c-SYNTH".to_string();
    let item = |kind: PlanKind, outcome: PlanOutcome| SealedItem {
        id: other(&format!("{kind:?} {outcome:?}")),
        kind,
        subject: subject.clone(),
        cause: b.member.clone(),
        edge: "decided_by".into(),
        outcome,
        answer: None,
    };
    let content = other("content of C");
    let place =
        |s: &mut rdy_qa::m3_audit::SealFact, i: SealedItem, stable: bool, answer: Option<Hash>| {
            s.subjects.insert(
                subject.clone(),
                SubjectFact {
                    stable,
                    content: Some(content),
                    decision: false,
                },
            );
            if let Some(seen) = answer {
                s.record.answers.push(PlanAnswer {
                    item: i.id,
                    act: match i.outcome {
                        PlanOutcome::Done => PlanAct::Done,
                        PlanOutcome::Deferred => PlanAct::Defer,
                        _ => PlanAct::Confirm,
                    },
                    reason: Some("checked".into()),
                    subject: Some(seen),
                    cause: other("cause"),
                    by: writer().provenance(),
                });
            }
            s.record.seal.as_mut().unwrap().items.push(i);
        };
    // an open required item on stable content; drafts and update_impl keep their exceptions
    let required = with(&|s| place(s, item(PlanKind::Update, PlanOutcome::Open), true, None));
    assert_eq!(rules(&required), vec!["m3_plan"], "{required:?}");
    assert_eq!(
        with(&|s| place(s, item(PlanKind::Update, PlanOutcome::Open), false, None)),
        vec![]
    );
    assert_eq!(
        with(&|s| place(
            s,
            item(PlanKind::UpdateImplementation, PlanOutcome::Open),
            true,
            None
        )),
        vec![]
    );
    // a confirm answer binds the subject's content: one given before it changed lapsed
    let ok = with(&|s| {
        place(
            s,
            item(PlanKind::Confirm, PlanOutcome::Confirmed),
            true,
            Some(content),
        )
    });
    assert_eq!(ok, vec![]);
    let lapsed = with(&|s| {
        place(
            s,
            item(PlanKind::Confirm, PlanOutcome::Confirmed),
            true,
            Some(other("old C")),
        )
    });
    assert_eq!(rules(&lapsed), vec!["m3_plan"], "{lapsed:?}");
    let none = with(&|s| {
        place(
            s,
            item(PlanKind::Confirm, PlanOutcome::Confirmed),
            true,
            None,
        )
    });
    assert_eq!(rules(&none), vec!["m3_plan"], "{none:?}");
    // a deferred non-code item names the accepted deferral card
    let deferred = with(&|s| {
        place(
            s,
            item(PlanKind::Update, PlanOutcome::Deferred),
            true,
            Some(content),
        )
    });
    assert_eq!(rules(&deferred), vec!["m3_plan"], "{deferred:?}");
}

#[test]
fn mg1_landing_has_no_intervening_reach_or_person_hold() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = rdy_qa::fixtures::seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A new claim."}]),
    );
    answer_open_items(&mut r, &writer(), "a");
    let p = r.propose(&writer(), "a", "a", Some(TEST_SUMMARY)).unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    // an unrelated landing after the assessment
    put(
        &mut r,
        "u",
        json!([{"op": "create", "type": "doc", "fields": {"name": "elsewhere", "title": "Elsewhere", "doc_type": "overview"}}]),
    );
    let unrelated = h(&land(&mut r, "u").unwrap());
    confirm_before_landing(&mut r, &approver(), &p);
    let before = r.read();
    let l = h(&r
        .land(&approver(), &p.id)
        .expect("an unrelated landing does not move it"));
    let after = r.read();
    let observed = ObservedLanding {
        landing: l,
        shown: None,
    };
    assert_eq!(
        audit_m3(&before, &after, &observed),
        vec![],
        "an unrelated landing is allowed"
    );
    let t = TemporalFacts::read(&before, &after, &observed).unwrap();
    assert_eq!(
        t.since.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
        vec![unrelated]
    );
    assert!(t.holds.is_empty());
    let a = t.landing.assessment.clone().unwrap();

    let with = |f: &dyn Fn(&mut TemporalFacts)| {
        let mut x = t.clone();
        f(&mut x);
        check_temporal(&x)
    };
    let reach = with(&|x| {
        x.since[0].1.insert(a.reach[0].clone());
    });
    assert_eq!(rules(&reach), vec!["m3_fresh"], "{reach:?}");
    assert_eq!(
        rules(&with(&|x| x.pack_digest = other("pack"))),
        vec!["m3_fresh"]
    );
    assert_eq!(
        rules(&with(&|x| x.config_digest = other("config"))),
        vec!["m3_fresh"]
    );
    let policy = with(&|x| {
        x.live_policy = Some(PolicyRef {
            block: BlockId::parse(&section).unwrap(),
            version: other("p"),
        })
    });
    assert_eq!(rules(&policy), vec!["m3_fresh"], "{policy:?}");
    // each of the five later refusals
    let held = with(&|x| {
        for kind in [
            HoldKind::RejectedDecision,
            HoldKind::RejectedReaffirm,
            HoldKind::RefusedDeferrals,
            HoldKind::AcceptedAdvice,
            HoldKind::AcceptedVerdict,
        ] {
            x.holds.push(Hold {
                kind,
                item: kind.as_str().into(),
            });
        }
    });
    assert_eq!(rules(&held), vec!["m3_hold"; 5], "{held:?}");
    // a before snapshot that is not the canon the landing read
    let stale = with(&|x| x.before_head = Some(other("head")));
    assert!(rules(&stale).contains(&"m3_snapshot"), "{stale:?}");

    // the hold reader on a real person act: a rejected decision of the batch
    let (_d2, mut r2, b) = batch_project();
    let pref = r2.resolve_proposal(&b.r#ref).unwrap();
    assert_eq!(holds_of(&r2.read(), &pref.version).unwrap(), vec![]);
    let one_way = pending(&r2)
        .items
        .into_iter()
        .find(|i| i.r#ref.starts_with(&format!("{}@", b.one_way)))
        .expect("the one_way exception");
    relayed(&mut r2, &one_way.r#ref, "reject", &one_way.card_hash);
    assert_eq!(
        holds_of(&r2.read(), &pref.version).unwrap(),
        vec![Hold {
            kind: HoldKind::RejectedDecision,
            item: one_way.r#ref.clone()
        }]
    );
}

/// The policy of one rule: agents `agent:a*` may land draft edits with an `editorial`
/// justification and no decision.
fn policy_fields() -> serde_json::Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [{
            "name": "drafts",
            "actors": ["agent:a*"],
            "lifecycle": "draft",
            "justifications": ["implements", "editorial"],
            "decisions": "none",
            "max_blocks": 20,
            "max_items": 20
        }],
        "roles": {"approvers": [PERSON]},
        "require": [{"when": "decision", "role": "approvers"}],
        "owners": []
    })
}

fn write_j(
    r: &mut Rdy,
    view: &str,
    ops: serde_json::Value,
    j: Option<Justification>,
) -> rdy_engine::WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops).unwrap(),
            justification: j,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write in {view}: {e}"))
}

/// A QA-pack project with a draft claim `x` and a landed authority policy; agent `agent:a1`
/// fixes a typo in `x` and lands it under the policy. The landing.
fn policy_landing() -> (tempfile::TempDir, Rdy, Hash) {
    let (d, mut r) = qa_project();
    let w = write_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."}
        ]),
        None,
    );
    land(&mut r, "seed").unwrap();
    let x = BlockId::parse(&w.created["$x"]).unwrap();
    let rules = write_j(
        &mut r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        None,
    );
    let dec = BlockId::parse(&rules.created["$dec"]).unwrap();
    write_j(
        &mut r,
        "pol",
        json!([{"op": "create", "type": "authority_policy", "parent": rules.created["$d"], "fields": policy_fields()}]),
        Some(Justification::Implements(vec![dec])),
    );
    land(&mut r, "pol").unwrap();
    put_j(&mut r, "a1", upd(&x, "X holds, now."), j_typo());
    let p = r
        .propose(&ag("a1"), "a1", "a1", Some(TEST_SUMMARY))
        .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    r.land_by_policy(&ag("a1"), &p.r#ref)
        .expect("the policy lands it");
    let l = r.read().canon_head().unwrap();
    (d, r, l)
}

#[test]
fn mg1_policy_and_person_evidence_cannot_be_interchanged() {
    let (_d, r, l) = policy_landing();
    assert_eq!(check_m3(&r), vec![], "the policy landing holds");
    let facts = M3Facts::read(&r.read()).unwrap();
    let p = facts.landing(&l).unwrap();
    assert_eq!(p.record.assurance, Assurance::Policy);
    assert!(p.record.policy.is_some() && p.record.assessment.is_some());
    let live = p.record.policy.clone().unwrap();
    let with = |facts: &M3Facts, at: Hash, f: &dyn Fn(&mut rdy_qa::m3_audit::LandingFact)| {
        let mut x = facts.clone();
        f(x.landing_mut(&at).unwrap());
        check_facts(&x)
    };
    let route = |v: &[Violation]| rules(v).into_iter().filter(|r| *r == "m3_route").count();
    // policy without its assessment, or without its version
    assert_eq!(route(&with(&facts, l, &|l| l.record.assessment = None)), 1);
    assert!(route(&with(&facts, l, &|l| l.record.approved_version = None)) >= 1);
    // policy plus a relay
    let relay = ActorRun {
        actor: "agent:relay".into(),
        run: None,
    };
    assert_eq!(
        route(&with(&facts, l, &|l| l.record.relayed_by = Some(relay.clone()))),
        1
    );
    // a policy recorded on a person's landing
    let person = with(&facts, l, &|l| l.record.assurance = Assurance::Local);
    assert_eq!(rules(&person), vec!["m3_route"], "{person:?}");

    // the person's route: a relayed card approval
    let (_d2, r2, _ready, observed, f) = relayed_landing();
    assert_eq!(f.audit, vec![]);
    let facts = M3Facts::read(&r2.read()).unwrap();
    let at = observed.landing;
    assert_eq!(
        facts.landing(&at).unwrap().record.assurance,
        Assurance::Relayed
    );
    // relayed without the shown card
    let unshown = with(&facts, at, &|l| {
        l.record.evidence.as_mut().unwrap().card_hash = None
    });
    assert_eq!(rules(&unshown), vec!["m3_route"], "{unshown:?}");
    // a relay with no evidence, or elicited evidence
    assert_eq!(
        rules(&with(&facts, at, &|l| l.record.evidence = None)),
        vec!["m3_route"]
    );
    let elicited = with(&facts, at, &|l| {
        l.record.evidence.as_mut().unwrap().kind = EvidenceKind::Elicited
    });
    assert_eq!(rules(&elicited), vec!["m3_route"]);
    // local is not relayed
    let local = with(&facts, at, &|l| l.record.assurance = Assurance::Local);
    assert_eq!(rules(&local), vec!["m3_route"], "{local:?}");
    // a person's landing that records a policy
    let p2 = with(&facts, at, &|l| l.record.policy = Some(live.clone()));
    assert_eq!(rules(&p2), vec!["m3_route"], "{p2:?}");
    // a re-affirmation card's landing is the stated landing with no assessment
    let mut card = f.t.clone();
    card.landing.reaffirm_card = true;
    card.landing.record.assessment = None;
    card.landing.assessment = None;
    assert_eq!(
        check_temporal(&card),
        vec![],
        "a re-affirmation card needs no assessment"
    );
    card.landing.reaffirm_card = false;
    assert_eq!(
        rules(&check_temporal(&card)),
        vec!["m3_assessment"],
        "any other landing does"
    );
}

#[test]
fn mg1_checker_reports_each_fault_without_calling_readiness() {
    let (_d, r, l) = briefed_stable_landing();
    let calls = readiness_calls_for_tests();
    let st = r.read();
    let facts = M3Facts::read(&st).unwrap();
    assert_eq!(check_facts(&facts), vec![]);
    let mut x = facts.clone();
    let f = x.landing_mut(&l).unwrap();
    f.changes[0].briefing = None;
    f.record.approved_version = Some(other("v"));
    let got = check_facts(&x);
    assert_eq!(rules(&got), vec!["m3_briefing", "m3_version"], "{got:?}");
    assert!(
        got.iter().all(|v| v.detail.contains(&l.short())),
        "each names the landing: {got:?}"
    );
    assert_eq!(check_m3(&r), vec![]);
    assert_eq!(
        readiness_calls_for_tests(),
        calls,
        "the audit never calls readiness"
    );
}
