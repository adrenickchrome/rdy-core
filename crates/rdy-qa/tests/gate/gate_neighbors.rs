//! nb8 (D-YXBXH, D-ME2HD, D-NJ2SP, D-MPJM0; TASK-978 O2): a neighbor consequence becomes a plan
//! item with edge `neighbor` at propose. On stable content readiness needs its answer; on drafts
//! it stays advice. Its answer keeps the item's content hashes, so it lapses as any answer does,
//! and the landing seals it with its edge and outcome. A grouped answer closes neighbor items
//! only: it never stands in for a rule check, a summary, a tension answer, a deferral acceptance
//! or an accepted verdict. Past the pair bound or the plan's item bound, nothing is written.
use crate::gate_check_verdicts::meaning_verdict;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::consequence::{MAX_NEIGHBOR_PAIRS, NEIGHBOR_EDGE};
use rdy_engine::engine::Local;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, Pending, PendingQuery};
use rdy_engine::plan::api::PlanAnswerSpec;
use rdy_engine::plan::lifecycle::{PLAN_GATE_FIX, PLAN_OPEN_FIX, W_PLAN_OPEN};
use rdy_engine::plan::{ItemState, PlanAnswerInput, PlanReply, PlanRow};
use rdy_engine::{
    E_DEFERRAL_OPEN, E_LIMIT, E_PLAN_OPEN, E_PROPOSAL_NOT_READY, E_RULE_CHECK, E_VALIDATION,
    Engine, ProposalInfo, Rdy, RdyError, Scope,
};
use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome, item_display, item_hash};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, land, project, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, accept_deferrals};
use rdy_qa::tensions::{ag, j_typo, put_j, upd};
use rdy_schema::{Issue, Severity};
use rdy_store::segment::Location;
use serde_json::{Value, json};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn pi(cause: &BlockId, subject: &BlockId, kind: PlanKind) -> String {
    item_display(&item_hash(cause, subject.as_str(), kind))
}

fn confirm(item: &str, reason: &str) -> PlanAnswerInput {
    PlanAnswerInput {
        item: item.into(),
        act: PlanAct::Confirm,
        reason: Some(reason.into()),
    }
}

/// One grouped answer: every open item with edge `neighbor`, answered with `act`.
fn neighbor_group(act: &str, reason: &str) -> Vec<PlanAnswerSpec> {
    vec![
        serde_json::from_value(
            json!({"group": {"edge": NEIGHBOR_EDGE}, "act": act, "reason": reason}),
        )
        .unwrap(),
    ]
}

fn propose(r: &mut Rdy, view: &str, summary: Option<&str>) -> ProposalInfo {
    r.propose(&writer(), view, view, summary)
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn pending(r: &Rdy) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
}

fn has_card(r: &Rdy, pref: &str) -> bool {
    pending(r)
        .items
        .iter()
        .any(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
}

fn plan_open(n: usize) -> Issue {
    Issue {
        severity: Severity::Error,
        code: E_PLAN_OPEN,
        block: None,
        message: format!("{n} change-plan items are open"),
        fix: PLAN_GATE_FIX.into(),
    }
}

/// The live items of `reply`: (id, cause, subject, kind, edge, state).
fn live(reply: &PlanReply) -> Vec<(String, String, String, PlanKind, String, ItemState)> {
    reply
        .items
        .iter()
        .filter_map(|row| match row {
            PlanRow::Live(i) => Some((
                i.id.clone(),
                i.cause.clone(),
                i.subject.clone(),
                i.kind,
                i.edge.clone(),
                i.state,
            )),
            _ => None,
        })
        .collect()
}

fn state_of(r: &Rdy, view: &str, item: &str) -> ItemState {
    let reply = r.read().view_plan(view).unwrap();
    live(&reply)
        .into_iter()
        .find(|i| i.0 == item)
        .unwrap_or_else(|| panic!("no item {item}: {reply:#?}"))
        .5
}

/// Every ref and the log end: what a refusal leaves as it was.
fn log_state(r: &Rdy) -> (Location, Vec<(String, Vec<u8>)>) {
    (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    )
}

/// Canon: a stable doc whose section holds claim `a`, claim `b` (which references `a`) and claim
/// `c` (nothing); a draft doc whose section holds `x`, `y` (which references `x`) and `z`.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    a: BlockId,
    b: BlockId,
    c: BlockId,
    x: BlockId,
    y: BlockId,
    z: BlockId,
}

