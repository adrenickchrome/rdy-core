//! rg5 (D-ME2HD, D-45RG6, D-FQ7AK, D-ERKCP; O5, O21, O22): each advisory tension that involves
//! a version is a change-plan item of kind `tension`, shown by the tension's own `tn-` id. The
//! author answers it `confirm` or `dismiss` with a reason; `defer` does not answer it, and
//! `dismiss` answers a tension item only. An answer keeps the tension's vkey as its cause, so it
//! lapses when the tension changes. Every open advisory tension that involves the version needs
//! that answer, an in-flight form too; a canon tension that the version's change resolves closes
//! as `updated`; a reviewer's dismissal leaves no item. A landing computes the tension items on
//! the version's recorded canon, so its seal holds what the propose showed, and a landing whose
//! seal would hold an open tension item is not ready.
//!
//! The gate (`plan_issues`): open non-code items on stable content and open tension items are
//! one `E_PLAN_OPEN`, counted over every item, past the summary cut; open non-code items on
//! drafts stay one `W_PLAN_OPEN`; an `update_implementation` item never counts.
mod common;

use rdy_engine::plan::lifecycle::plan_issues;
use rdy_engine::plan::{
    ItemState, LiveItem, PLAN_OPEN_FIX, PLAN_SUMMARY_ITEMS, PlanAnswerInput, PlanQuery,
    PlanSummary, W_PLAN_OPEN,
};
use rdy_engine::tensions::{Tension, TensionKind, tension_label};
use rdy_engine::{
    Coverage, E_BAD_INPUT, E_PLAN_ITEM, E_PLAN_OPEN, E_PROPOSAL_NOT_READY, ProposalInfo, Rdy,
    RdyError, Scope, Session, WriteRequest, WriteResult, parse_ops,
};
use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome};
use rdy_model::{EditorialClass, Justification, Role};
use rdy_schema::{Issue, Severity};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

const E_PLAN_OPEN_FIX: &str = "close or defer each item, and answer each tension item with confirm or dismiss: rdy_plan / rdy plan";

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn reviewer() -> Session {
    Session::agent("agent:rev".into(), Some("rev".into()), Role::Reviewer).unwrap()
}

fn request(view: &str, ops: Value, justification: Option<Justification>) -> WriteRequest {
    WriteRequest {
        view: view.into(),
        ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
        justification,
        message: format!("write to {view}"),
    }
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(&writer(), request(view, ops, None))
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn clarify() -> Option<Justification> {
    Some(Justification::Editorial {
        class: EditorialClass::Clarify,
        reason: "the rule moved".into(),
    })
}

/// A justified write: a seed that creates stable content (no unjustified change).
fn seed(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(&writer(), request("seed", ops, clarify()))
        .unwrap_or_else(|e| panic!("write the seed: {e}"))
}

/// A briefed, justified edit of stable block `id` in `view` (a fresh view: canon's briefing).
fn edit_stable(r: &mut Rdy, view: &str, id: &str, body: &str) -> WriteResult {
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![id.to_string()],
                path: None,
            },
            None,
        )
        .unwrap_or_else(|e| panic!("brief {id}: {e}"));
    b.register(&r.briefs());
    r.write_briefed(
        &writer(),
        request(
            view,
            json!([{"op": "update", "id": id, "body": body}]),
            clarify(),
        ),
        Some(b.base.token.clone()),
    )
    .unwrap_or_else(|e| panic!("edit {id} in {view}: {e}"))
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    r.propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) -> String {
    let p = propose(r, view);
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e} {:?}", e.issues))
}

fn answer(
    r: &mut Rdy,
    view: &str,
    item: &str,
    act: PlanAct,
    reason: Option<&str>,
) -> Result<(), RdyError> {
    r.answer_plan(
        &writer(),
        view,
        &[PlanAnswerInput {
            item: item.into(),
            act,
            reason: reason.map(str::to_string),
        }],
    )
    .map(|_| ())
}

fn whole(r: &Rdy, view: &str) -> Value {
    let p = r
        .read()
        .change_plan(
            &PlanQuery {
                view: Some(view.into()),
                max_tokens: Some(100_000),
                ..Default::default()
            },
            "work",
        )
        .unwrap();
    serde_json::to_value(p).unwrap()
}

fn rows_of_kind<'v>(plan: &'v Value, kind: &str) -> Vec<&'v Value> {
    plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .collect()
}

