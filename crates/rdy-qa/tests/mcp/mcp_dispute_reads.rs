//! ap7 (D-ZWP7Q, D-C8RZW; O22, O23): `rdy_log` and `rdy log` rows carry `policy` and
//! `dispute: {state, disputes}`; `rdy_trace`'s `introduced` and `accepted_in` carry `dispute`. The
//! tests write disputes and resolutions through the store's own writers (`Store::put_dispute`,
//! `put_dispute_resolution`), not through `rdy_dispute`, and a policy landing through
//! `Store::prepare_land_by_policy`.
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_mcp::tools::{McpState, rdy_session, rdy_trace};
use rdy_mcp::view_tools::rdy_log;
use rdy_model::assessment::*;
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::{Block, BlockId, Change, ChangeLinks, Hash, Op, Provenance, Role};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_store::LandRequest;
use rdy_store::plan::ProposeRequest;
use serde_json::{Value, from_value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    from_value(v).unwrap()
}

fn prov(actor: &str, role: Role, at_ms: i64) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("run-1".into()),
        role,
        at_ms,
    }
}

/// A dispute of `landing` by a person, opened at `at_ms`.
fn dispute(landing: &str, reason: &str, at_ms: i64) -> Dispute {
    Dispute {
        landing: landing.parse().unwrap(),
        opened_by: prov("human:qa", Role::Approver, at_ms),
        reason: reason.into(),
        at_ms,
    }
}

/// Stores `d` through the store's writer; returns its hash.
fn open_dispute(r: &mut Rdy, d: &Dispute) -> Hash {
    r.store_mut().put_dispute(d, None).unwrap().0
}

/// Stores the resolution of `dispute`.
fn resolve(r: &mut Rdy, dispute: Hash, outcome: DisputeOutcome) {
    let res = DisputeResolution {
        dispute,
        outcome,
        by: prov("human:qa", Role::Approver, 99),
        correction: None,
    };
    r.store_mut().put_dispute_resolution(&res, None).unwrap();
}

/// Publishes what the store holds.
fn publish(r: &mut Rdy) {
    r.invalidate();
    r.seal_open().unwrap();
}

/// One more landing: a doc `name` in view `name`, landed by the person.
fn land_doc(r: &mut Rdy, name: &str) -> String {
    write(
        r,
        name,
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
    )
    .unwrap();
    land(r, name).unwrap()
}

fn mcp(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:qa", "run": "qa", "role": "reader"})),
    )
    .unwrap();
    st
}

/// The log rows by landing id.
fn rows_by_landing(st: &mut McpState) -> BTreeMap<String, Value> {
    let rows = rdy_log(st, p(json!({}))).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| (r["landing"].as_str().unwrap().to_string(), r.clone()))
        .collect()
}

// ---- a policy landing, as Store::prepare_land_by_policy writes it ----

