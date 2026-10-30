//! ap8 (D-ZWP7Q "The verdict", D-DDSBJ, D-ME2HD; M3c O8, O9, O13, O19): readiness computes the
//! authority policy's verdict for every ready version under the live policy, the policy block in
//! canon at its head. The assessment stores it, the approve card shows it as the fact `Policy`
//! after `Exceptions`, and `rdy_propose` and `rdy propose` report it. A change to the policy
//! block needs `implements` a decision: the last change of the version that writes it counts.
use rdy_engine::engine::Local;
use rdy_engine::pending::{Item, ItemKind, PendingQuery};
use rdy_engine::policy::{LivePolicy, live_policy};
use rdy_engine::policy_verdict::NO_POLICY;
use rdy_engine::{E_VALIDATION, ProposalInfo, Rdy, Scope, Session, WriteRequest, WriteResult};
use rdy_mcp::tools::{McpState, rdy_propose, rdy_session};
use rdy_model::assessment::{AssessedPolicy, Assessment, PolicyVerdict};
use rdy_model::{BlockId, Hash, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, parse_test_ops, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::tensions::{ag, j_typo, put_j, upd};
use serde_json::{Value, json};
use std::sync::Arc;

/// The policy of one rule: agents `agent:a*` may land drafts with `implements` or `editorial`
/// changes and no decision; a version that writes a decision needs a person from `approvers`.
fn policy_fields() -> Value {
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

/// Canon: a draft doc `notes` with a section `s` and a claim `x`; with `policy`, also a doc
/// `rules` that holds the policy block, landed with the decision it implements.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    x: BlockId,
    s: BlockId,
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Writes `ops` in `view` as `session` with justification `j`.
fn write_j(
    r: &mut Rdy,
    session: &Session,
    view: &str,
    ops: Value,
    j: Option<Justification>,
) -> WriteResult {
    r.write(
        session,
        WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops).unwrap(),
            justification: j,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write in {view}: {e}"))
}

/// In view `view`, as the human writer: a doc `rules` and a decision under it that needs no
/// content; returns (doc, decision).
fn rules_and_decision(r: &mut Rdy, view: &str) -> (BlockId, BlockId) {
    let w = write_j(
        r,
        &writer(),
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        None,
    );
    (id(&w.created["$d"]), id(&w.created["$dec"]))
}

/// In view `view`, as the human writer: the policy block under `doc`, with justification `j`.
fn write_policy(
    r: &mut Rdy,
    view: &str,
    doc: &BlockId,
    j: Option<Justification>,
) -> (BlockId, WriteResult) {
    let w = write_j(
        r,
        &writer(),
        view,
        json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": doc.to_string(), "fields": policy_fields()}]),
        j,
    );
    (id(&w.created["$p"]), w)
}

fn implements(d: &BlockId) -> Option<Justification> {
    Some(Justification::Implements(vec![d.clone()]))
}

fn world(policy: bool) -> World {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = write_j(
        &mut r,
        &writer(),
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."}
        ]),
        None,
    );
    land(&mut r, "seed").unwrap();
    if policy {
        let (doc, d) = rules_and_decision(&mut r, "pol");
        write_policy(&mut r, "pol", &doc, implements(&d));
        land(&mut r, "pol").unwrap();
    }
    World {
        x: id(&w.created["$x"]),
        s: id(&w.created["$s"]),
        _dir: dir,
        r,
    }
}

fn live(r: &Rdy) -> LivePolicy {
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    live_policy(&st, &canon).expect("a policy in canon")
}

/// Agent `agent:<name>` proposes its view `<name>` with a summary.
fn propose_as(r: &mut Rdy, name: &str) -> ProposalInfo {
    r.propose(&ag(name), name, name, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {name}: {e}"))
}

/// Agent `agent:<name>` fixes a typo in claim `x`: a draft edit the policy accepts.
fn typo_edit(w: &mut World, name: &str) {
    put_j(&mut w.r, name, upd(&w.x, "X holds, now."), j_typo());
}

/// Agent `agent:<name>` records a decision that needs no content under section `s`: a version
/// the rule `drafts` does not cover. Returns the decision.
fn decision_edit(w: &mut World, name: &str) -> BlockId {
    let res = put_j(
        &mut w.r,
        name,
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": w.s.to_string(),
                "fields": {"statement": "Ideas stay short", "status": "proposed", "needs_content": false}}]),
        j_typo(),
    );
    id(&res.created["$dec"])
}

/// The assessment version `p` stored.
fn stored(r: &Rdy, p: &ProposalInfo) -> Assessment {
    let st = r.read();
    let v: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&v, st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    st.reader().assessment(&h).unwrap()
}

fn approve_item(r: &Rdy, pref: &str) -> Item {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
        .unwrap_or_else(|| panic!("an approve item for {pref}"))
}

fn fact<'a>(i: &'a Item, name: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("fact {name}"))
        .value
        .as_str()
}

fn accepts(rule: &str) -> PolicyVerdict {
    PolicyVerdict::Accepts {
        rule: rule.to_string(),
    }
}