/// The deprecated-reference tensions of `view`'s landing form.
fn deprecated_refs(r: &Rdy, view: &str) -> Vec<Arc<Tension>> {
    let st = r.read();
    let changes = st.included(view);
    st.landing_tensions(view, &changes)
        .unwrap()
        .into_iter()
        .filter(|t| t.kind == TensionKind::DeprecatedReference)
        .collect()
}

fn one_tension(r: &Rdy, view: &str) -> Arc<Tension> {
    let mut all = deprecated_refs(r, view);
    assert_eq!(all.len(), 1, "{all:#?}");
    all.pop().unwrap()
}

fn tension_items(s: &PlanSummary) -> Vec<&LiveItem> {
    s.items
        .iter()
        .filter(|i| i.kind == PlanKind::Tension)
        .collect()
}

fn codes(issues: &[Issue]) -> Vec<&'static str> {
    issues.iter().map(|i| i.code).collect()
}

/// Canon: a stable doc (claim `a` and claim `p`, which states a's literal and references it), a
/// draft doc (the same pair, `da` and `dp`, and a section `n` to write into), and a deprecated
/// doc holding claim `x`.
struct Fx {
    _dir: tempfile::TempDir,
    r: Rdy,
    a: String,
    da: String,
    n: String,
    x: String,
    legacy: String,
    lx: String,
}

fn project() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = seed(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Wait 30 days per {{$a}}."},
            {"op": "create", "type": "doc", "id": "$nd", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$n", "parent": "$nd", "fields": {"title": "Drafts"}},
            {"op": "create", "type": "claim", "id": "$da", "parent": "$n", "body": "Drafts stay open for 45 hours."},
            {"op": "create", "type": "claim", "id": "$dp", "parent": "$n", "body": "Close each draft after 45 hours, as {{$da}} says."},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "old", "title": "Old", "doc_type": "overview", "lifecycle": "deprecated"}},
            {"op": "create", "type": "section", "id": "$o", "parent": "$od", "fields": {"title": "Old rules"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$o", "body": "X is the old rule."},
            {"op": "create", "type": "doc", "id": "$ld", "fields": {"name": "legacy", "title": "Legacy", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$l", "parent": "$ld", "fields": {"title": "Legacy rules"}},
            {"op": "create", "type": "claim", "id": "$lx", "parent": "$l", "body": "LX is a legacy rule."}
        ]),
    );
    land(&mut r, "seed");
    let id = |k: &str| w.created[k].clone();
    Fx {
        a: id("$a"),
        da: id("$da"),
        n: id("$n"),
        x: id("$x"),
        legacy: id("$ld"),
        lx: id("$lx"),
        _dir: dir,
        r,
    }
}

/// View `view` writes a draft claim that references the deprecated claim `x`: one advisory
/// deprecated-reference tension of the version.
fn cite_deprecated(fx: &mut Fx, view: &str, body: &str) -> String {
    let w = put(
        &mut fx.r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": fx.n, "body": format!("{body} {{{{{}}}}}.", fx.x)}]),
    );
    w.created["$y"].clone()
}

#[test]
fn rg5_an_advisory_tension_of_the_version_is_a_tension_item_shown_by_its_tn_id() {
    let mut fx = project();
    cite_deprecated(&mut fx, "a", "Y follows");
    let t = one_tension(&fx.r, "a");
    assert_eq!(t.severity.to_string(), "advisory");
    let tn = tension_label(&t.id);
    let cause = t.blocks()[0].to_string();

    // the view's plan lists it by its tn- id, with the tension id as subject (O5)
    let plan = whole(&fx.r, "a");
    let rows = rows_of_kind(&plan, "tension");
    assert_eq!(
        rows,
        [
            &json!({"id": tn, "kind": "tension", "subject": t.id.to_string(), "cause": cause,
                 "edge": "tension", "state": "open"})
        ],
        "{plan:#}"
    );
    assert_eq!(
        plan["counts"]["by_kind"]["tension"],
        json!({"open": 1, "closed": 0, "deferred": 0})
    );

    // the propose summary shows the same item, open
    let info = propose(&mut fx.r, "a");
    let items = tension_items(&info.plan);
    assert_eq!(items.len(), 1, "{:#?}", info.plan);
    assert_eq!(items[0].id, tn);
    assert_eq!(items[0].subject, t.id.to_string());
    assert_eq!(items[0].state, ItemState::Open);
    let by_kind = serde_json::to_value(&info.plan).unwrap()["by_kind"].clone();
    assert_eq!(
        by_kind["tension"],
        json!({"open": 1, "closed": 0, "deferred": 0})
    );
}

