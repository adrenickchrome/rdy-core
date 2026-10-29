//! rg8 (D-ME2HD, D-3JZP4, D-FQ7AK, D-45RG6, D-AAQ9Z; O3, O23): the one readiness function. A
//! version that fails is not ready: rdy lists each issue with its fix, in the closed order,
//! raises no approval request, and refuses an approval of it inside `E_PROPOSAL_NOT_READY`. On
//! stable content an open change-plan item is the issue `E_PLAN_OPEN`; on drafts it stays the
//! warning `W_PLAN_OPEN`. A stable change with no briefing is not ready, unless the log holds it
//! before the first briefed change or `rdy import` wrote it.
use rdy_engine::brief::E_BRIEF_REQUIRED;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, Pending, PendingQuery};
use rdy_engine::plan::lifecycle::{PLAN_GATE_FIX, PLAN_OPEN_FIX, W_PLAN_OPEN};
use rdy_engine::readiness::check::BRIEFING_FIX;
use rdy_engine::readiness::{
    Candidate, PlanSource, candidate_builds_for_tests, readiness, readiness_calls_for_tests,
};
use rdy_engine::{
    DecisionInput, E_BLOCKING_TENSION, E_PLAN_OPEN, E_PROPOSAL_NOT_READY, ProposalInfo, Rdy, Scope,
};
use rdy_model::plan::PlanAct;
use rdy_model::{ActorRun, BlockId, Change, ChangeLinks, Hash, Op};
use rdy_qa::fixtures::{approver, land, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items, confirm_before_landing};
use rdy_qa::tensions::{ag, j_typo, put, put_j, try_briefed, upd};
use rdy_schema::{Issue, Severity};
use serde_json::json;

/// Canon: a stable doc whose claim `b` references claim `a`, and a draft doc whose claim `y`
/// references claim `x`.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    s: BlockId,
    a: BlockId,
    x: BlockId,
    /// The draft doc and its section.
    n: BlockId,
    t: BlockId,
}

fn world() -> World {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$t", "body": "Y builds on {{$x}}."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    World {
        s: id("$s"),
        a: id("$a"),
        x: id("$x"),
        n: id("$n"),
        t: id("$t"),
        _dir: dir,
        r,
    }
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    r.propose(&writer(), view, view, Some(TEST_SUMMARY))
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

/// The issues the queue lists for the version `pref`, which is not ready.
fn not_ready(r: &Rdy, pref: &str) -> Vec<Issue> {
    let q = pending(r);
    q.not_ready
        .into_iter()
        .find(|n| n.r#ref == pref)
        .unwrap_or_else(|| panic!("{pref} is not ready"))
        .issues
}

fn codes(issues: &[Issue]) -> Vec<&str> {
    issues.iter().map(|i| i.code).collect()
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

/// A change that updates block `id` (as canon holds it) to `body`, written straight through the
/// store with no briefing: the way no product writer writes a stable change. `imported` sets
/// `links.imported_by`, as `rdy import` does. Returns the change.
fn raw_update(r: &mut Rdy, view: &str, id: &BlockId, body: &str, imported: bool) -> Hash {
    let mut b = r.read().block(&Scope::Canon, id).unwrap();
    let base = b.version_hash();
    b.body = body.into();
    let h = r
        .store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops: vec![Op::Update { base, block: b }],
                deps: vec![],
                provenance: writer().provenance(),
                justification: j_typo(),
                message: "raw".into(),
                links: ChangeLinks {
                    imported_by: imported.then(|| ActorRun {
                        actor: "human:qa".into(),
                        run: Some("import-1".into()),
                    }),
                    ..ChangeLinks::default()
                },
                read_identity: None,
            },
        )
        .expect("the raw change");
    r.invalidate();
    h
}

/// View `view` exists with one draft change.
fn draft_change(w: &mut World, view: &str) {
    put(&mut w.r, view, upd(&w.x, &format!("X holds in {view}.")));
}

