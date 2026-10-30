//! rg10 (D-ME2HD, D-3JZP4, D-SDSV2, D-EYRGM; O16 to O20): a propose runs readiness on the
//! version it makes. A ready version gets its `ASSESSMENT` record in the propose batch, beside
//! the version and its plan; a version that is not ready is stored as before, with no record.
//! The reply says `ready`, lists the readiness `issues`, and names the `assessment`; `rdy
//! propose` prints the same.
use rdy_engine::plan::{PlanAnswerInput, PlanQuery};
use rdy_engine::tensions::{TensionKind, tension_label};
use rdy_engine::{E_PLAN_OPEN, E_VALIDATION, ProposalInfo, Rdy, Session, WriteRequest};
use rdy_model::assessment::{Assessment, AssessmentTensionStatus};
use rdy_model::plan::{PlanAct, PlanOutcome};
use rdy_model::{EditorialClass, Hash, Justification, Role};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, parse_test_ops, writer};
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items, confirm_before_landing, pending};
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};

const PLAN_GATE_FIX: &str = "close or defer each item, and answer each tension item with confirm or dismiss: rdy_plan / rdy plan";

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn reviewer() -> Session {
    Session::agent("agent:rev".into(), Some("rev".into()), Role::Reviewer).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value, justification: Option<Justification>) -> Value {
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: parse_test_ops(ops).unwrap(),
                justification,
                message: format!("write to {view}"),
            },
        )
        .unwrap_or_else(|e| panic!("write to {view}: {e}"));
    serde_json::to_value(w.created).unwrap()
}

/// Canon: a draft doc with section `n` to write into, and a deprecated doc holding claims `x`
/// and `x2`. A claim in `n` that cites one of them is one advisory tension of its version.
struct World {
    n: String,
    x: String,
    x2: String,
}

fn world(r: &mut Rdy) -> World {
    let ids = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$nd", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$n", "parent": "$nd", "fields": {"title": "Drafts"}},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "old", "title": "Old", "doc_type": "overview", "lifecycle": "deprecated"}},
            {"op": "create", "type": "section", "id": "$o", "parent": "$od", "fields": {"title": "Old rules"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$o", "body": "X is the old rule."},
            {"op": "create", "type": "claim", "id": "$x2", "parent": "$o", "body": "X2 is another old rule."}
        ]),
        Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "the seed".into(),
        }),
    );
    land(r, "seed").unwrap_or_else(|e| panic!("land the seed: {e} {:?}", e.issues));
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    World {
        n: id("$n"),
        x: id("$x"),
        x2: id("$x2"),
    }
}

/// View `view` writes a draft claim in `n` that cites `target`: one advisory tension.
fn cite(r: &mut Rdy, w: &World, view: &str, target: &str, body: &str) -> String {
    let ids = put(
        r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": w.n, "body": format!("{body} {{{{{target}}}}}.")}]),
        None,
    );
    ids["$y"].as_str().unwrap().to_string()
}

fn project() -> (tempfile::TempDir, Rdy, World) {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = world(&mut r);
    (dir, r, w)
}