#[test]
fn rg5_a_tension_item_takes_confirm_or_dismiss_with_a_reason() {
    let mut fx = project();
    cite_deprecated(&mut fx, "a", "Y follows");
    let tn = tension_label(&one_tension(&fx.r, "a").id);

    // defer does not answer it
    let e = answer(&mut fx.r, "a", &tn, PlanAct::Defer, Some("later")).unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM, "{e}");
    assert_eq!(e.message, "defer does not answer a tension item");
    assert_eq!(e.fix, "answer it with confirm or dismiss, with a reason");
    // done does not answer it either
    let e = answer(&mut fx.r, "a", &tn, PlanAct::Done, None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.message, "done does not answer a tension item");
    // confirm and dismiss each need a reason
    for act in [PlanAct::Confirm, PlanAct::Dismiss] {
        let e = answer(&mut fx.r, "a", &tn, act, None).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(
            e.message,
            format!("{} needs a reason of 1 to 2000 characters", act.as_str())
        );
    }

    // confirm with a reason closes it
    answer(
        &mut fx.r,
        "a",
        &tn,
        PlanAct::Confirm,
        Some("the old rule still guides Y"),
    )
    .unwrap();
    let plan = whole(&fx.r, "a");
    let row = rows_of_kind(&plan, "tension")[0].clone();
    assert_eq!(row["id"], tn);
    assert_eq!(row["state"], "closed");
    assert_eq!(row["answer"]["act"], "confirm");
    assert_eq!(row["answer"]["reason"], "the old rule still guides Y");

    // dismiss with a reason replaces it, and closes it too
    answer(
        &mut fx.r,
        "a",
        &tn,
        PlanAct::Dismiss,
        Some("Y cites it as history"),
    )
    .unwrap();
    let plan = whole(&fx.r, "a");
    let row = rows_of_kind(&plan, "tension")[0].clone();
    assert_eq!(row["state"], "closed");
    assert_eq!(row["answer"]["act"], "dismiss");
    assert_eq!(
        plan["counts"]["by_kind"]["tension"],
        json!({"open": 0, "closed": 1, "deferred": 0})
    );
    // the propose summary has no open tension item, and the gate has nothing
    let info = propose(&mut fx.r, "a");
    assert!(tension_items(&info.plan).is_empty(), "{:#?}", info.plan);
    assert_eq!(plan_issues(&info.plan), vec![]);
}

#[test]
fn rg5_dismiss_answers_only_a_tension_item() {
    let mut fx = project();
    let da = fx.da.clone();
    put(
        &mut fx.r,
        "a",
        json!([{"op": "update", "id": da, "body": "Drafts stay open for 90 hours."}]),
    );
    let plan = whole(&fx.r, "a");
    let update = rows_of_kind(&plan, "update");
    assert_eq!(update.len(), 1, "{plan:#}");
    let item = update[0]["id"].as_str().unwrap().to_string();
    let e = answer(&mut fx.r, "a", &item, PlanAct::Dismiss, Some("no")).unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM, "{e}");
    assert_eq!(e.message, "dismiss answers a tension item only");
    assert_eq!(e.fix, "answer it with confirm, defer or done");
    // the refusal wrote nothing: the item is still open
    assert_eq!(
        rows_of_kind(&whole(&fx.r, "a"), "update")[0]["state"],
        "open"
    );
}

#[test]
fn rg5_an_open_item_on_stable_content_is_e_plan_open() {
    let mut fx = project();
    let a = fx.a.clone();
    edit_stable(&mut fx.r, "a", &a, "A holds 60 days.");
    let info = propose(&mut fx.r, "a");
    assert_eq!(info.plan.open, 1, "{:#?}", info.plan);
    assert_eq!(info.plan.items[0].kind, PlanKind::Update);
    let issues = plan_issues(&info.plan);
    assert_eq!(
        issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_PLAN_OPEN,
            block: None,
            message: "1 change-plan items are open".into(),
            fix: E_PLAN_OPEN_FIX.into(),
        }]
    );
    assert!(!codes(&issues).contains(&W_PLAN_OPEN));
}