fn block(i: &str, ty: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(i).unwrap(),
        block_type: ty.into(),
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
        provenance: prov("agent:w", Role::Writer, 3),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn propose(s: &mut rdy_store::Store, title: &str) -> Hash {
    let req = ProposeRequest {
        view: "work",
        title,
        summary: Some("edits a claim"),
        by: prov("agent:w", Role::Writer, 3),
        changes: s.included("work").unwrap().as_ref().clone(),
        alternative: None,
        prev: None,
        rekey: None,
        answer: None,
    };
    let prepared = s.prepare_propose(req).unwrap();
    let v = prepared.version();
    s.commit_propose(prepared).unwrap();
    v
}

fn scores(n: u32) -> AssessmentScores {
    AssessmentScores {
        overall: n,
        grounded: n,
        consistent: n,
        current: n,
        connected: n,
        complete: n,
        governed: n,
    }
}

/// An assessment of `proposal` on `base` that the policy `policy` accepts.
fn assessment(proposal: Hash, base: Hash, policy: PolicyRef, blocks: Vec<BlockId>) -> Assessment {
    Assessment {
        proposal,
        base: Some(base),
        method: 1,
        pack: AssessmentPack {
            name: "rdy-qa".into(),
            version: "1.0.0".into(),
            digest: Hash::of(b"pack digest"),
        },
        config: Hash::of(b"config"),
        policy: Some(AssessedPolicy {
            policy: Some(policy),
            verdict: PolicyVerdict::Accepts {
                rule: "drafts".into(),
            },
        }),
        facts: AssessmentFacts {
            changes: 1,
            consequences: 0,
            plan: AssessmentPlanCounts {
                open: 0,
                closed: 0,
                deferred: 0,
                by_kind: vec![],
            },
            supersedes: vec![],
            reaffirms: vec![],
            reversibility: vec![],
            exceptions: vec![],
        },
        tensions: vec![],
        tensions_omitted: 0,
        suspects: vec![],
        suspects_omitted: 0,
        coverage: AssessmentCoverage {
            pages: vec![],
            corpus: AssessmentScoreChange {
                before: scores(50),
                after: scores(60),
            },
        },
        confidence: AssessmentConfidence {
            level: AssessmentLevel::High,
            reasons: vec![],
        },
        reach: blocks.clone(),
        card: AssessmentCard {
            hash: Hash::of(b"card"),
            facts: vec!["Changes: 1".into()],
        },
        scope: AssessmentScope { proposal, blocks },
        issues: vec![],
    }
}

/// The landings of a project: `first` (by a person: the policy block and a claim) and `policy`
/// (an agent's edit of the claim, accepted by the policy).
struct PolicyProject {
    first: Hash,
    policy_landing: Hash,
    policy: PolicyRef,
}

fn policy_project(r: &mut Rdy) -> PolicyProject {
    let s = r.store_mut();
    let pol = block("ap-00001", "authority_policy", "The authority policy.");
    let claim = block("b-00001", "claim", "A claim.");
    s.commit_to_view(
        "work",
        change(vec![
            Op::Create { block: pol.clone() },
            Op::Create {
                block: claim.clone(),
            },
        ]),
    )
    .unwrap();
    let v1 = propose(s, "first");
    let first = s.land(&v1, prov("human:alice", Role::Approver, 3)).unwrap();
    let mut edited = claim.clone();
    edited.body = "A claim, edited.".into();
    s.commit_to_view(
        "work",
        change(vec![Op::Update {
            base: claim.version_hash(),
            block: edited,
        }]),
    )
    .unwrap();
    let version = propose(s, "second");
    let policy = PolicyRef {
        block: pol.id.clone(),
        version: pol.version_hash(),
    };
    let held = s
        .put_assessment(&assessment(
            version,
            first,
            policy.clone(),
            vec![claim.id.clone()],
        ))
        .unwrap();
    let req = LandRequest {
        proposal: version,
        accept: None,
        approver: prov("agent:w", Role::Writer, 3),
        alt: None,
        relay: None,
        evidence: None,
        assessment: Some(held),
    };
    let prepared = s.prepare_land_by_policy(req, policy.clone()).unwrap();
    let policy_landing = s.commit_land(prepared, &[]).unwrap().landing;
    publish(r);
    PolicyProject {
        first,
        policy_landing,
        policy,
    }
}

// ---- the tests ----

#[test]
fn ap7_rdy_log_rows_carry_the_dispute_state() {
    let (_d, mut r) = project();
    let none = land_doc(&mut r, "none");
    let open = land_doc(&mut r, "open");
    let upheld = land_doc(&mut r, "upheld");
    let dismissed = land_doc(&mut r, "dismissed");
    let withdrawn = land_doc(&mut r, "withdrawn");
    let mixed = land_doc(&mut r, "mixed");
    // a landing with a dispute opened and one that is still open ranks `open`
    let d_open = open_dispute(&mut r, &dispute(&open, "needs a look", 10));
    let d_upheld = open_dispute(&mut r, &dispute(&upheld, "wrong edit", 10));
    resolve(&mut r, d_upheld, DisputeOutcome::Upheld);
    let d_dismissed = open_dispute(&mut r, &dispute(&dismissed, "fine really", 10));
    resolve(&mut r, d_dismissed, DisputeOutcome::Dismissed);
    let d_withdrawn = open_dispute(&mut r, &dispute(&withdrawn, "never mind", 10));
    resolve(&mut r, d_withdrawn, DisputeOutcome::Withdrawn);
    // two disputes of one landing, written newest first: `disputes` lists them oldest first
    let late = open_dispute(&mut r, &dispute(&mixed, "second", 30));
    let early = open_dispute(&mut r, &dispute(&mixed, "first", 20));
    resolve(&mut r, late, DisputeOutcome::Dismissed);
    resolve(&mut r, early, DisputeOutcome::Upheld);
    publish(&mut r);

    let mut st = mcp(r);
    let rows = rows_by_landing(&mut st);
    let state = |l: &str| rows[l]["dispute"].clone();
    assert_eq!(
        state(&none),
        json!({"state": "none", "disputes": []}),
        "{}",
        rows[&none]
    );
    assert_eq!(
        state(&open),
        json!({"state": "open", "disputes": [d_open.to_string()]})
    );
    assert_eq!(
        state(&upheld),
        json!({"state": "upheld", "disputes": [d_upheld.to_string()]})
    );
    assert_eq!(
        state(&dismissed),
        json!({"state": "dismissed", "disputes": [d_dismissed.to_string()]})
    );
    // withdrawn reads as none, and still lists the dispute
    assert_eq!(
        state(&withdrawn),
        json!({"state": "none", "disputes": [d_withdrawn.to_string()]})
    );
    // upheld outranks dismissed; the ids run oldest first (O22)
    assert_eq!(
        state(&mixed),
        json!({"state": "upheld", "disputes": [early.to_string(), late.to_string()]})
    );
    // both keys are on every row
    for row in rows.values() {
        assert!(
            row.get("policy").is_some() && row.get("dispute").is_some(),
            "{row}"
        );
    }
}

#[test]
fn ap7_rdy_log_rows_carry_the_policy_of_a_policy_landing() {
    let (_d, mut r) = project();
    let f = policy_project(&mut r);
    let mut st = mcp(r);
    let rows = rows_by_landing(&mut st);
    let policy = &rows[&f.policy_landing.to_string()];
    assert_eq!(policy["assurance"], "policy", "{policy}");
    assert_eq!(
        policy["policy"],
        json!({"block": f.policy.block.to_string(), "version": f.policy.version.to_string()}),
        "{policy}"
    );
    // any other landing reads null
    let first = &rows[&f.first.to_string()];
    assert!(first.get("policy").is_some_and(Value::is_null), "{first}");
    assert_eq!(first["dispute"], json!({"state": "none", "disputes": []}));
}

#[test]
fn ap7_rdy_trace_introduced_and_accepted_in_carry_the_dispute() {
    let (_d, mut r) = project();
    let (claim, decision) = {
        let w = write(&mut r, "w", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "refunds", "title": "Refunds", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "decision", "id": "$dd", "parent": "$s",
                "fields": {"statement": "Refunds need an approver", "status": "proposed"}},
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": "A refund of 200 needs an approver.",
                "links": {"decided_by": [{"target": "$dd"}]}}
        ]))
        .unwrap();
        (w.created["$k"].clone(), w.created["$dd"].clone())
    };
    let landing = land(&mut r, "w").unwrap();
    let d = open_dispute(&mut r, &dispute(&landing, "the rule is too strict", 10));
    publish(&mut r);
    let mut st = mcp(r);
    let open = json!({"state": "open", "disputes": [d.to_string()]});

    let v = rdy_trace(&mut st, p(json!({"id": claim, "scope": "canon"}))).unwrap();
    assert_eq!(v["introduced"]["id"], landing.as_str(), "{v}");
    assert_eq!(v["introduced"]["dispute"], open, "{v}");
    let v = rdy_trace(&mut st, p(json!({"id": decision, "scope": "canon"}))).unwrap();
    let accepted = &v["decisions"][0]["accepted_in"];
    assert_eq!(accepted["id"], landing.as_str(), "{v}");
    assert_eq!(accepted["dispute"], open, "{v}");
}