fn world() -> World {
    let (dir, mut r) = project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$t", "body": "Y builds on {{$x}}."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$t", "body": "Z stands."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let get = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    World {
        a: get("$a"),
        b: get("$b"),
        c: get("$c"),
        x: get("$x"),
        y: get("$y"),
        z: get("$z"),
        _dir: dir,
        r,
    }
}

#[test]
fn nb8_stable_neighbors_need_answers_and_drafts_advise() {
    let mut w = world();
    let (a, b, c) = (w.a.clone(), w.b.clone(), w.c.clone());
    // stable: b's declared item answered, c's neighbor item open
    put_j(&mut w.r, "st", upd(&a, "A holds 31 days."), j_typo());
    let ab = pi(&a, &b, PlanKind::Confirm);
    let ac = pi(&a, &c, PlanKind::Confirm);
    w.r.answer_plan(&writer(), "st", &[confirm(&ab, "B still holds")])
        .unwrap();
    let reply = w.r.read().view_plan("st").unwrap();
    let neighbor: Vec<_> = live(&reply)
        .into_iter()
        .filter(|i| i.4 == NEIGHBOR_EDGE)
        .collect();
    assert_eq!(neighbor.len(), 1, "{reply:#?}");
    assert_eq!(
        (neighbor[0].0.as_str(), neighbor[0].3, neighbor[0].5),
        (ac.as_str(), PlanKind::Confirm, ItemState::Open)
    );
    let p = propose(&mut w.r, "st", Some(TEST_SUMMARY));
    assert!(!p.ready);
    assert_eq!(p.issues, vec![plan_open(1)], "{:#?}", p.issues);
    assert!(
        !has_card(&w.r, &p.r#ref),
        "no approval card while not ready"
    );
    let e = w.r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    assert_eq!(e.issues, vec![plan_open(1)]);
    // its answer makes the next version ready
    w.r.answer_plan(&writer(), "st", &[confirm(&ac, "C still stands")])
        .unwrap();
    let p2 = propose(&mut w.r, "st", Some(TEST_SUMMARY));
    assert!(p2.ready, "{:#?}", p2.issues);
    assert!(has_card(&w.r, &p2.r#ref));

    // drafts: the neighbor item is advice, counted in the warning, and the version is ready
    let (x, y, z) = (w.x.clone(), w.y.clone(), w.z.clone());
    write(&mut w.r, "dr", upd(&x, "X holds firm.")).unwrap();
    let p = propose(&mut w.r, "dr", Some(TEST_SUMMARY));
    assert!(p.ready, "{:#?}", p.issues);
    assert!(has_card(&w.r, &p.r#ref));
    let open: Vec<&Issue> = p
        .warnings
        .iter()
        .filter(|i| i.code == W_PLAN_OPEN)
        .collect();
    assert_eq!(open.len(), 1, "{:#?}", p.warnings);
    assert_eq!(open[0].message, "2 change-plan items are open");
    assert_eq!(open[0].fix, PLAN_OPEN_FIX);
    let mut ids: Vec<String> = p.plan.items.iter().map(|i| i.id.clone()).collect();
    ids.sort();
    let mut want = vec![pi(&x, &y, PlanKind::Confirm), pi(&x, &z, PlanKind::Confirm)];
    want.sort();
    assert_eq!(ids, want);
}

#[test]
fn nb8_neighbor_answers_lapse_with_their_existing_content_hashes() {
    let mut w = world();
    let (x, y, z) = (w.x.clone(), w.y.clone(), w.z.clone());
    let xz = pi(&x, &z, PlanKind::Confirm);
    let xy = pi(&x, &y, PlanKind::Confirm);
    write(&mut w.r, "w", upd(&x, "X holds firm.")).unwrap();
    w.r.answer_plan(&writer(), "w", &[confirm(&xz, "Z still stands")])
        .unwrap();
    assert_eq!(state_of(&w.r, "w", &xz), ItemState::Closed);
    let lapsed = |r: &Rdy, view: &str| -> Vec<String> {
        r.read()
            .view_plan(view)
            .unwrap()
            .notes
            .iter()
            .filter(|n| n.code == rdy_engine::plan::W_PLAN_ANSWER_LAPSED)
            .map(|n| n.message.clone())
            .collect()
    };
    // a pure order move of the subject in canon keeps its content identity: the answer stands
    let t = w.r.read().block(&Scope::Canon, &z).unwrap().parent.unwrap();
    write(
        &mut w.r,
        "mv",
        json!([{"op": "move", "id": z.to_string(), "parent": t.to_string(), "before": x.to_string()}]),
    )
    .unwrap();
    land(&mut w.r, "mv").unwrap();
    assert_eq!(state_of(&w.r, "w", &xz), ItemState::Closed);
    assert!(lapsed(&w.r, "w").is_empty());
    // the subject's text changes in canon: the answer lapses
    write(&mut w.r, "zc", upd(&z, "Z stands firm.")).unwrap();
    land(&mut w.r, "zc").unwrap();
    assert_eq!(state_of(&w.r, "w", &xz), ItemState::Open);
    assert_eq!(
        lapsed(&w.r, "w"),
        vec![format!("the answer to {xz} lapsed: {z} changed")]
    );
    // answered again, then the cause changes again: it lapses on the cause
    w.r.answer_plan(&writer(), "w", &[confirm(&xz, "Z still stands, again")])
        .unwrap();
    assert_eq!(state_of(&w.r, "w", &xz), ItemState::Closed);
    write(&mut w.r, "w", upd(&x, "X holds firmer.")).unwrap();
    assert_eq!(state_of(&w.r, "w", &xz), ItemState::Open);
    assert_eq!(
        lapsed(&w.r, "w"),
        vec![format!("the answer to {xz} lapsed: {x} changed")]
    );

    // the landing seals the neighbor item with its edge and outcome
    w.r.answer_plan(
        &writer(),
        "w",
        &[
            confirm(&xz, "Z still stands, at last"),
            confirm(&xy, "Y still builds on X"),
        ],
    )
    .unwrap();
    let p = propose(&mut w.r, "w", Some(TEST_SUMMARY));
    assert!(p.ready, "{:#?}", p.issues);
    let l: Hash = w.r.land(&approver(), &p.id).unwrap().parse().unwrap();
    let sealed = |r: &Rdy| {
        let st = r.read();
        let sh = st
            .reader()
            .plan_of_landing(&l, r.store().log_end())
            .unwrap()
            .unwrap();
        let plan = st.reader().plan(&sh).unwrap();
        let want = item_hash(&x, z.as_str(), PlanKind::Confirm);
        let item = plan
            .seal
            .as_ref()
            .unwrap()
            .items
            .iter()
            .find(|i| i.id == want)
            .cloned()
            .unwrap_or_else(|| panic!("no sealed neighbor item: {plan:#?}"));
        (item.edge.clone(), item.outcome)
    };
    let want = (NEIGHBOR_EDGE.to_string(), PlanOutcome::Confirmed);
    assert_eq!(sealed(&w.r), want);
    // through a reopen, a reindex, and a dump and load
    let dir = w._dir.path().to_path_buf();
    let World { _dir, r, .. } = w;
    r.close().unwrap();
    let mut r = Rdy::open(&dir).unwrap();
    assert_eq!(sealed(&r), want);
    r.reindex().unwrap();
    assert_eq!(sealed(&r), want);
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    let to = scratch.path().join("loaded");
    rdy_engine::load::load(&file, &to).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    assert_eq!(sealed(&loaded), want);
    loaded.close().unwrap();
    drop(_dir);
}

/// A draft doc whose one section holds `n` claims, landed; their ids in id order. With
/// `referrer`, a claim in another section references the first.
fn wide(n: usize, referrer: bool) -> (tempfile::TempDir, Rdy, Vec<String>) {
    let (dir, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "wide", "title": "Wide", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "All"}}),
        json!({"op": "create", "type": "section", "id": "$o", "parent": "$d", "fields": {"title": "Other"}}),
    ];
    for i in 0..n {
        ops.push(
            json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s",
            "body": format!("Claim {i} holds.")}),
        );
    }
    let w = write(&mut r, "seed", Value::Array(ops)).unwrap();
    let mut ids: Vec<String> = (0..n)
        .map(|i| w.created[&format!("$c{i}")].clone())
        .collect();
    ids.sort();
    if referrer {
        write(
            &mut r,
            "seed",
            json!([{"op": "create", "type": "claim", "parent": w.created["$o"],
                "body": format!("It builds on {{{{{}}}}}.", ids[0])}]),
        )
        .unwrap();
    }
    land(&mut r, "seed").unwrap();
    (dir, r, ids)
}