#[test]
fn rg5_an_open_item_on_a_draft_stays_w_plan_open() {
    let mut fx = project();
    let da = fx.da.clone();
    put(
        &mut fx.r,
        "a",
        json!([{"op": "update", "id": da, "body": "Drafts stay open for 90 hours."}]),
    );
    let info = propose(&mut fx.r, "a");
    assert_eq!(info.plan.open, 1, "{:#?}", info.plan);
    let issues = plan_issues(&info.plan);
    assert_eq!(
        issues,
        vec![Issue {
            severity: Severity::Warning,
            code: W_PLAN_OPEN,
            block: None,
            message: "1 change-plan items are open".into(),
            fix: PLAN_OPEN_FIX.into(),
        }]
    );
    assert!(!codes(&issues).contains(&E_PLAN_OPEN));
}

#[test]
fn rg5_an_unanswered_tension_item_blocks_a_draft_too() {
    let mut fx = project();
    cite_deprecated(&mut fx, "a", "Y follows");
    let info = propose(&mut fx.r, "a");
    assert_eq!(tension_items(&info.plan).len(), 1, "{:#?}", info.plan);
    assert!(
        info.plan.items.iter().all(|i| i.kind == PlanKind::Tension),
        "the draft claim owes nothing else: {:#?}",
        info.plan
    );
    let issues = plan_issues(&info.plan);
    assert_eq!(
        issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_PLAN_OPEN,
            block: None,
            message: "1 change-plan items are open".into(),
            fix: E_PLAN_OPEN_FIX.into(),
        }]
    );
}

#[test]
fn rg5_the_gate_counts_items_past_the_summary_cut() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let n = PLAN_SUMMARY_ITEMS + 5;
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}),
        json!({"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."}),
    ];
    for i in 0..n {
        ops.push(
            json!({"op": "create", "type": "claim", "id": format!("$p{i}"), "parent": "$s",
                        "body": format!("Rule {i}: wait 30 days per {{{{$a}}}}.")}),
        );
    }
    let w = seed(&mut r, Value::Array(ops));
    land(&mut r, "seed");
    let a = w.created["$a"].clone();
    edit_stable(&mut r, "a", &a, "A holds 60 days.");
    let info = propose(&mut r, "a");
    assert_eq!(info.plan.open, n, "{:#?}", info.plan.by_kind);
    assert_eq!(info.plan.items.len(), PLAN_SUMMARY_ITEMS);
    assert_eq!(info.plan.items_omitted, n - PLAN_SUMMARY_ITEMS);
    let issues = plan_issues(&info.plan);
    assert_eq!(codes(&issues), [E_PLAN_OPEN]);
    assert_eq!(issues[0].message, format!("{n} change-plan items are open"));
}

// ---- code items (D-ERKCP) ----

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fresh checkout linked to the project at `project` as code repo `app`.
fn linked_repo(project: &Path) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "--initial-branch=main"]);
    let cfg = project.join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
    text.push_str("code_repos: [app]\n");
    std::fs::write(&cfg, text).unwrap();
    std::fs::write(
        project.join(".rdy/local.yaml"),
        format!(
            "code_paths:\n  app: {}\n",
            serde_json::to_string(&tmp.path().display().to_string()).unwrap()
        ),
    )
    .unwrap();
    tmp
}

#[test]
fn rg5_an_update_implementation_item_never_counts() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let repo = linked_repo(dir.path());
    let w = seed(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."}
        ]),
    );
    land(&mut r, "seed");
    let a = w.created["$a"].clone();
    std::fs::write(repo.path().join("f.rs"), "start\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(
        repo.path(),
        &[
            "commit",
            "-q",
            "--no-verify",
            "-m",
            &format!("start\n\nImplements: {a}"),
        ],
    );
    edit_stable(&mut r, "a", &a, "A holds 60 days.");
    let info = propose(&mut r, "a");
    assert_eq!(info.plan.open, 1, "{:#?}", info.plan);
    assert_eq!(info.plan.items[0].kind, PlanKind::UpdateImplementation);
    assert_eq!(info.plan.code.files, 1);
    // on stable content, open, and still no issue of the gate
    assert_eq!(plan_issues(&info.plan), vec![]);
}

// ---- lapses, closes and dismissals ----