fn no_policy() -> AssessedPolicy {
    AssessedPolicy {
        policy: None,
        verdict: PolicyVerdict::NeedsPerson {
            role: None,
            reasons: vec![NO_POLICY.to_string()],
        },
    }
}

/// The O19 issue of the change whose short hash is `short`, on policy block `block`.
fn o19(block: &BlockId, short: &str) -> rdy_schema::Issue {
    rdy_schema::Issue {
        severity: rdy_schema::Severity::Error,
        code: E_VALIDATION,
        block: Some(block.clone()),
        message: format!("change {short} changes the authority policy and implements no decision"),
        fix: "write it again with implements: the decision that changes the policy".into(),
    }
}

fn short_of(w: &WriteResult) -> String {
    w.change.parse::<Hash>().unwrap().short()
}

#[test]
fn ap8_a_ready_version_stores_its_verdict() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    assert!(p.ready, "{:?}", p.issues);
    let a = stored(&w.r, &p);
    assert_eq!(
        a.policy,
        Some(AssessedPolicy {
            policy: Some(live(&w.r).reference()),
            verdict: accepts("drafts"),
        })
    );
    a.validate().expect("the stored assessment validates");
}

#[test]
fn ap8_the_card_shows_policy_after_exceptions() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    let i = approve_item(&w.r, &p.r#ref);
    let names: Vec<&str> = i.facts.iter().map(|f| f.name.as_ref()).collect();
    assert_eq!(names[names.len() - 2..], ["Exceptions", "Policy"]);
    assert_eq!(
        fact(&i, "Policy"),
        format!("{}: accepts under rule drafts", live(&w.r).label())
    );
    // the stored card holds the same line
    let a = stored(&w.r, &p);
    assert_eq!(
        a.card.facts.last().unwrap(),
        &format!("Policy: {}: accepts under rule drafts", live(&w.r).label())
    );
}

#[test]
fn ap8_with_no_policy_the_fact_reads_no_authority_policy_is_set() {
    let mut w = world(false);
    typo_edit(&mut w, "a1");
    let p = propose_as(&mut w.r, "a1");
    assert!(p.ready, "{:?}", p.issues);
    assert_eq!(stored(&w.r, &p).policy, Some(no_policy()));
    let i = approve_item(&w.r, &p.r#ref);
    assert_eq!(
        fact(&i, "Policy"),
        "needs a person: no authority policy is set"
    );
}

#[test]
fn ap8_a_version_that_writes_a_policy_is_judged_under_canons_policy() {
    // with no policy in canon, a version that writes one is judged as having none
    let mut w = world(false);
    let (doc, d) = rules_and_decision(&mut w.r, "w");
    write_policy(&mut w.r, "w", &doc, implements(&d));
    let p =
        w.r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap();
    assert!(p.ready, "{:?}", p.issues);
    assert_eq!(stored(&w.r, &p).policy, Some(no_policy()));
    assert_eq!(
        fact(&approve_item(&w.r, &p.r#ref), "Policy"),
        "needs a person: no authority policy is set"
    );

    // with a policy in canon, a version that changes it is judged under canon's version, which
    // never accepts a change to itself
    let mut w = world(true);
    let before = live(&w.r);
    let dec = write_j(
        &mut w.r,
        &writer(),
        "w",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": w.s.to_string(),
                "fields": {"statement": "Widen the policy", "status": "proposed", "needs_content": false}}]),
        None,
    );
    let d = id(&dec.created["$dec"]);
    let mut fields = policy_fields();
    fields["agent_may_accept"][0]["max_blocks"] = json!(30);
    write_j(
        &mut w.r,
        &writer(),
        "w",
        json!([{"op": "update", "id": before.block.to_string(), "fields": fields}]),
        implements(&d),
    );
    let p =
        w.r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap();
    assert!(p.ready, "{:?}", p.issues);
    assert_eq!(
        stored(&w.r, &p).policy,
        Some(AssessedPolicy {
            policy: Some(before.reference()),
            verdict: PolicyVerdict::NeedsPerson {
                role: Some("approvers".into()),
                reasons: vec!["it changes the authority policy".into()],
            },
        })
    );
}

#[test]
fn ap8_rdy_propose_reports_policy() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    let d = decision_edit(&mut w, "a2");
    let label = live(&w.r);
    let engine = Arc::new(Local::new(w.r));
    let propose = |name: &str| {
        let mut st = McpState::new(engine.clone());
        rdy_session(
            &mut st,
            serde_json::from_value(
                json!({"actor": format!("agent:{name}"), "run": name, "role": "writer"}),
            )
            .unwrap(),
        )
        .unwrap();
        rdy_propose(
            &mut st,
            serde_json::from_value(json!({"title": name, "view": name, "summary": TEST_SUMMARY}))
                .unwrap(),
        )
        .unwrap_or_else(|e| panic!("rdy_propose {name}: {e}"))
    };
    let accepted = propose("a1");
    assert_eq!(accepted["ready"], true, "{accepted:#}");
    assert_eq!(
        accepted["policy"],
        json!({
            "block": label.block.to_string(),
            "version": label.version.to_string(),
            "verdict": "accepts",
            "rule": "drafts",
            "role": null,
            "reasons": []
        })
    );
    let person = propose("a2");
    assert_eq!(person["ready"], true, "{person:#}");
    assert_eq!(
        person["policy"],
        json!({
            "block": label.block.to_string(),
            "version": label.version.to_string(),
            "verdict": "needs_person",
            "rule": null,
            "role": "approvers",
            "reasons": [format!("rule drafts: it records decision {d}")]
        })
    );
}