#[test]
fn ap7_rdy_trace_landing_without_a_dispute_reads_none() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    )
    .unwrap();
    land(&mut r, "w").unwrap();
    let id = w.created["$d"].clone();
    let mut st = mcp(r);
    let v = rdy_trace(&mut st, p(json!({"id": id, "scope": "canon"}))).unwrap();
    assert_eq!(
        v["introduced"]["dispute"],
        json!({"state": "none", "disputes": []}),
        "{v}"
    );
}

#[test]
fn ap7_rdy_log_text_prints_the_policy_and_dispute_lines() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let f = policy_project(&mut r);
    let other = land_doc(&mut r, "other");
    open_dispute(&mut r, &dispute(&f.policy_landing.to_string(), "why", 10));
    let u = open_dispute(&mut r, &dispute(&other, "bad", 10));
    resolve(&mut r, u, DisputeOutcome::Upheld);
    publish(&mut r);
    r.close().unwrap();

    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    let o = s.rdy(&["log"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    // a landing's lines run from its head line to the next head line
    let block_of = |landing: &str| -> Vec<String> {
        let short = &landing[..12];
        let mut it = text.lines().skip_while(|l| !l.starts_with(short));
        let mut out = vec![it.next().expect("the head line").to_string()];
        out.extend(it.take_while(|l| l.starts_with("    ")).map(str::to_string));
        out
    };
    let pol = block_of(&f.policy_landing.to_string());
    let policy_line = format!(
        "    policy {}@{}",
        f.policy.block,
        &f.policy.version.to_string()[..12]
    );
    let at = pol
        .iter()
        .position(|l| l.starts_with("    assessment "))
        .unwrap_or_else(|| panic!("an assessment line in {pol:?}"));
    // the policy line comes right after the assessment line, then the dispute line
    assert_eq!(pol[at + 1], policy_line, "{pol:?}");
    assert_eq!(pol[at + 2], "    dispute open: 1", "{pol:?}");
    // a resolved dispute reads by its state; a landing with no dispute has no line
    let other_lines = block_of(&other);
    assert!(
        other_lines.contains(&"    dispute upheld: 1".to_string()),
        "{other_lines:?}"
    );
    assert!(!other_lines.iter().any(|l| l.starts_with("    policy ")));
    let plain = block_of(&f.first.to_string());
    assert!(
        !plain
            .iter()
            .any(|l| l.starts_with("    policy ") || l.starts_with("    dispute ")),
        "{plain:?}"
    );
}

#[test]
fn ap7_rdy_trace_text_prints_the_dispute_state() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    )
    .unwrap();
    let landing = land(&mut r, "w").unwrap();
    let id = w.created["$d"].clone();
    let d = open_dispute(&mut r, &dispute(&landing, "why", 10));
    resolve(&mut r, d, DisputeOutcome::Dismissed);
    publish(&mut r);
    r.close().unwrap();
    let o = s.rdy(&["trace", &id, "--scope", "canon"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let line = text
        .lines()
        .find(|l| l.starts_with("introduced: "))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.ends_with(" (dispute dismissed)"), "{line}");
}