#[test]
fn rg5_a_tension_answer_lapses_when_the_tension_changes() {
    let mut fx = project();
    cite_deprecated(&mut fx, "a", "Y follows");
    let t = one_tension(&fx.r, "a");
    let tn = tension_label(&t.id);
    fx.r.answer_plan(
        &writer(),
        "a",
        &[PlanAnswerInput {
            item: tn.clone(),
            act: PlanAct::Confirm,
            reason: Some("the old rule still guides Y".into()),
        }],
    )
    .unwrap();
    let plan = whole(&fx.r, "a");
    let recorded: rdy_model::Hash = plan_answers_head(&fx.r, "a");
    let stored = fx.r.read().reader().plan(&recorded).unwrap();
    assert_eq!(stored.answers.len(), 1);
    // O5: the answer keeps no subject content and the tension's vkey as its cause
    assert_eq!(stored.answers[0].subject, None);
    assert_eq!(stored.answers[0].cause, t.vkey);
    assert_eq!(rows_of_kind(&plan, "tension")[0]["state"], "closed");

    // the deprecated side changes: the tension changes with it, and the answer no longer
    // answers it
    let x = fx.x.clone();
    put(
        &mut fx.r,
        "a",
        json!([{"op": "update", "id": x, "body": "X is the older rule."}]),
    );
    let t2 = one_tension(&fx.r, "a");
    assert_ne!(t2.vkey, t.vkey);
    let plan = whole(&fx.r, "a");
    let rows = rows_of_kind(&plan, "tension");
    assert_eq!(rows.len(), 1, "{plan:#}");
    assert_eq!(rows[0]["id"], tension_label(&t2.id));
    assert_eq!(rows[0]["state"], "open");
    assert!(rows[0].get("answer").is_none(), "{plan:#}");
    // the open tension item is the one gated item (the edit of x owes draft items besides)
    let info = propose(&mut fx.r, "a");
    let issues = plan_issues(&info.plan);
    assert_eq!(issues[0].code, E_PLAN_OPEN, "{issues:?}");
    assert_eq!(issues[0].message, "1 change-plan items are open");
}

/// The head of `view`'s plan chain (`plan/<view>`).
fn plan_answers_head(r: &Rdy, view: &str) -> rdy_model::Hash {
    let b = r
        .read()
        .reader()
        .get_ref(&format!("plan/{view}"))
        .unwrap()
        .expect("a plan ref");
    rdy_model::Hash(b.as_slice().try_into().expect("a 32-byte ref"))
}

/// The change plan of a landing (its seal) or of a proposal version, as `change_plan` shows it.
fn plan_of(r: &Rdy, query: PlanQuery) -> Value {
    let p = r
        .read()
        .change_plan(
            &PlanQuery {
                max_tokens: Some(100_000),
                ..query
            },
            "work",
        )
        .unwrap();
    serde_json::to_value(p).unwrap()
}

fn sealed(r: &Rdy, landing: &str) -> Value {
    plan_of(
        r,
        PlanQuery {
            landing: Some(landing.to_string()),
            ..Default::default()
        },
    )
}

fn version_plan(r: &Rdy, version: &str) -> Value {
    plan_of(
        r,
        PlanQuery {
            proposal: Some(version.to_string()),
            ..Default::default()
        },
    )
}

/// The deprecated-reference tensions of `view`'s in-flight forms.
fn inflight_refs(r: &Rdy, view: &str) -> Vec<Arc<Tension>> {
    let st = r.read();
    st.tensions_of_changes(&st.included(view))
        .unwrap()
        .into_iter()
        .filter(|t| t.kind == TensionKind::DeprecatedReference)
        .collect()
}

/// View `b` deprecates the legacy doc; view `a` writes draft claim `$y` citing its claim `lx`.
fn cite_legacy(fx: &mut Fx, view: &str) -> String {
    let (lx, n) = (fx.lx.clone(), fx.n.clone());
    let w = put(
        &mut fx.r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": n, "body": format!("Y follows {{{{{lx}}}}}.")}]),
    );
    w.created["$y"].clone()
}

fn deprecate_legacy(fx: &mut Fx, view: &str) {
    let legacy = fx.legacy.clone();
    put(
        &mut fx.r,
        view,
        json!([{"op": "update", "id": legacy, "fields": {"lifecycle": "deprecated"}}]),
    );
}