#[test]
fn rg8_a_not_ready_version_has_no_card_and_lists_each_issue() {
    let mut w = world();
    // unjustified on stable content: a blocking tension; b references a: an open stable item
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), None);
    let p = propose(&mut w.r, "a");
    let q = pending(&w.r);
    assert!(
        q.items
            .iter()
            .all(|i| i.r#ref != p.r#ref && i.batch.as_deref() != Some(p.r#ref.as_str())),
        "{q:#?}"
    );
    let issues = not_ready(&w.r, &p.r#ref);
    assert_eq!(
        codes(&issues),
        vec![E_BLOCKING_TENSION, E_PLAN_OPEN],
        "{issues:#?}"
    );
    assert_eq!(issues[1], plan_open(1));
}

#[test]
fn rg8_a_stable_change_with_no_briefing_is_not_ready() {
    let mut w = world();
    // a briefed stable change: the store's first
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    assert!(
        w.r.read()
            .reader()
            .first_briefed_change()
            .unwrap()
            .is_some()
    );
    draft_change(&mut w, "b");
    let raw = raw_update(&mut w.r, "b", &w.a, "A holds 32 days.", false);
    answer_open_items(&mut w.r, &writer(), "b");
    let p = propose(&mut w.r, "b");
    let issues = not_ready(&w.r, &p.r#ref);
    assert_eq!(
        issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_BRIEF_REQUIRED,
            block: Some(w.a.clone()),
            message: format!(
                "change {} touches stable content ({}) and records no briefing",
                raw.short(),
                w.a
            ),
            fix: BRIEFING_FIX.into(),
        }]
    );
    assert_eq!(
        BRIEFING_FIX,
        "exclude the change from the view, then brief the block and write it again"
    );
}