/// The ops of one write that edits the text of the first `k` of `ids`.
fn edits(ids: &[String], k: usize) -> Value {
    Value::Array(
        ids[..k]
            .iter()
            .map(|c| json!({"op": "update", "id": c, "body": format!("{c} holds anew.")}))
            .collect(),
    )
}

/// Edits the first `k` of `ids` in view `w` in one write; its change.
fn edit_first(r: &mut Rdy, ids: &[String], k: usize) -> String {
    write(r, "w", edits(ids, k)).unwrap().change
}

#[test]
fn nb8_neighbor_bounds_refuse_before_plan_or_answer_append() {
    assert_eq!(MAX_NEIGHBOR_PAIRS, 100_000);
    // at the bound: 250 edited claims, each with 400 unchanged section neighbors
    let (_d, mut r, ids) = wide(650, false);
    let change = edit_first(&mut r, &ids, 250);
    let c = r
        .read()
        .consequences(
            &Scope::View("w".into()),
            &change.parse().unwrap(),
            Some(1_000_000),
        )
        .unwrap();
    assert_eq!(c.items.len(), 400, "one entry per neighbor");
    let reply = r.read().view_plan("w").unwrap();
    assert_eq!(reply.items.len(), 100_000);
    let p = propose(&mut r, "w", Some(TEST_SUMMARY));
    assert!(p.ready, "{:#?}", p.issues);
    assert_eq!(p.plan.open, 100_000);
    drop((r, _d));

    // one past it: 250 times 401. The write stands and its reply reports the refusal; every
    // read refuses whole, and nothing is appended
    let (d, r, ids) = wide(651, false);
    let e = Local::new(r);
    let ops = edits(&ids, 250);
    let v = rdy_engine::consequence::write_and_report(&e, "w", None, move |r| write(r, "w", ops))
        .unwrap();
    assert!(v["change"].is_string(), "{v:#}");
    assert_eq!(v["consequences"], json!([]));
    assert_eq!(
        v["consequences_error"],
        "E_LIMIT: change exceeds 100000 neighbor consequences"
    );
    e.close().unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    let change = v["change"].as_str().unwrap().to_string();
    let refusal = |e: RdyError| {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "change exceeds 100000 neighbor consequences",
                "split the change into smaller proposals"
            )
        );
    };
    refusal(
        r.read()
            .consequences(&Scope::View("w".into()), &change.parse().unwrap(), None)
            .unwrap_err(),
    );
    refusal(r.read().view_plan("w").unwrap_err());
    let before = log_state(&r);
    refusal(
        r.propose(&writer(), "w", "w", Some(TEST_SUMMARY))
            .unwrap_err(),
    );
    assert_eq!(log_state(&r), before, "no proposal, no plan");
    let item = pi(&id(&ids[0]), &id(&ids[300]), PlanKind::Confirm);
    refusal(
        r.answer_plan(&writer(), "w", &[confirm(&item, "it holds")])
            .unwrap_err(),
    );
    assert_eq!(log_state(&r), before, "no answer");
    drop((r, d));

    // at the pair bound with one declared item more: the plan's item bound refuses
    let (_d, mut r, ids) = wide(650, true);
    edit_first(&mut r, &ids, 250);
    let before = log_state(&r);
    let e = r
        .propose(&writer(), "w", "w", Some(TEST_SUMMARY))
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "change plan exceeds 100000 plan items",
            "split the change into smaller proposals"
        )
    );
    assert_eq!(log_state(&r), before, "no proposal, no plan");
}