#[test]
fn rg5_a_tension_the_version_resolves_seals_as_updated() {
    let mut fx = project();
    // canon holds a draft claim z that cites the deprecated claim x: a canon tension
    let z = cite_deprecated(&mut fx, "c", "Z follows");
    let tn_c = tension_label(&one_tension(&fx.r, "c").id);
    answer(
        &mut fx.r,
        "c",
        &tn_c,
        PlanAct::Confirm,
        Some("x still guides z for now"),
    )
    .unwrap();
    land(&mut fx.r, "c");
    let canon: Vec<Arc<Tension>> =
        fx.r.read()
            .canon_tensions()
            .unwrap()
            .into_iter()
            .filter(|t| t.kind == TensionKind::DeprecatedReference)
            .collect();
    assert_eq!(canon.len(), 1, "{canon:#?}");
    let tn = tension_label(&canon[0].id);

    // view a rewrites z so it no longer cites x: its landing form has no such tension
    put(
        &mut fx.r,
        "a",
        json!([{"op": "update", "id": z, "body": "Z follows the new rule."}]),
    );
    assert!(deprecated_refs(&fx.r, "a").is_empty());
    assert!(inflight_refs(&fx.r, "a").is_empty());

    // the version's plan closes the canon tension as updated: the author owes no answer
    let plan = whole(&fx.r, "a");
    let rows = rows_of_kind(&plan, "tension");
    assert_eq!(rows.len(), 1, "{plan:#}");
    assert_eq!(rows[0]["id"], tn);
    assert_eq!(rows[0]["subject"], canon[0].id.to_string());
    assert_eq!(rows[0]["state"], "closed");
    assert_eq!(rows[0]["how"], "updated");
    let info = propose(&mut fx.r, "a");
    assert!(tension_items(&info.plan).is_empty(), "{:#?}", info.plan);

    // the landing seals it as updated
    common::confirm_exceptions(&mut fx.r, &approver(), &info);
    let landing = fx.r.land(&approver(), &info.id).unwrap();
    let seal = sealed(&fx.r, &landing);
    let rows = rows_of_kind(&seal, "tension");
    assert_eq!(rows.len(), 1, "{seal:#}");
    assert_eq!(rows[0]["id"], tn);
    assert_eq!(rows[0]["outcome"], PlanOutcome::Updated.as_str());
    assert!(fx.r.store().features().has("plan_tension"));
}

#[test]
fn rg5_an_in_flight_tension_needs_the_authors_answer() {
    let mut fx = project();
    // view b deprecates the legacy doc in flight; view a cites its claim
    deprecate_legacy(&mut fx, "b");
    cite_legacy(&mut fx, "a");
    // the tension is open in flight (b's deprecation); the version's landing form has none
    assert!(deprecated_refs(&fx.r, "a").is_empty());
    let inflight = inflight_refs(&fx.r, "a");
    assert_eq!(inflight.len(), 1, "{inflight:#?}");
    let tn = tension_label(&inflight[0].id);

    // it involves the version, so it is an item that waits for the author's answer
    let plan = whole(&fx.r, "a");
    let rows = rows_of_kind(&plan, "tension");
    assert_eq!(rows.len(), 1, "{plan:#}");
    assert_eq!(rows[0]["id"], tn);
    assert_eq!(rows[0]["state"], "open");
    assert!(rows[0].get("how").is_none(), "{plan:#}");
    let info = propose(&mut fx.r, "a");
    assert_eq!(tension_items(&info.plan).len(), 1, "{:#?}", info.plan);
    let issues = plan_issues(&info.plan);
    assert_eq!(codes(&issues), [E_PLAN_OPEN], "{issues:?}");
    assert_eq!(issues[0].message, "1 change-plan items are open");

    // after the author's confirm the version is ready, and the seal holds the confirm
    answer(
        &mut fx.r,
        "a",
        &tn,
        PlanAct::Confirm,
        Some("y keeps the legacy rule until b lands"),
    )
    .unwrap();
    let info = propose(&mut fx.r, "a");
    assert!(tension_items(&info.plan).is_empty(), "{:#?}", info.plan);
    assert_eq!(plan_issues(&info.plan), vec![]);
    common::confirm_exceptions(&mut fx.r, &approver(), &info);
    let landing = fx.r.land(&approver(), &info.id).unwrap();
    let seal = sealed(&fx.r, &landing);
    let rows = rows_of_kind(&seal, "tension");
    assert_eq!(rows.len(), 1, "{seal:#}");
    assert_eq!(rows[0]["id"], tn);
    assert_eq!(rows[0]["outcome"], PlanOutcome::Confirmed.as_str());
}