fn propose(r: &mut Rdy, view: &str, summary: Option<&str>) -> ProposalInfo {
    r.propose(&writer(), view, view, summary)
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn kinds_from(r: &Rdy, from: Location) -> Vec<u8> {
    let mut out = vec![];
    r.store()
        .reader()
        .for_each_frame_from(from, r.store().log_end(), |_, kind, _, _, _| {
            out.push(rdy_store::codec::logical_kind(kind));
            Ok(())
        })
        .unwrap();
    out
}

/// The assessment the store names for version `id`, and the record.
fn stored_assessment(r: &Rdy, id: &str) -> Option<(Hash, Assessment)> {
    let st = r.read();
    let a = st
        .reader()
        .assessment_of_proposal(&h(id), st.seq())
        .unwrap()?;
    Some((a, st.reader().assessment(&a).unwrap()))
}

/// The bound plan of version `id`.
fn bound_plan(r: &Rdy, id: &str) -> Hash {
    let st = r.read();
    st.reader()
        .plan_of_proposal(&h(id), st.seq())
        .unwrap()
        .expect("a bound plan")
}

#[test]
fn rg10_a_ready_version_stores_its_assessment_in_the_propose_batch() {
    let (_d, mut r, w) = project();
    cite(&mut r, &w, "a", &w.x, "Z follows");
    let answered = answer_open_items(&mut r, &writer(), "a");
    assert_eq!(answered.len(), 1, "{answered:#?}");
    let start = r.store().log_end();
    let info = propose(&mut r, "a", Some(TEST_SUMMARY));
    assert!(info.ready, "{:#?}", info.issues);
    assert_eq!(info.issues, vec![]);
    let named = info
        .assessment
        .clone()
        .expect("a ready version names its assessment");
    // one batch, one marker: the version, its plan and its assessment
    let kinds_of = kinds_from(&r, start);
    let commits: Vec<usize> = kinds_of
        .iter()
        .enumerate()
        .filter(|(_, k)| **k == kinds::BATCH_COMMIT)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(commits.len(), 1, "{kinds_of:?}");
    for k in [kinds::PROPOSAL, kinds::PLAN, kinds::ASSESSMENT] {
        let at = kinds_of.iter().position(|x| *x == k);
        assert!(at.is_some_and(|i| i < commits[0]), "{k}: {kinds_of:?}");
    }
    // the store's index names the reply's assessment, and the record names the version
    let (a, record) = stored_assessment(&r, &info.id).expect("the store names an assessment");
    assert_eq!(a.to_string(), named);
    assert_eq!(record.proposal, h(&info.id));
    assert_eq!(record.base, info.canon_at.as_deref().map(h));
    assert_eq!(record.facts.changes, info.changes as u64);
    assert_eq!(record.scope.proposal, h(&info.id));
    record.validate().unwrap();
    r.fsck().unwrap();
}

#[test]
fn rg10_a_not_ready_version_stores_no_assessment() {
    let (_d, mut r, w) = project();
    cite(&mut r, &w, "a", &w.x, "Z follows");
    let start = r.store().log_end();
    // no summary, and its tension item has no answer
    let info = propose(&mut r, "a", None);
    assert!(!info.ready);
    assert_eq!(info.assessment, None);
    let codes: Vec<&str> = info.issues.iter().map(|i| i.code).collect();
    assert_eq!(codes, [E_VALIDATION, E_PLAN_OPEN], "{:#?}", info.issues);
    assert_eq!(
        (
            info.issues[0].message.as_str(),
            info.issues[1].message.as_str(),
            info.issues[1].fix.as_str()
        ),
        (
            format!("{} has no summary", info.r#ref).as_str(),
            "1 change-plan items are open",
            PLAN_GATE_FIX
        )
    );
    // the reply's issues are the queue's
    assert_eq!(
        info.issues,
        rdy_qa::intake::not_ready_issues(&r, &info.r#ref)
    );
    let kinds_of = kinds_from(&r, start);
    assert!(kinds_of.contains(&kinds::PROPOSAL), "{kinds_of:?}");
    assert!(kinds_of.contains(&kinds::PLAN), "{kinds_of:?}");
    assert!(!kinds_of.contains(&kinds::ASSESSMENT), "{kinds_of:?}");
    assert!(stored_assessment(&r, &info.id).is_none());
    // the reply as JSON: ready false, the issues, and assessment null, in that order after plan
    let text = serde_json::to_string(&info).unwrap();
    let at = |k: &str| text.find(&format!("\"{k}\":")).unwrap();
    assert!(
        at("plan") < at("ready") && at("ready") < at("issues") && at("issues") < at("assessment"),
        "{text}"
    );
    let v = serde_json::to_value(&info).unwrap();
    assert_eq!(v["ready"], false);
    assert_eq!(v["assessment"], Value::Null);
    assert_eq!(v["issues"].as_array().unwrap().len(), 2, "{v:#}");
}

#[test]
fn rg10_the_stored_card_hash_is_the_queue_cards_hash() {
    let (_d, mut r, w) = project();
    cite(&mut r, &w, "a", &w.x, "Z follows");
    answer_open_items(&mut r, &writer(), "a");
    let info = propose(&mut r, "a", Some(TEST_SUMMARY));
    let (_, record) = stored_assessment(&r, &info.id).expect("an assessment");
    let q = pending(&r);
    let item = q
        .items
        .iter()
        .find(|i| i.r#ref == info.r#ref)
        .unwrap_or_else(|| panic!("no approve item: {q:#?}"));
    assert_eq!(item.card_hash, record.card.hash.to_string(), "{item:#?}");
    let facts: Vec<String> = item
        .facts
        .iter()
        .map(|f| format!("{}: {}", f.name, f.value))
        .collect();
    assert_eq!(record.card.facts, facts);
}

#[test]
fn rg10_the_assessment_holds_each_tension_with_its_answer() {
    let (_d, mut r, w) = project();
    cite(&mut r, &w, "a", &w.x, "Z follows");
    cite(&mut r, &w, "a", &w.x2, "Z2 follows");
    // a reviewer dismisses the tension on x2; the author confirms the one on x
    let tensions: Vec<_> = {
        let st = r.read();
        let changes = st.included("a");
        st.landing_tensions("a", &changes)
            .unwrap()
            .into_iter()
            .filter(|t| t.kind == TensionKind::DeprecatedReference)
            .collect()
    };
    assert_eq!(tensions.len(), 2, "{tensions:#?}");
    let on = |id: &str| {
        tensions
            .iter()
            .find(|t| t.blocks().iter().any(|b| b.to_string() == id))
            .unwrap()
            .clone()
    };
    let (tx, tx2) = (on(&w.x), on(&w.x2));
    r.dismiss_tension(
        &reviewer(),
        &tension_label(&tx2.id),
        "x2 is cited as history",
    )
    .unwrap();
    r.answer_plan(
        &writer(),
        "a",
        &[PlanAnswerInput {
            item: tension_label(&tx.id),
            act: PlanAct::Confirm,
            reason: Some("x still guides z".into()),
        }],
    )
    .unwrap();
    let info = propose(&mut r, "a", Some(TEST_SUMMARY));
    assert!(info.ready, "{:#?}", info.issues);
    let (_, record) = stored_assessment(&r, &info.id).expect("an assessment");
    let plan = bound_plan(&r, &info.id);
    let answer = {
        let st = r.read();
        st.reader()
            .plan(&plan)
            .unwrap()
            .answers
            .into_iter()
            .find(|a| a.act == PlanAct::Confirm)
            .expect("the bound plan holds the confirm")
    };
    let row = |id: Hash| {
        record
            .tensions
            .iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("no row {id}: {:#?}", record.tensions))
    };
    let confirmed = row(tx.id);
    assert_eq!(confirmed.status, AssessmentTensionStatus::Open);
    let a = confirmed.answer.as_ref().expect("the row names its answer");
    assert_eq!((a.plan, a.item), (plan, answer.item));
    assert_eq!(confirmed.dismissal, None);
    assert_eq!(confirmed.vkey, tx.vkey);
    let dismissed = row(tx2.id);
    assert_eq!(dismissed.status, AssessmentTensionStatus::Dismissed);
    assert!(dismissed.dismissal.is_some(), "{dismissed:#?}");
    assert_eq!(dismissed.answer, None);
    // both rows' blocks are in the reach
    for b in confirmed.blocks.iter().chain(&dismissed.blocks) {
        assert!(record.reach.contains(b), "{b} {:?}", record.reach);
    }
}

#[test]
fn rg10_cli_propose_prints_readiness() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = world(&mut r);
        cite(&mut r, &w, "ok", &w.x, "Z follows");
        answer_open_items(&mut r, &writer(), "ok");
        cite(&mut r, &w, "open", &w.x2, "Z2 follows");
        r.close().unwrap();
    }
    let out = s.rdy(&[
        "propose",
        "--title",
        "ok",
        "--view",
        "ok",
        "--summary",
        TEST_SUMMARY,
    ]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    let pref = lines[0].split_whitespace().nth(1).unwrap().to_string();
    let n = lines.len();
    // ap8 (O13): the policy's verdict follows `ready`; with no policy it needs a person
    assert_eq!(
        lines[n - 3..],
        [
            "ready",
            "policy: needs a person: no authority policy is set",
            format!("approve with: rdy approve {pref}").as_str()
        ],
        "{text}"
    );
    let out = s.rdy(&["propose", "--title", "open", "--view", "open"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pref = text.split_whitespace().nth(1).unwrap().to_string();
    let tail: Vec<&str> = text.lines().rev().take(3).collect();
    assert_eq!(
        tail,
        [
            format!("  {E_PLAN_OPEN} -: 1 change-plan items are open (fix: {PLAN_GATE_FIX})")
                .as_str(),
            format!("  {E_VALIDATION} -: {pref} has no summary (fix: propose it again with a summary in plain words for the person who approves it)").as_str(),
            "not ready: 2 issue(s)",
        ],
        "{text}"
    );
    assert!(!text.contains("approve with:"), "{text}");
    // --json: the same reply
    let (ok, v) = s.json(&[
        "--json",
        "propose",
        "--title",
        "ok again",
        "--view",
        "ok",
        "--summary",
        TEST_SUMMARY,
    ]);
    assert!(ok, "{v}");
    assert_eq!(v["ready"], true, "{v:#}");
    assert_eq!(v["issues"], json!([]), "{v:#}");
    assert_eq!(v["assessment"].as_str().map(str::len), Some(64), "{v:#}");
    let (ok, v) = s.json(&["--json", "propose", "--title", "open", "--view", "open"]);
    assert!(ok, "{v}");
    assert_eq!(v["ready"], false, "{v:#}");
    assert_eq!(v["assessment"], Value::Null, "{v:#}");
    let codes: Vec<&str> = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, [E_VALIDATION, E_PLAN_OPEN], "{v:#}");
}

#[test]
fn rg10_a_second_version_gets_its_own_assessment() {
    let (_d, mut r, w) = project();
    cite(&mut r, &w, "a", &w.x, "Z follows");
    answer_open_items(&mut r, &writer(), "a");
    let v1 = propose(&mut r, "a", Some(TEST_SUMMARY));
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": w.n, "body": "One more draft claim."}]),
        None,
    );
    answer_open_items(&mut r, &writer(), "a");
    let v2 = propose(&mut r, "a", Some(TEST_SUMMARY));
    assert_eq!((v1.version, v2.version), (1, 2));
    assert!(v1.ready && v2.ready, "{:#?} {:#?}", v1.issues, v2.issues);
    let (a1, r1) = stored_assessment(&r, &v1.id).expect("v1's assessment");
    let (a2, r2) = stored_assessment(&r, &v2.id).expect("v2's assessment");
    assert_ne!(a1, a2);
    assert_eq!(Some(a1.to_string()), v1.assessment);
    assert_eq!(Some(a2.to_string()), v2.assessment);
    assert_eq!((r1.proposal, r2.proposal), (h(&v1.id), h(&v2.id)));
    assert_eq!((r1.facts.changes, r2.facts.changes), (1, 2));
    // the second version lands and names its own assessment
    confirm_before_landing(&mut r, &approver(), &v2);
    r.land(&approver(), &v2.id)
        .unwrap_or_else(|e| panic!("land v2: {e} {:?}", e.issues));
}

/// Addendum 2 (ruling task-15 F2): a0 and a1 append a claim under one section from one base;
/// a1 lands first; a0 answers its advisory tension item and proposes. The version is ready at
/// propose, at `change_plan{proposal}`, and at its landing, which gives a0's claim a new order
/// key.
#[test]
fn rg10_an_order_key_change_keeps_the_answered_tension_item() {
    let (_d, mut r, w) = project();
    cite(&mut r, &w, "a0", &w.x, "Z follows");
    put(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "claim", "parent": w.n, "body": "A1 appends here too."}]),
        None,
    );
    land(&mut r, "a1").unwrap_or_else(|e| panic!("land a1: {e} {:?}", e.issues));
    let answered = answer_open_items(&mut r, &writer(), "a0");
    assert_eq!(answered.len(), 1, "{answered:#?}");
    let info = propose(&mut r, "a0", Some(TEST_SUMMARY));
    assert!(info.ready, "{:#?}", info.issues);
    assert!(info.assessment.is_some());
    // change_plan{proposal}: the tension item is closed
    let plan = r
        .read()
        .change_plan(
            &PlanQuery {
                proposal: Some(info.r#ref.clone()),
                max_tokens: Some(100_000),
                ..Default::default()
            },
            "work",
        )
        .unwrap();
    let plan = serde_json::to_value(plan).unwrap();
    let rows: Vec<&Value> = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "tension")
        .collect();
    assert_eq!(rows.len(), 1, "{plan:#}");
    assert_eq!(rows[0]["state"], "closed", "{plan:#}");
    assert!(
        rdy_qa::intake::pending(&r)
            .not_ready
            .iter()
            .all(|n| n.r#ref != info.r#ref),
        "the queue lists it as ready"
    );
    // the landing gives y a new order key and seals the confirm
    confirm_before_landing(&mut r, &approver(), &info);
    let landing = r
        .land(&approver(), &info.id)
        .unwrap_or_else(|e| panic!("land a0: {e} {:?}", e.issues));
    let seal = r
        .read()
        .change_plan(
            &PlanQuery {
                landing: Some(landing),
                max_tokens: Some(100_000),
                ..Default::default()
            },
            "work",
        )
        .unwrap();
    let seal = serde_json::to_value(seal).unwrap();
    let rows: Vec<&Value> = seal["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "tension")
        .collect();
    assert_eq!(rows.len(), 1, "{seal:#}");
    assert_eq!(
        rows[0]["outcome"],
        PlanOutcome::Confirmed.as_str(),
        "{seal:#}"
    );
}