#[test]
fn rg8_a_change_before_the_first_briefed_change_is_exempt() {
    let mut w = world();
    draft_change(&mut w, "b");
    assert!(
        w.r.read()
            .reader()
            .first_briefed_change()
            .unwrap()
            .is_none()
    );
    let raw = raw_update(&mut w.r, "b", &w.a, "A holds 32 days.", false);
    // the first briefed change comes after it
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    let first = w.r.read().reader().first_briefed_change().unwrap().unwrap();
    assert!(w.r.read().reader().change_location(&raw).unwrap() < first);
    answer_open_items(&mut w.r, &writer(), "b");
    let p = propose(&mut w.r, "b");
    let q = pending(&w.r);
    assert!(
        q.not_ready.iter().all(|n| n.r#ref != p.r#ref),
        "{:#?}",
        q.not_ready
    );
    assert!(q.items.iter().any(|i| i.r#ref == p.r#ref), "{q:#?}");
}

#[test]
fn rg8_an_imported_change_is_exempt() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    draft_change(&mut w, "b");
    let raw = raw_update(&mut w.r, "b", &w.a, "A holds 32 days.", true);
    let first = w.r.read().reader().first_briefed_change().unwrap().unwrap();
    assert!(w.r.read().reader().change_location(&raw).unwrap() > first);
    answer_open_items(&mut w.r, &writer(), "b");
    let p = propose(&mut w.r, "b");
    let q = pending(&w.r);
    assert!(
        q.not_ready.iter().all(|n| n.r#ref != p.r#ref),
        "{:#?}",
        q.not_ready
    );
    assert!(q.items.iter().any(|i| i.r#ref == p.r#ref), "{q:#?}");
}

#[test]
fn rg8_an_approval_refuses_with_e_plan_open_inside_e_proposal_not_ready() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    let p = propose(&mut w.r, "a");
    let seq = w.r.read().seq();
    let e = w.r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    assert_eq!(e.message, format!("{} is not ready: 1 issue", p.r#ref));
    assert_eq!(
        e.fix,
        "fix each issue and propose again: rdy_propose / rdy propose"
    );
    assert_eq!(e.issues, vec![plan_open(1)]);
    assert_eq!(w.r.read().seq(), seq, "the refused approval wrote nothing");
}

#[test]
fn rg8_w_plan_open_warns_on_drafts_only() {
    let mut w = world();
    let (a, x) = (w.a.to_string(), w.x.to_string());
    put_j(
        &mut w.r,
        "a",
        json!([
            {"op": "update", "id": a, "body": "A holds 31 days."},
            {"op": "update", "id": x, "body": "X holds twice."}
        ]),
        j_typo(),
    );
    let p = propose(&mut w.r, "a");
    let warned: Vec<&Issue> = p
        .warnings
        .iter()
        .filter(|i| i.code == W_PLAN_OPEN)
        .collect();
    assert_eq!(
        warned,
        vec![&Issue {
            severity: Severity::Warning,
            code: W_PLAN_OPEN,
            block: None,
            message: "1 change-plan items are open".into(),
            fix: PLAN_OPEN_FIX.into(),
        }],
        "{:#?}",
        p.warnings
    );
    assert_eq!(not_ready(&w.r, &p.r#ref), vec![plan_open(1)]);
}

#[test]
fn rg8_a_rejected_decision_is_still_a_later_refusal() {
    let (_d, mut r, b) = rdy_qa::intake::batch_project();
    let q = pending(&r);
    let one_way = q
        .items
        .iter()
        .find(|i| {
            i.kind == ItemKind::Confirm
                && i.batch.as_deref() == Some(b.r#ref.as_str())
                && i.r#ref.starts_with(&format!("{}@", b.one_way))
        })
        .expect("the one_way exception")
        .r#ref
        .clone();
    let info = r
        .answer(
            &approver(),
            PERSON,
            rdy_engine::answer::AnswerRequest {
                r#ref: one_way,
                option: "reject".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(info.act, "reject_decision");
    let issues = not_ready(&r, &b.r#ref);
    let want = format!(
        "{} was rejected; drop or revise it, then propose again",
        b.one_way
    );
    assert!(issues.iter().any(|i| i.message == want), "{issues:#?}");
    // the version's own checks pass: the rejection is its only issue
    assert_eq!(issues.len(), 1, "{issues:#?}");
}

#[test]
fn rg8_readiness_on_an_in_hand_plan_equals_readiness_on_the_stored_one() {
    let mut w = world();
    // a not-ready version (an open stable item), and a ready one
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    let open = propose(&mut w.r, "a");
    draft_change(&mut w, "b");
    let ready = propose(&mut w.r, "b");
    let prefs = [&open, &ready].map(|p| (p, w.r.resolve_proposal(&p.r#ref).unwrap()));
    let st = w.r.read();
    for (p, pref) in prefs {
        let record = st.reader().proposal(&pref.version).unwrap();
        let stored = Candidate::stored(&st, pref, record.clone()).unwrap();
        let PlanSource::Bound(Some(h)) = stored.plan else {
            panic!("{} binds a plan", p.r#ref)
        };
        let in_hand = Candidate {
            pref,
            p: record,
            plan: PlanSource::InHand {
                plan: st.reader().plan(&h).unwrap(),
                hash: h,
            },
        };
        let (calls, builds) = (readiness_calls_for_tests(), candidate_builds_for_tests());
        let a = readiness(&st, &stored).unwrap();
        assert_eq!(readiness_calls_for_tests(), calls + 1);
        assert_eq!(candidate_builds_for_tests(), builds + 1);
        let b = readiness(&st, &in_hand).unwrap();
        assert_eq!(a.issues, b.issues, "{}", p.r#ref);
        assert_eq!(a.summary, b.summary, "{}", p.r#ref);
        let card = |c: &Option<rdy_engine::pending::Item>| {
            c.as_ref()
                .map(|i| (i.r#ref.clone(), i.title.clone(), i.facts.clone()))
        };
        assert_eq!(card(&a.card), card(&b.card), "{}", p.r#ref);
        if p.r#ref == open.r#ref {
            assert_eq!(a.issues, vec![plan_open(1)]);
            assert!(a.card.is_none());
            assert_eq!(a.summary.open, 1, "{:#?}", a.summary);
        } else {
            assert!(a.issues.is_empty(), "{:#?}", a.issues);
            assert_eq!(
                a.card.as_ref().map(|c| (c.kind, c.r#ref.clone())),
                Some((ItemKind::Approve, ready.r#ref.clone()))
            );
        }
    }
}

#[test]
fn rg8_answer_open_items_makes_a_stable_version_ready() {
    let mut w = world();
    // an accepted decision that cites a: a change of a makes it suspect (a reaffirm item)
    let (s, a) = (w.s.to_string(), w.a.to_string());
    try_briefed(&mut w.r, "dv", |r, brief| {
        r.decide(
            &ag("dv"),
            "dv",
            DecisionInput {
                parent: s.clone(),
                statement: "A rules the wait".into(),
                body: format!("Per {{{{{a}}}}}."),
                brief,
                ..Default::default()
            },
        )
    })
    .unwrap();
    land(&mut w.r, "dv").unwrap();
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    let answers = answer_open_items(&mut w.r, &writer(), "a");
    let acts: Vec<PlanAct> = answers.iter().map(|i| i.act).collect();
    assert!(acts.contains(&PlanAct::Confirm), "{answers:#?}");
    assert!(acts.contains(&PlanAct::Defer), "{answers:#?}");
    assert!(
        answers
            .iter()
            .all(|i| i.reason.as_deref() == Some("test: checked")),
        "{answers:#?}"
    );
    let p = propose(&mut w.r, "a");
    let q = pending(&w.r);
    assert!(
        q.not_ready.iter().all(|n| n.r#ref != p.r#ref),
        "{:#?}",
        q.not_ready
    );
    // the update item is deferred: its deferral card stands beside the batch
    let card = format!("defer:{}", p.r#ref);
    assert!(
        q.items
            .iter()
            .any(|i| i.kind == ItemKind::Defer && i.r#ref == card),
        "{q:#?}"
    );
    let accepted = confirm_before_landing(&mut w.r, &approver(), &p);
    assert!(accepted.is_empty(), "the version writes no decision");
    assert!(
        pending(&w.r).items.iter().all(|i| i.r#ref != card),
        "the deferral card is accepted"
    );
    w.r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {}: {e}", p.r#ref));
    // a view with no open item: the helper writes nothing
    draft_change(&mut w, "c");
    let seq = w.r.read().seq();
    assert!(answer_open_items(&mut w.r, &writer(), "c").is_empty());
    assert_eq!(w.r.read().seq(), seq);
}

/// The version `pref` is ready: the queue lists no `not_ready` row for it, and its approve item
/// stands.
fn assert_ready(r: &Rdy, pref: &str) {
    let q = pending(r);
    assert!(
        q.not_ready.iter().all(|n| n.r#ref != pref),
        "{pref}: {:#?}",
        q.not_ready
    );
    assert!(q.items.iter().any(|i| i.r#ref == pref), "{pref}: {q:#?}");
}

/// The briefing change `change` records.
fn briefing_of(r: &Rdy, change: &str) -> Option<Hash> {
    r.read()
        .reader()
        .change(&change.parse().unwrap())
        .unwrap()
        .links
        .briefing
}

/// Replays change `src` into a new view `view`: the replay.
fn replay(r: &mut Rdy, view: &str, src: Hash) -> Hash {
    r.view_create(&writer(), view, None, false).unwrap();
    let res = r.view_replay(&writer(), view, &[src.to_string()]).unwrap();
    assert_eq!(res.replayed.len(), 1, "{res:?}");
    res.replayed[0].change.parse().unwrap()
}

/// Ruling task-8 F2: check 1 judges a change on the state it was written on. The version's own
/// earlier change made section `s` a draft, so the later edit of `a` under it touches no stable
/// content, as the write funnel judged it.
#[test]
fn rg8_a_block_the_version_made_draft_needs_no_briefing() {
    let mut w = world();
    let s = w.s.to_string();
    let demote = put_j(
        &mut w.r,
        "v",
        json!([{"op": "update", "id": s, "fields": {"lifecycle": "draft"}}]),
        j_typo(),
    );
    assert!(briefing_of(&w.r, &demote.change).is_some());
    let edit = put_j(&mut w.r, "v", upd(&w.a, "A holds 40 days."), j_typo());
    assert_eq!(briefing_of(&w.r, &edit.change), None, "a is a draft in v");
    answer_open_items(&mut w.r, &writer(), "v");
    let p = propose(&mut w.r, "v");
    assert_ready(&w.r, &p.r#ref);
}

/// Ruling task-8 F2: a block the version's own earlier change moved out of a stable doc is a
/// draft for its later edit.
#[test]
fn rg8_a_block_moved_out_of_a_stable_doc_needs_no_briefing() {
    let mut w = world();
    let (a, t) = (w.a.to_string(), w.t.to_string());
    let moved = put_j(
        &mut w.r,
        "v",
        json!([{"op": "update", "id": a, "move_to": {"parent": t}}]),
        j_typo(),
    );
    assert!(briefing_of(&w.r, &moved.change).is_some());
    let edit = put_j(&mut w.r, "v", upd(&w.a, "A holds 40 days."), j_typo());
    assert_eq!(briefing_of(&w.r, &edit.change), None, "a is a draft in v");
    answer_open_items(&mut w.r, &writer(), "v");
    let p = propose(&mut w.r, "v");
    assert_ready(&w.r, &p.r#ref);
}

/// Ruling task-8 F2: a draft doc that canon makes stable after a change was written does not
/// make that change need a briefing: the change is judged on the canon it was written on.
#[test]
fn rg8_a_doc_canon_made_stable_after_the_write_needs_no_briefing() {
    let mut w = world();
    // the store's first briefed change, in a view that stays in flight
    put_j(&mut w.r, "z", upd(&w.a, "A holds 31 days."), j_typo());
    let edit = put_j(&mut w.r, "v", upd(&w.x, "X holds twice."), j_typo());
    assert_eq!(briefing_of(&w.r, &edit.change), None, "x is a draft");
    // another view makes the notes doc stable, and lands
    let n = w.n.to_string();
    put_j(
        &mut w.r,
        "m",
        json!([{"op": "update", "id": n, "fields": {"lifecycle": "stable"}}]),
        j_typo(),
    );
    land(&mut w.r, "m").unwrap();
    let h: Hash = edit.change.parse().unwrap();
    assert!(w.r.read().included("v").contains(&h), "v keeps its change");
    answer_open_items(&mut w.r, &writer(), "v");
    let p = propose(&mut w.r, "v");
    assert_ready(&w.r, &p.r#ref);
}

/// Ruling task-8 F2: the issue's fix names the way out (exclude the change, brief, write it
/// again), and following it makes the version ready. The guidance row says it too.
#[test]
fn rg8_the_brief_required_fix_names_exclude() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    draft_change(&mut w, "b");
    let raw = raw_update(&mut w.r, "b", &w.a, "A holds 32 days.", false);
    answer_open_items(&mut w.r, &writer(), "b");
    let p = propose(&mut w.r, "b");
    let issues = not_ready(&w.r, &p.r#ref);
    assert_eq!(codes(&issues), vec![E_BRIEF_REQUIRED], "{issues:#?}");
    assert_eq!(
        issues[0].fix,
        "exclude the change from the view, then brief the block and write it again"
    );
    let row = rdy_mcp::tools::ERROR_ROWS
        .iter()
        .find(|(c, _, _)| *c == E_BRIEF_REQUIRED)
        .unwrap();
    assert_eq!(
        row.2,
        "retry with the token of the briefing the error carries\non a readiness issue: exclude the change (rdy_view exclude), rdy_brief the block, and write it again"
    );
    // the fix works
    w.r.view_exclude(&ag("b"), "b", &[raw.to_string()], false)
        .unwrap();
    let again = put_j(&mut w.r, "b", upd(&w.a, "A holds 32 days."), j_typo());
    assert!(briefing_of(&w.r, &again.change).is_some());
    answer_open_items(&mut w.r, &writer(), "b");
    let p = propose(&mut w.r, "b");
    assert_ready(&w.r, &p.r#ref);
}

/// Ruling task-8 F3: exemption (d) follows `replayed_from` to the chain's root. R comes before
/// the first briefed change (exempt by (c)); C1 replays R and C2 replays C1: both are exempt.
#[test]
fn rg8_a_replay_of_an_exempt_replay_is_exempt() {
    let mut w = world();
    draft_change(&mut w, "b");
    let raw = raw_update(&mut w.r, "b", &w.a, "A holds 32 days.", false);
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    let c1 = replay(&mut w.r, "c", raw);
    let c2 = replay(&mut w.r, "d", c1);
    let st = w.r.read();
    let reader = st.reader();
    let first = reader.first_briefed_change().unwrap().unwrap();
    assert!(reader.change_location(&raw).unwrap() < first);
    assert!(reader.change_location(&c1).unwrap() > first);
    assert_eq!(reader.change(&c2).unwrap().links.replayed_from, Some(c1));
    assert_eq!(reader.change(&c1).unwrap().links.replayed_from, Some(raw));
    drop(st);
    for view in ["c", "d"] {
        answer_open_items(&mut w.r, &writer(), view);
        let p = propose(&mut w.r, view);
        assert_ready(&w.r, &p.r#ref);
    }
}

/// Ruling task-8 F3: exemption (d) follows at most 64 steps. C64's root is 64 steps away (exempt);
/// C65's is 65 (not exempt: it needs a briefing). Each view but the last two gives its replay up,
/// so the in-flight set stays small.
#[test]
fn rg8_a_replay_chain_past_64_is_not_exempt() {
    let mut w = world();
    draft_change(&mut w, "b");
    let raw = raw_update(&mut w.r, "b", &w.a, "A holds 32 days.", false);
    w.r.view_exclude(&ag("b"), "b", &[raw.to_string()], false)
        .unwrap();
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    let mut src = raw;
    for k in 1..=65 {
        let next = replay(&mut w.r, &format!("r{k}"), src);
        if (2..=64).contains(&k) {
            let prev = format!("r{}", k - 1);
            w.r.view_exclude(&writer(), &prev, &[src.to_string()], false)
                .unwrap();
        }
        src = next;
    }
    answer_open_items(&mut w.r, &writer(), "r64");
    let p64 = propose(&mut w.r, "r64");
    assert_ready(&w.r, &p64.r#ref);
    answer_open_items(&mut w.r, &writer(), "r65");
    let p65 = propose(&mut w.r, "r65");
    let issues = not_ready(&w.r, &p65.r#ref);
    assert_eq!(codes(&issues), vec![E_BRIEF_REQUIRED], "{issues:#?}");
    assert!(
        issues[0]
            .message
            .starts_with(&format!("change {} ", src.short())),
        "{issues:#?}"
    );
}

/// Finding 4 (fix round 1): readiness reads the plan in hand, not the one the store binds. The
/// author answers the version's open item on the view after the propose: the view's plan chain
/// gets a head the version does not bind. On that plan in hand the version is ready; on the
/// bound plan it is not.
#[test]
fn rg8_readiness_follows_an_in_hand_plan_the_store_does_not_bind() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.a, "A holds 31 days."), j_typo());
    let open = propose(&mut w.r, "a");
    let answers = answer_open_items(&mut w.r, &writer(), "a");
    assert_eq!(answers.len(), 1, "{answers:#?}");
    let pref = w.r.resolve_proposal(&open.r#ref).unwrap();
    let st = w.r.read();
    let record = st.reader().proposal(&pref.version).unwrap();
    let stored = Candidate::stored(&st, pref, record.clone()).unwrap();
    let PlanSource::Bound(Some(bound)) = stored.plan else {
        panic!("{} binds a plan", open.r#ref)
    };
    let head = st.reader().get_ref("plan/a").unwrap().expect("a plan ref");
    let head = Hash(head.as_slice().try_into().expect("a 32-byte ref"));
    assert_ne!(head, bound, "the answer is a new plan version");
    let in_hand = Candidate {
        pref,
        p: record,
        plan: PlanSource::InHand {
            plan: st.reader().plan(&head).unwrap(),
            hash: head,
        },
    };
    assert_eq!(readiness(&st, &stored).unwrap().issues, vec![plan_open(1)]);
    let r = readiness(&st, &in_hand).unwrap();
    assert!(r.issues.is_empty(), "{:#?}", r.issues);
    assert_eq!(r.summary.open, 0, "{:#?}", r.summary);
    assert_eq!(
        r.card.map(|c| (c.kind, c.r#ref)),
        Some((ItemKind::Approve, open.r#ref.clone()))
    );
}