#[test]
fn rg5_a_landing_seals_the_tension_set_the_propose_showed() {
    let mut fx = project();
    // view a writes a draft claim citing lx (not deprecated yet) and a draft edit, and proposes:
    // no tension item
    let da = fx.da.clone();
    put(
        &mut fx.r,
        "a",
        json!([{"op": "update", "id": da, "body": "Drafts stay open for 90 hours."}]),
    );
    let y = cite_legacy(&mut fx, "a");
    let info = propose(&mut fx.r, "a");
    assert!(tension_items(&info.plan).is_empty(), "{:#?}", info.plan);

    // view b then deprecates the legacy doc and lands: it touches neither y nor lx
    deprecate_legacy(&mut fx, "b");
    land(&mut fx.r, "b");

    // a's version keeps the tension items of its recorded canon: none
    let plan = version_plan(&fx.r, &info.r#ref);
    assert!(rows_of_kind(&plan, "tension").is_empty(), "{plan:#}");

    // a lands, and its seal has no tension item
    common::confirm_exceptions(&mut fx.r, &approver(), &info);
    let landing =
        fx.r.land(&approver(), &info.id)
            .unwrap_or_else(|e| panic!("land a: {e} {:?}", e.issues));
    let seal = sealed(&fx.r, &landing);
    assert!(rows_of_kind(&seal, "tension").is_empty(), "{seal:#}");
    assert_eq!(rows_of_kind(&seal, "update").len(), 1, "{seal:#}");

    // the tension the later landing made is canon's now
    let canon: Vec<Arc<Tension>> =
        fx.r.read()
            .canon_tensions()
            .unwrap()
            .into_iter()
            .filter(|t| t.kind == TensionKind::DeprecatedReference)
            .collect();
    assert_eq!(canon.len(), 1, "{canon:#?}");
    assert!(
        canon[0].blocks().iter().any(|b| b.to_string() == y),
        "{canon:#?}"
    );
}

/// A version whose seal would hold an unanswered tension item does not land (D-45RG6): the
/// store has no open seal form for it, and the landing writes nothing.
#[test]
fn rg5_an_unanswered_tension_item_refuses_the_landing() {
    let mut fx = project();
    let da = fx.da.clone();
    put(
        &mut fx.r,
        "a",
        json!([{"op": "update", "id": da, "body": "Drafts stay open for 90 hours."}]),
    );
    cite_deprecated(&mut fx, "a", "Y follows");
    let tn = tension_label(&one_tension(&fx.r, "a").id);
    assert_eq!(rows_of_kind(&whole(&fx.r, "a"), "tension")[0]["id"], tn);
    let info = propose(&mut fx.r, "a");
    common::confirm_exceptions(&mut fx.r, &approver(), &info);
    let seq = fx.r.read().seq();
    let e = fx.r.land(&approver(), &info.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    assert_eq!(e.message, format!("{} is not ready: 1 issue", info.r#ref));
    assert_eq!(
        e.issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_PLAN_OPEN,
            block: None,
            message: "1 change-plan items are open".into(),
            fix: E_PLAN_OPEN_FIX.into(),
        }]
    );
    assert_eq!(fx.r.read().seq(), seq, "the refused landing wrote nothing");
    assert!(!fx.r.store().features().has("plan_tension"));
}

#[test]
fn rg5_a_reviewer_dismissal_removes_the_item() {
    let mut fx = project();
    cite_deprecated(&mut fx, "a", "Y follows");
    let tn = tension_label(&one_tension(&fx.r, "a").id);
    assert_eq!(rows_of_kind(&whole(&fx.r, "a"), "tension").len(), 1);
    fx.r.dismiss_tension(&reviewer(), &tn, "Y cites it as history")
        .unwrap();
    let plan = whole(&fx.r, "a");
    assert!(rows_of_kind(&plan, "tension").is_empty(), "{plan:#}");
    let info = propose(&mut fx.r, "a");
    assert!(tension_items(&info.plan).is_empty(), "{:#?}", info.plan);
    assert_eq!(plan_issues(&info.plan), vec![]);
}