#[test]
fn ap8_a_not_ready_version_reports_policy_null() {
    let mut w = world(true);
    typo_edit(&mut w, "a1");
    // no summary: not ready, so no verdict
    let p = w.r.propose(&ag("a1"), "a1", "a1", None).unwrap();
    assert!(!p.ready);
    assert_eq!(p.policy, None);
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v.get("policy"), Some(&Value::Null), "{v:#}");
}

#[test]
fn ap8_a_policy_change_that_implements_no_decision_is_not_ready() {
    let mut w = world(false);
    let (doc, _) = rules_and_decision(&mut w.r, "w");
    let (policy, write) = write_policy(&mut w.r, "w", &doc, None);
    let p =
        w.r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap();
    assert!(!p.ready);
    assert_eq!(p.issues, vec![o19(&policy, &short_of(&write))]);
    assert_eq!(p.policy, None);
}

#[test]
fn ap8_a_policy_change_that_implements_a_decision_is_ready() {
    let mut w = world(false);
    let (doc, d) = rules_and_decision(&mut w.r, "w");
    write_policy(&mut w.r, "w", &doc, implements(&d));
    let p =
        w.r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap();
    assert!(p.ready, "{:?}", p.issues);
    assert!(p.issues.is_empty());
}

#[test]
fn ap8_a_later_write_of_the_same_policy_block_that_implements_a_decision_clears_the_issue() {
    // the first change implements no decision; a later one that writes the block again does
    let mut w = world(false);
    let (doc, d) = rules_and_decision(&mut w.r, "w");
    let (policy, _) = write_policy(&mut w.r, "w", &doc, j_typo());
    let p =
        w.r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap();
    assert!(!p.ready);
    let mut fields = policy_fields();
    fields["agent_may_accept"][0]["max_items"] = json!(10);
    write_j(
        &mut w.r,
        &writer(),
        "w",
        json!([{"op": "update", "id": policy.to_string(), "fields": fields.clone()}]),
        implements(&d),
    );
    let p =
        w.r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap();
    assert!(p.ready, "{:?}", p.issues);

    // the reverse: the last change counts, so an editorial write after it is the issue
    fields["agent_may_accept"][0]["max_items"] = json!(5);
    let last = write_j(
        &mut w.r,
        &writer(),
        "w",
        json!([{"op": "update", "id": policy.to_string(), "fields": fields}]),
        j_typo(),
    );
    let p =
        w.r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap();
    assert!(!p.ready);
    assert_eq!(p.issues, vec![o19(&policy, &short_of(&last))]);
}

#[test]
fn ap8_cli_propose_prints_policy_and_land_with() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write_j(
            &mut r,
            &writer(),
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
                {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."}
            ]),
            None,
        );
        land(&mut r, "seed").unwrap();
        let (doc, d) = rules_and_decision(&mut r, "pol");
        write_policy(&mut r, "pol", &doc, implements(&d));
        land(&mut r, "pol").unwrap();
        let x = id(&w.created["$x"]);
        put_j(&mut r, "a1", upd(&x, "X holds, now."), j_typo());
        r.close().unwrap();
    }
    let label = {
        let r = Rdy::open(s.dir.path()).unwrap();
        let l = live(&r);
        r.close().unwrap();
        l.label()
    };
    let out = s.rdy_as(
        "agent:a1",
        Some("a1"),
        &[
            "propose",
            "--title",
            "Typo",
            "--view",
            "a1",
            "--summary",
            TEST_SUMMARY,
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let pref = text
        .split_whitespace()
        .find(|w| w.starts_with("P-"))
        .unwrap_or_else(|| panic!("a proposal ref in {text}"))
        .to_string();
    assert!(
        text.contains(&format!(
            "\nready\npolicy: {label}: accepts under rule drafts\nland with: rdy land {pref}\napprove with: rdy approve {pref}"
        )),
        "{text}"
    );
    // --json carries the same verdict in the reply's `policy`
    let (ok, v) = s.json_as(
        "agent:a1",
        Some("a1"),
        &[
            "--json",
            "propose",
            "--title",
            "Typo",
            "--view",
            "a1",
            "--summary",
            TEST_SUMMARY,
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v["policy"]["verdict"], "accepts", "{v:#}");
    assert_eq!(v["policy"]["rule"], "drafts", "{v:#}");
}