/// A pack whose claims carry a `checked` flag, with one blocking rule: every claim is checked.
const RULE_PACK: &str = "\
name: nb8
version: \"1\"
extends: rdy-standard
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      checked: { kind: bool }
rules:
  - name: checked-claims
    select: type = claim
    require: fields.checked = true
    severity: blocking
    message: checked-claims message
    fix: checked-claims fix
";

#[test]
fn nb8_answer_paths_preserve_m3d_rule_and_verdict_holds() {
    // a rule check: the grouped neighbor answer closes the neighbor item, never the rule
    let (d, r) = project();
    drop(r);
    rdy_qa::packs::use_pack(d.path(), "nb8", RULE_PACK, "");
    let mut r = Rdy::open(d.path()).unwrap();
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C holds.", "fields": {"checked": true}},
            {"op": "create", "type": "claim", "id": "$n", "parent": "$s", "body": "N stands.", "fields": {"checked": true}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let (c, n) = (id(&seeded.created["$c"]), id(&seeded.created["$n"]));
    put_j(
        &mut r,
        "rule",
        json!([{"op": "update", "id": c.to_string(), "body": "C holds firm.", "fields": {"checked": false}}]),
        j_typo(),
    );
    r.answer_plan_specs(
        &writer(),
        "rule",
        &neighbor_group("confirm", "N still stands"),
    )
    .unwrap();
    assert_eq!(
        state_of(&r, "rule", &pi(&c, &n, PlanKind::Confirm)),
        ItemState::Closed
    );
    let p = propose(&mut r, "rule", Some(TEST_SUMMARY));
    let codes: Vec<&str> = p.issues.iter().map(|i| i.code).collect();
    assert_eq!(codes, vec![E_RULE_CHECK], "{:#?}", p.issues);
    drop((r, d));

    // a summary: the grouped answer closes the items; a version with no summary is not ready
    let mut w = world();
    let (a, b, cc) = (w.a.clone(), w.b.clone(), w.c.clone());
    put_j(&mut w.r, "sum", upd(&a, "A holds 31 days."), j_typo());
    w.r.answer_plan(
        &writer(),
        "sum",
        &[confirm(&pi(&a, &b, PlanKind::Confirm), "B holds")],
    )
    .unwrap();
    w.r.answer_plan_specs(
        &writer(),
        "sum",
        &neighbor_group("confirm", "C still stands"),
    )
    .unwrap();
    assert_eq!(
        state_of(&w.r, "sum", &pi(&a, &cc, PlanKind::Confirm)),
        ItemState::Closed
    );
    let p = propose(&mut w.r, "sum", None);
    assert!(!p.ready);
    assert_eq!(p.issues.len(), 1, "{:#?}", p.issues);
    assert_eq!(p.issues[0].message, format!("{} has no summary", p.r#ref));

    // a tension answer: a grouped neighbor answer leaves a tension item open
    let (x, z) = (w.x.clone(), w.z.clone());
    let old = put_j(
        &mut w.r,
        "old",
        json!([
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "old", "title": "Old", "doc_type": "overview", "lifecycle": "deprecated"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$od", "fields": {"title": "Old rules"}},
            {"op": "create", "type": "claim", "id": "$o", "parent": "$os", "body": "O is the old rule."}
        ]),
        j_typo(),
    );
    land(&mut w.r, "old").unwrap();
    let o = old.created["$o"].clone();
    let ideas = w.r.read().block(&Scope::Canon, &x).unwrap().parent.unwrap();
    let t = put_j(
        &mut w.r,
        "tn",
        json!([{"op": "create", "type": "claim", "id": "$q", "parent": ideas.to_string(),
            "body": format!("Q follows {{{{{o}}}}}.")}]),
        j_typo(),
    );
    let q = id(&t.created["$q"]);
    w.r.answer_plan_specs(
        &writer(),
        "tn",
        &neighbor_group("confirm", "they still hold"),
    )
    .unwrap();
    let reply = w.r.read().view_plan("tn").unwrap();
    let items = live(&reply);
    assert!(
        items
            .iter()
            .filter(|i| i.4 == NEIGHBOR_EDGE)
            .all(|i| i.5 == ItemState::Closed && i.1 == q.to_string()),
        "{items:#?}"
    );
    assert!(
        items
            .iter()
            .any(|i| i.4 == NEIGHBOR_EDGE && i.2 == z.to_string())
    );
    let tension: Vec<_> = items.iter().filter(|i| i.3 == PlanKind::Tension).collect();
    assert_eq!(tension.len(), 1, "{items:#?}");
    assert_eq!(
        tension[0].5,
        ItemState::Open,
        "the tension still needs its answer"
    );

    // a deferral acceptance: deferred neighbor items wait for the person's card
    put_j(&mut w.r, "df", upd(&a, "A holds 32 days."), j_typo());
    w.r.answer_plan(
        &writer(),
        "df",
        &[confirm(&pi(&a, &b, PlanKind::Confirm), "B holds")],
    )
    .unwrap();
    w.r.answer_plan_specs(
        &writer(),
        "df",
        &neighbor_group("defer", "C waits for review"),
    )
    .unwrap();
    let p = propose(&mut w.r, "df", Some(TEST_SUMMARY));
    assert!(p.ready, "{:#?}", p.issues);
    let e = w.r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, E_DEFERRAL_OPEN, "{e}");
    assert!(accept_deferrals(&mut w.r, &approver(), &p.id, &p.r#ref).is_some());

    // an accepted verdict: the person's hold stands whatever the plan's answers
    put_j(&mut w.r, "vd", upd(&a, "A holds 33 days."), j_typo());
    w.r.answer_plan(
        &writer(),
        "vd",
        &[confirm(&pi(&a, &b, PlanKind::Confirm), "B holds")],
    )
    .unwrap();
    w.r.answer_plan_specs(
        &writer(),
        "vd",
        &neighbor_group("confirm", "C still stands"),
    )
    .unwrap();
    let p = propose(&mut w.r, "vd", Some(TEST_SUMMARY));
    assert!(p.ready, "{:#?}", p.issues);
    let World { _dir, r, .. } = w;
    let e = Local::new(r);
    let review = meaning_verdict(&e, &ag("vd"), &p.r#ref, "fail", &a);
    let accept = format!("accept_verdict:{review}:{}", p.id);
    let (pref, opt) = (p.r#ref.clone(), accept.clone());
    rdy_engine::engine::with_writer_result(&e, move |r| {
        r.answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: pref,
                option: opt,
                ..Default::default()
            },
        )
    })
    .unwrap();
    let id = p.id.clone();
    let err =
        rdy_engine::engine::with_writer_result(&e, move |r| r.land(&approver(), &id)).unwrap_err();
    assert_eq!(err.code, E_PROPOSAL_NOT_READY, "{err}");
    let held: Vec<&Issue> = err
        .issues
        .iter()
        .filter(|i| i.code == E_VALIDATION)
        .collect();
    assert_eq!(held.len(), 1, "{:#?}", err.issues);
    assert_eq!(
        held[0].message,
        format!(
            "meaning_preservation verdict {review} is accepted as blocking for {}",
            p.r#ref
        )
    );
    let _ = e.read();
}
