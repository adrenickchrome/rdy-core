//! Change-plan reads and answers (D-4JMPH; reference/mcp-tools p-92F5M, p-CSBA7, p-JJNYH,
//! p-PZX3T): the four selectors and their closed reply shapes, atomic answer-only plan versions
//! that change no block, content-only lapses, computed closes before answers, the act matrix,
//! the write guards, stable pages, recorded proposal state and the O6 chain end.
mod common;
#[path = "support/plan_frames.rs"]
mod plan_frames;

use plan_frames::put_plan_frames;
use rdy_engine::plan::{PlanAnswerInput, PlanQuery, W_PLAN_ANSWER_LAPSED, check_plan_request};
use rdy_engine::{
    Coverage, E_BAD_INPUT, E_LIMIT, E_PAGE_STALE, E_PERMISSION, E_PLAN_ITEM, E_VIEW_OWNER,
    ProposalInfo, Rdy, RdyError, Scope, Session, WriteRequest, WriteResult, parse_ops,
};
use rdy_model::plan::{
    Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem, item_display,
    item_hash,
};
use rdy_model::{BlockId, EditorialClass, Hash, Justification, Provenance, Role};
use rdy_store::kinds;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

// ---- sessions and writes ----

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn reader() -> Session {
    Session::human("human:qa".into(), Role::Reader).unwrap()
}

fn agent(run: &str) -> Session {
    Session::agent("agent:qa".into(), Some(run.into()), Role::Writer).unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

/// A project whose decisions have one tracking field, `disposition` (trk1).
fn trk_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}

fn put_as(r: &mut Rdy, s: &Session, view: &str, ops: Value) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    put_as(r, &writer(), view, ops)
}

fn edit(r: &mut Rdy, view: &str, target: &str, body: &str) -> WriteResult {
    put(
        r,
        view,
        json!([{"op": "update", "id": target, "body": body}]),
    )
}

/// Proposes `view` and lands it; returns the version and the landing.
fn land(r: &mut Rdy, view: &str) -> (ProposalInfo, String) {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &approver(), &p);
    let l = r.land(&approver(), &p.id).unwrap();
    (p, l)
}

/// Canon: doc `spec`, section `Rules` and claims a ("A holds 30 days."), c ("C stands."), b,
/// which references a and c, and p, which states a's literal. Each reference pins its target.
struct Spec {
    sec: String,
    a: String,
    b: String,
    c: String,
    p: String,
}

fn spec(r: &mut Rdy) -> Spec {
    let w = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."},
            {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Wait 30 days per {{$a}}."}
        ]),
    );
    land(r, "seed");
    let get = |k: &str| w.created[k].clone();
    Spec {
        sec: get("$s"),
        a: get("$a"),
        b: get("$b"),
        c: get("$c"),
        p: get("$p"),
    }
}

/// A version of `view` as a store-16 engine proposed it (the raw store writer: every engine
/// propose binds a plan): its hash and `P-x@1`.
fn raw_propose(r: &mut Rdy, view: &str) -> (Hash, String) {
    let v = r.store_mut().propose(view, view, prov()).unwrap();
    r.invalidate();
    (v, format!("P-{}@1", v.short()))
}

/// Lands `version` through the raw store writer, which seals no plan; returns the landing.
fn raw_land(r: &mut Rdy, version: &Hash) -> String {
    let l = r
        .store_mut()
        .land(version, approver().provenance())
        .unwrap();
    r.invalidate();
    l.to_string()
}

// ---- plan reads ----

fn view_q(v: &str) -> PlanQuery {
    PlanQuery {
        view: Some(v.into()),
        ..Default::default()
    }
}

fn plan_of(r: &Rdy, q: &PlanQuery) -> Result<Value, RdyError> {
    r.read()
        .change_plan(q, "work")
        .map(|p| serde_json::to_value(p).unwrap())
}

fn plan(r: &Rdy, q: &PlanQuery) -> Value {
    plan_of(r, q).unwrap_or_else(|e| panic!("plan {q:?}: {e}"))
}

/// The whole plan of `view`, uncut.
fn whole(r: &Rdy, view: &str) -> Value {
    let v = plan(
        r,
        &PlanQuery {
            max_tokens: Some(100_000),
            ..view_q(view)
        },
    );
    assert!(v.get("next").is_none(), "{v}");
    v
}

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(&id(cause), subject, kind))
}

fn row<'v>(v: &'v Value, item: &str) -> &'v Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item)
        .unwrap_or_else(|| panic!("no item {item}: {v:#}"))
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

fn counts(open: usize, closed: usize, deferred: usize, by_kind: &[(&str, [usize; 3])]) -> Value {
    let mut k = serde_json::Map::new();
    for name in [
        "confirm",
        "reaffirm",
        "rejustify",
        "update",
        "update_implementation",
    ] {
        let [o, c, d] = by_kind
            .iter()
            .find(|(n, _)| *n == name)
            .map_or([0, 0, 0], |(_, x)| *x);
        k.insert(name.into(), json!({"open": o, "closed": c, "deferred": d}));
    }
    json!({"open": open, "closed": closed, "deferred": deferred, "by_kind": k})
}

fn ans(item: &str, act: PlanAct, reason: Option<&str>) -> PlanAnswerInput {
    PlanAnswerInput {
        item: item.into(),
        act,
        reason: reason.map(str::to_string),
    }
}

fn answer(r: &mut Rdy, view: &str, answers: &[PlanAnswerInput]) -> Hash {
    r.answer_plan(&writer(), view, answers)
        .unwrap_or_else(|e| panic!("answer {view}: {e}"))
}

fn expect_err(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

/// Every ref and the log end: what a refused answer must leave as it was.
fn log_state(r: &Rdy) -> (rdy_engine::Location, Vec<(String, Vec<u8>)>) {
    (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    )
}

fn plan_ref(r: &Rdy, view: &str) -> Option<Hash> {
    r.read()
        .reader()
        .get_ref(&rdy_store::plan::plan_ref(view))
        .unwrap()
        .map(|v| Hash(v.as_slice().try_into().unwrap()))
}

fn prov() -> Provenance {
    Provenance {
        actor: "agent:fixture".into(),
        run: Some("fixture".into()),
        role: Role::Writer,
        at_ms: 1,
    }
}

// ---- git ----

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

/// Writes `path` (its content names the message), commits with `message`; returns the commit.
fn commit(dir: &Path, path: &str, message: &str) -> String {
    std::fs::write(dir.join(path), format!("{message}\n")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
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

// ---- tests ----

/// O17: every selector's reply has exactly scope, plan, items, counts, code and notes; a live
/// item is {id, kind, subject, cause, edge, state}, a sealed one has outcome, a decision's rows
/// are {landing, version, items, outcomes} newest first, and counts name all five kinds.
#[test]
fn cp8_all_four_selectors_have_closed_shapes() {
    let (_d, mut r) = project();
    // nothing yet: the session view's empty plan
    let empty = plan(&r, &PlanQuery::default());
    assert_eq!(
        empty,
        json!({"scope": "view:work", "plan": null, "items": [], "counts": counts(0, 0, 0, &[]),
               "code": {"files": 0, "unavailable": true}, "notes": []})
    );
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let v = plan(&r, &view_q("w"));
    assert_eq!(
        keys(&v),
        ["code", "counts", "items", "notes", "plan", "scope"]
    );
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let update = pi(&s.a, &s.p, PlanKind::Update);
    let mut want = vec![
        json!({"id": confirm, "kind": "confirm", "subject": s.b, "cause": s.a, "edge": "references", "state": "open"}),
        json!({"id": update, "kind": "update", "subject": s.p, "cause": s.a, "edge": "references", "state": "open"}),
    ];
    want.sort_by_key(|i| {
        let (c, sub, k) = if i["kind"] == "confirm" {
            (&s.a, &s.b, PlanKind::Confirm)
        } else {
            (&s.a, &s.p, PlanKind::Update)
        };
        item_hash(&id(c), sub, k)
    });
    assert_eq!(
        v,
        json!({"scope": "view:w", "plan": null, "items": want,
               "counts": counts(2, 0, 0, &[("confirm", [1, 0, 0]), ("update", [1, 0, 0])]),
               "code": {"files": 0, "unavailable": true}, "notes": []})
    );

    // a proposal version: its own scope, the same items, no plan bound (O19): the raw store
    // writer, as a store-16 engine proposed (every engine propose binds a plan)
    let (version, pref) = raw_propose(&mut r, "w");
    let pv = plan(
        &r,
        &PlanQuery {
            proposal: Some(pref.clone()),
            ..Default::default()
        },
    );
    assert_eq!(
        pv,
        json!({"scope": format!("proposal:{pref}"), "plan": null, "items": v["items"],
               "counts": v["counts"], "code": v["code"], "notes": []})
    );

    // a landing with no seal: no items and no plan (the raw store landing, which seals nothing)
    let l = raw_land(&mut r, &version);
    let lq = PlanQuery {
        landing: Some(l.clone()),
        ..Default::default()
    };
    assert_eq!(
        plan(&r, &lq),
        json!({"scope": format!("landing:{l}"), "plan": null, "items": [],
               "counts": counts(0, 0, 0, &[]), "code": {"files": 0}, "notes": []})
    );

    // a sealed plan, as the landing composition records it
    let landing: Hash = l.parse().unwrap();
    let accepted = Hash([7; 32]);
    let sealed = |subject: &str, kind: PlanKind, edge: &str, outcome, answer| SealedItem {
        id: item_hash(&id(&s.a), subject, kind),
        kind,
        subject: subject.into(),
        cause: id(&s.a),
        edge: edge.into(),
        outcome,
        answer,
    };
    let mut items = vec![
        sealed(
            &s.b,
            PlanKind::Confirm,
            "references",
            PlanOutcome::Deferred,
            Some(accepted),
        ),
        sealed(
            &s.p,
            PlanKind::Update,
            "references",
            PlanOutcome::Updated,
            None,
        ),
        sealed(
            "app:f.rs",
            PlanKind::UpdateImplementation,
            "implements",
            PlanOutcome::Open,
            None,
        ),
    ];
    items.sort_by_key(|i| i.id);
    let seal = Plan {
        stable: None,
        prev: None,
        view: "w".into(),
        proposal: Some(version),
        answers: vec![],
        heads: vec![PlanHead {
            repo: "app".into(),
            head: Some("a".repeat(40)),
        }],
        provenance: prov(),
        seal: Some(PlanSeal {
            landing,
            version,
            decisions: vec![id(&s.a)],
            items: items.clone(),
        }),
    };
    let h = put_plan_frames(r.store_mut(), &seal);
    r.invalidate();
    let rows: Vec<Value> = items
        .iter()
        .map(|i| {
            let mut row = json!({"id": item_display(&i.id), "kind": i.kind.as_str(),
                "subject": i.subject, "cause": s.a, "edge": i.edge, "outcome": i.outcome.as_str()});
            if let Some(a) = i.answer {
                row["answer"] = json!(a.to_string());
            }
            row
        })
        .collect();
    let sealed_counts = counts(
        1,
        1,
        1,
        &[
            ("confirm", [0, 0, 1]),
            ("update", [0, 1, 0]),
            ("update_implementation", [1, 0, 0]),
        ],
    );
    assert_eq!(
        plan(&r, &lq),
        json!({"scope": format!("landing:{l}"), "plan": h.to_string(), "items": rows,
               "counts": sealed_counts, "code": {"files": 1}, "notes": []})
    );

    // a decision: its seals newest first, each with all ten outcomes counted
    edit(&mut r, "w2", &s.c, "C stands firm.");
    let (version2, _) = raw_propose(&mut r, "w2");
    let l2 = raw_land(&mut r, &version2);
    let landing2: Hash = l2.parse().unwrap();
    let seal2 = Plan {
        view: "w2".into(),
        proposal: Some(version2),
        heads: vec![],
        seal: Some(PlanSeal {
            landing: landing2,
            version: version2,
            decisions: vec![id(&s.a)],
            items: vec![],
        }),
        ..seal.clone()
    };
    put_plan_frames(r.store_mut(), &seal2);
    r.invalidate();
    let outcomes = |deferred: usize, updated: usize, open: usize| {
        json!({"committed": 0, "confirmed": 0, "deferred": deferred, "done": 0, "open": open,
               "reaffirmed": 0, "relinked": 0, "repinned": 0, "retired": 0, "updated": updated})
    };
    let dq = PlanQuery {
        decision: Some(s.a.clone()),
        ..Default::default()
    };
    assert_eq!(
        plan(&r, &dq),
        json!({"scope": format!("decision:{}", s.a), "plan": null, "items": [
                   {"landing": l2, "version": version2.to_string(), "items": 0, "outcomes": outcomes(0, 0, 0)},
                   {"landing": l, "version": version.to_string(), "items": 3, "outcomes": outcomes(1, 1, 1)}
               ],
               "counts": sealed_counts, "code": {"files": 0}, "notes": []})
    );
    // a decision no seal names
    assert_eq!(
        plan(
            &r,
            &PlanQuery {
                decision: Some(s.c.clone()),
                ..Default::default()
            }
        ),
        json!({"scope": format!("decision:{}", s.c), "plan": null, "items": [],
               "counts": counts(0, 0, 0, &[]), "code": {"files": 0}, "notes": []})
    );
    // a seal recorded with no linked checkout reads its code as unavailable
    assert_eq!(
        plan(
            &r,
            &PlanQuery {
                landing: Some(l2.clone()),
                ..Default::default()
            }
        )["code"],
        json!({"files": 0, "unavailable": true})
    );
}

/// The atomic answer: 299 valid answers and one refused write nothing; 300 valid ones write one
/// answer-only plan version and its ref, and no block, change, proposal, view or canon ref.
#[test]
fn cp8_answers_are_atomic_and_do_not_write_blocks() {
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}),
        json!({"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."}),
    ];
    for i in 0..300 {
        ops.push(
            json!({"op": "create", "type": "claim", "id": format!("$n{i}"), "parent": "$s",
                        "body": format!("N{i} builds on {{{{$a}}}}.")}),
        );
    }
    let w = put(&mut r, "seed", Value::Array(ops));
    land(&mut r, "seed");
    let a = w.created["$a"].clone();
    edit(&mut r, "w", &a, "A holds firmly.");
    let v = whole(&r, "w");
    let ids: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            assert_eq!(i["kind"], "confirm");
            i["id"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(ids.len(), 300);
    let st = r.read();
    let before = (
        log_state(&r),
        st.canon_head(),
        st.view_head("w").cloned().map(|h| h.head),
        st.included("w").to_vec(),
    );
    drop(st);
    let mut answers: Vec<PlanAnswerInput> = ids[..299]
        .iter()
        .map(|i| ans(i, PlanAct::Confirm, Some("it still holds")))
        .collect();
    answers.push(ans(&ids[299], PlanAct::Done, None));
    let e = r.answer_plan(&writer(), "w", &answers).unwrap_err();
    expect_err(
        e,
        E_BAD_INPUT,
        "done does not answer a confirm item",
        "read the plan and use an act the item accepts",
    );
    let st = r.read();
    assert_eq!(
        (
            log_state(&r),
            st.canon_head(),
            st.view_head("w").cloned().map(|h| h.head),
            st.included("w").to_vec(),
        ),
        before,
        "a refused request writes nothing"
    );
    drop(st);
    assert_eq!(plan_ref(&r, "w"), None);

    answers[299] = ans(&ids[299], PlanAct::Confirm, Some("it still holds"));
    let m_before = r
        .read()
        .materialized(&Scope::View("w".into()))
        .unwrap()
        .versions()
        .map(|(i, v)| (i.clone(), *v))
        .collect::<Vec<_>>();
    let h = answer(&mut r, "w", &answers);
    assert_eq!(plan_ref(&r, "w"), Some(h));
    let frames: Vec<u8> = r.store().last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(frames, [kinds::PLAN, kinds::REF_UPDATE]);
    let st = r.read();
    assert_eq!(st.canon_head(), before.1);
    assert_eq!(st.view_head("w").cloned().map(|h| h.head), before.2);
    assert_eq!(st.included("w").to_vec(), before.3);
    let m_after = st
        .materialized(&Scope::View("w".into()))
        .unwrap()
        .versions()
        .map(|(i, v)| (i.clone(), *v))
        .collect::<Vec<_>>();
    assert_eq!(m_after, m_before, "no block moved");
    let p = st.reader().plan(&h).unwrap();
    assert_eq!(
        (p.stable, p.prev, p.proposal, p.seal),
        (None, None, None, None)
    );
    assert_eq!(p.view, "w");
    assert_eq!(p.answers.len(), 300);
    assert!(p.answers.windows(2).all(|w| w[0].item < w[1].item));
    assert!(p.answers.iter().all(|a| a.act == PlanAct::Confirm
        && a.reason.as_deref() == Some("it still holds")
        && a.by.actor == "human:qa"));
    drop(st);
    let v = whole(&r, "w");
    assert_eq!(v["plan"], h.to_string());
    assert_eq!(v["counts"], counts(0, 300, 0, &[("confirm", [0, 300, 0])]));
}

/// p-0HXT0: an answer keeps its subject's and cause's content hashes. A tracking-field edit and
/// a repin change neither, so the answer holds; a rewrite of the subject or the cause lapses
/// it, naming the subject when it changed and else the cause. A code subject has no hash: only
/// its cause lapses it.
#[test]
fn cp8_answer_lapses_on_subject_or_cause_content_only() {
    let (d, mut r) = trk_project();
    let repo = linked_repo(d.path());
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."}
        ]),
    );
    let (a, b, sec) = (
        w.created["$a"].clone(),
        w.created["$b"].clone(),
        w.created["$s"].clone(),
    );
    let made = r
        .decide(
            &writer(),
            "seed",
            rdy_engine::DecisionInput {
                parent: sec,
                statement: "Build on A".into(),
                depends_on: vec![a.clone()],
                fields: [("disposition".to_string(), json!("now"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
    let s = made.created["$decision"].clone();
    land(&mut r, "seed");
    commit(repo.path(), "f.rs", &format!("start\n\nImplements: {a}"));
    edit(&mut r, "w", &a, "A holds firmly.");
    let confirm = pi(&a, &b, PlanKind::Confirm);
    let reaffirm = pi(&a, &s, PlanKind::Reaffirm);
    let code = pi(&a, "app:f.rs", PlanKind::UpdateImplementation);
    let v = whole(&r, "w");
    assert_eq!(v["items"].as_array().unwrap().len(), 3, "{v:#}");
    answer(
        &mut r,
        "w",
        &[
            ans(&confirm, PlanAct::Confirm, Some("  b still holds  ")),
            ans(&reaffirm, PlanAct::Defer, Some("after the review")),
            ans(&code, PlanAct::Done, Some("ignored")),
        ],
    );
    let by = json!({"actor": "human:qa", "run": "cli", "role": "writer"});
    let shown = |v: &Value, item: &str| {
        let r = row(v, item);
        let mut a = r.get("answer").cloned();
        if let Some(a) = &mut a {
            assert!(a["by"]["at_ms"].is_i64());
            a["by"].as_object_mut().unwrap().remove("at_ms");
        }
        (r["state"].clone(), a)
    };
    let v = whole(&r, "w");
    assert_eq!(v["notes"], json!([]));
    assert_eq!(
        shown(&v, &confirm),
        (
            json!("closed"),
            Some(json!({"act": "confirm", "reason": "b still holds", "by": by}))
        )
    );
    assert_eq!(
        shown(&v, &reaffirm),
        (
            json!("deferred"),
            Some(json!({"act": "defer", "reason": "after the review", "by": by}))
        )
    );
    assert_eq!(
        shown(&v, &code),
        (json!("closed"), Some(json!({"act": "done", "by": by})))
    );
    let stored = r.read().reader().plan(&plan_ref(&r, "w").unwrap()).unwrap();
    let done: &PlanAnswer = stored
        .answers
        .iter()
        .find(|x| item_display(&x.item) == code)
        .unwrap();
    assert_eq!((done.reason.as_ref(), done.subject), (None, None));

    // a tracking-field edit of the decision changes no content: its deferral holds
    r.write(
        &writer(),
        WriteRequest {
            view: "w".into(),
            ops: parse_ops(vec![
                json!({"op": "update", "id": s, "fields": {"disposition": "later"}}),
            ])
            .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "re-sliced".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    let v = whole(&r, "w");
    assert_eq!(v["notes"], json!([]));
    assert_eq!(shown(&v, &reaffirm).0, "deferred");

    // a rewrite of the subject lapses its answer, naming the subject
    edit(&mut r, "w", &b, &format!("B builds firmly on {{{{{a}}}}}."));
    let v = whole(&r, "w");
    assert_eq!(shown(&v, &confirm), (json!("open"), None));
    assert_eq!(
        v["notes"],
        json!([{"code": W_PLAN_ANSWER_LAPSED,
                "message": format!("the answer to {confirm} lapsed: {b} changed"),
                "fix": "answer the item again"}])
    );
    // answered again; a repin moves only pins, so the answer holds
    answer(
        &mut r,
        "w",
        &[ans(&confirm, PlanAct::Confirm, Some("b still holds"))],
    );
    put(
        &mut r,
        "w",
        json!([{"op": "repin", "id": b, "targets": [a]}]),
    );
    let v = whole(&r, "w");
    assert_eq!(v["notes"], json!([]));
    assert_eq!(shown(&v, &confirm).0, "closed");
    assert!(row(&v, &confirm).get("how").is_none());

    // the subject and the cause both change: the subject is named; the decision's and the
    // code item's causes changed alone
    put(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": a, "body": "A holds for good."},
            {"op": "update", "id": b, "body": format!("B builds for good on {{{{{a}}}}}.")}
        ]),
    );
    let v = whole(&r, "w");
    let mut want: Vec<(String, String)> = vec![
        (confirm.clone(), b.clone()),
        (reaffirm.clone(), a.clone()),
        (code.clone(), a.clone()),
    ];
    want.sort();
    let notes: Vec<Value> = want
        .iter()
        .map(|(item, block)| {
            json!({"code": W_PLAN_ANSWER_LAPSED,
                   "message": format!("the answer to {item} lapsed: {block} changed"),
                   "fix": "answer the item again"})
        })
        .collect();
    assert_eq!(v["notes"], Value::Array(notes));
    for item in [&confirm, &reaffirm, &code] {
        assert_eq!(shown(&v, item), (json!("open"), None), "{item}");
    }
    assert_eq!(v["counts"]["open"], 3);
}

/// tb-2ZED7: a computed close wins before any answer is read: a deferred item the version
/// then closes is closed, shows how, and its stale answer is no warning.
#[test]
fn cp8_computed_close_wins_over_defer() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let update = pi(&s.a, &s.p, PlanKind::Update);
    answer(
        &mut r,
        "w",
        &[
            ans(&confirm, PlanAct::Defer, Some("later")),
            ans(&update, PlanAct::Defer, Some("later")),
        ],
    );
    let v = whole(&r, "w");
    assert_eq!(v["counts"]["deferred"], 2);
    // a repin closes the confirm; a rewrite closes the update (and would lapse its answer)
    put(
        &mut r,
        "w",
        json!([
            {"op": "repin", "id": s.b, "targets": [s.a]},
            {"op": "update", "id": s.p, "body": format!("Wait 60 days per {{{{{}}}}}.", s.a)}
        ]),
    );
    let v = whole(&r, "w");
    assert_eq!(v["notes"], json!([]), "no lapse warning for a closed item");
    let c = row(&v, &confirm);
    assert_eq!(
        (&c["state"], &c["how"], c.get("answer")),
        (&json!("closed"), &json!("repinned"), None)
    );
    let u = row(&v, &update);
    assert_eq!(
        (&u["state"], &u["how"], u.get("answer")),
        (&json!("closed"), &json!("updated"), None)
    );
    assert_eq!(
        v["counts"],
        counts(0, 2, 0, &[("confirm", [0, 1, 0]), ("update", [0, 1, 0])])
    );
}

/// tb-2ZED7: update, rejustify and reaffirm take defer; confirm takes confirm and defer; code
/// takes done and defer. A `pi-` prefix and a `cq-` id resolve to their full item, so two
/// aliases of one item in a call are a duplicate; a stale alias is not an item.
#[test]
fn cp8_plan_act_matrix_and_duplicate_aliases_are_exact() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    let d1 = r
        .decide(
            &writer(),
            "seed2",
            rdy_engine::DecisionInput {
                parent: s.sec.clone(),
                statement: "Keep 30 days".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let dep = r
        .decide(
            &writer(),
            "seed2",
            rdy_engine::DecisionInput {
                parent: s.sec.clone(),
                statement: "Build on A".into(),
                depends_on: vec![s.a.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let rq = put(
        &mut r,
        "seed2",
        json!([{"op": "create", "type": "claim", "id": "$r", "parent": s.sec, "body": "Teams follow it.",
                "links": {"decided_by": [{"target": d1}]}}]),
    )
    .created["$r"]
    .clone();
    land(&mut r, "seed2");
    commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.a),
    );
    let first = edit(&mut r, "w", &s.a, "A holds 60 days.");
    let sup = r
        .supersede(
            &writer(),
            "w",
            &d1,
            None,
            rdy_engine::DecisionInput {
                parent: s.sec.clone(),
                statement: "Keep 60 days".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let v = whole(&r, "w");
    let item = |kind: PlanKind| -> String {
        let (cause, subject) = match kind {
            PlanKind::Update => (&s.a, s.p.clone()),
            PlanKind::Rejustify => (&d1, rq.clone()),
            PlanKind::Reaffirm => (&s.a, dep.clone()),
            PlanKind::Confirm => (&s.a, s.b.clone()),
            PlanKind::UpdateImplementation => (&s.a, "app:f.rs".to_string()),
        };
        let i = pi(cause, &subject, kind);
        assert_eq!(row(&v, &i)["kind"], kind.as_str());
        i
    };
    let allowed = [
        (PlanKind::Update, [false, true, false]),
        (PlanKind::Rejustify, [false, true, false]),
        (PlanKind::Reaffirm, [false, true, false]),
        (PlanKind::Confirm, [true, true, false]),
        (PlanKind::UpdateImplementation, [false, true, true]),
    ];
    let acts = [PlanAct::Confirm, PlanAct::Defer, PlanAct::Done];
    let mut pairs = 0;
    for (kind, ok) in allowed {
        let i = item(kind);
        for (act, ok) in acts.iter().zip(ok) {
            pairs += 1;
            let got = r.answer_plan(&writer(), "w", &[ans(&i, *act, Some("because"))]);
            if ok {
                got.unwrap_or_else(|e| panic!("{} on {}: {e}", act.as_str(), kind.as_str()));
            } else {
                expect_err(
                    got.unwrap_err(),
                    E_BAD_INPUT,
                    &format!("{} does not answer a {} item", act.as_str(), kind.as_str()),
                    "read the plan and use an act the item accepts",
                );
            }
        }
    }
    assert_eq!(pairs, 15);

    // a full display id and a shorter prefix of it are one item
    let confirm = item(PlanKind::Confirm);
    let dup = |e: RdyError, item: &str| {
        expect_err(
            e,
            E_BAD_INPUT,
            &format!("duplicate plan answer for {item}"),
            "answer each item once per call",
        )
    };
    dup(
        r.answer_plan(
            &writer(),
            "w",
            &[
                ans(&confirm, PlanAct::Defer, Some("x")),
                ans(&confirm[..10], PlanAct::Confirm, Some("y")),
            ],
        )
        .unwrap_err(),
        &confirm,
    );
    // the last write's consequence id maps to its item; with its pi id it is a duplicate
    let st = r.read();
    let sup_change: Hash = sup.change.parse().unwrap();
    let report = st
        .consequences(&Scope::View("w".into()), &sup_change, None)
        .unwrap();
    let rejustify = item(PlanKind::Rejustify);
    let cq = report
        .items
        .iter()
        .find(|c| c.block == rq)
        .unwrap_or_else(|| panic!("a consequence on {rq}: {report:#?}"))
        .id
        .clone();
    let first_change: Hash = first.change.parse().unwrap();
    let stale = st
        .consequences(&Scope::View("w".into()), &first_change, None)
        .unwrap()
        .items
        .iter()
        .find(|c| c.block == s.b)
        .unwrap()
        .id
        .clone();
    drop(st);
    dup(
        r.answer_plan(
            &writer(),
            "w",
            &[
                ans(&cq, PlanAct::Defer, Some("x")),
                ans(&rejustify, PlanAct::Defer, Some("y")),
            ],
        )
        .unwrap_err(),
        &rejustify,
    );
    let h = answer(&mut r, "w", &[ans(&cq, PlanAct::Defer, Some("mapped"))]);
    let p = r.read().reader().plan(&h).unwrap();
    let mapped = p
        .answers
        .iter()
        .find(|a| item_display(&a.item) == rejustify)
        .unwrap();
    assert_eq!(mapped.reason.as_deref(), Some("mapped"));
    // an earlier write's consequence id is stale
    expect_err(
        r.answer_plan(&writer(), "w", &[ans(&stale, PlanAct::Confirm, Some("x"))])
            .unwrap_err(),
        E_PLAN_ITEM,
        &format!("{stale} is not an item of the change plan for view:w"),
        "read the plan with rdy_plan / rdy plan and pass an item id it lists",
    );
}

/// p-PZX3T: a reader is E_PERMISSION and an agent answers only its own view (E_VIEW_OWNER),
/// before any validation and with the log and every ref left as they were.
#[test]
fn cp8_reader_and_view_owner_guards_precede_write() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let owner = agent("r1");
    put_as(
        &mut r,
        &owner,
        "r1",
        json!([{"op": "update", "id": s.a, "body": "A holds 60 days."}]),
    );
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let good = [ans(&confirm, PlanAct::Confirm, Some("holds"))];
    let junk = [ans("pi-zz", PlanAct::Done, None)];
    let before = log_state(&r);
    for answers in [&good[..], &junk[..]] {
        expect_err(
            r.answer_plan(&reader(), "r1", answers).unwrap_err(),
            E_PERMISSION,
            "role Reader cannot write",
            "start the session as a writer",
        );
        assert_eq!(log_state(&r), before);
        let e = r.answer_plan(&agent("r2"), "r1", answers).unwrap_err();
        assert_eq!(e.code, E_VIEW_OWNER, "{e}");
        assert_eq!(e.message, "view \"r1\" is owned by agent:qa (run r1)");
        assert_eq!(log_state(&r), before);
        let e = r.answer_plan(&owner, "work", answers).unwrap_err();
        assert_eq!(e.code, E_VIEW_OWNER, "{e}");
        assert_eq!(log_state(&r), before);
    }
    let many = vec![ans(&confirm, PlanAct::Confirm, Some("holds")); 100_001];
    assert_eq!(
        r.answer_plan(&reader(), "r1", &many).unwrap_err().code,
        E_PERMISSION
    );
    assert_eq!(log_state(&r), before);
    // the owner answers its view; the person may answer any view
    let h = r.answer_plan(&owner, "r1", &good).unwrap();
    assert_eq!(plan_ref(&r, "r1"), Some(h));
    let h2 = r
        .answer_plan(
            &writer(),
            "r1",
            &[ans(&confirm, PlanAct::Defer, Some("later"))],
        )
        .unwrap();
    let p = r.read().reader().plan(&h2).unwrap();
    assert_eq!((p.stable, p.prev), (Some(h), Some(h)));
}

/// p-PZX3T: the pages of a cut plan hold every item once, in order, each within its budget
/// (at least one item a page); every page carries the whole counts and notes; a changed answer
/// makes the next page stale; a page goes alone.
#[test]
fn cp8_pages_are_complete_and_stale_on_changed_answers() {
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}),
        json!({"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."}),
    ];
    for i in 0..12 {
        ops.push(
            json!({"op": "create", "type": "claim", "id": format!("$n{i}"), "parent": "$s",
                        "body": format!("N{i} builds on {{{{$a}}}}.")}),
        );
    }
    let w = put(&mut r, "seed", Value::Array(ops));
    land(&mut r, "seed");
    edit(&mut r, "w", &w.created["$a"], "A holds firmly.");
    let all = whole(&r, "w");
    let total = all["items"].as_array().unwrap().len();
    assert_eq!(total, 12);
    for max in [0usize, 1, 150, 400, 1000] {
        let mut seen: Vec<Value> = vec![];
        let mut q = PlanQuery {
            max_tokens: Some(max),
            ..view_q("w")
        };
        let mut pages = 0;
        loop {
            let p = plan(&r, &q);
            pages += 1;
            let n = p["items"].as_array().unwrap().len();
            assert!(n >= 1, "a page moves on");
            let bytes = serde_json::to_string(&p).unwrap().len();
            assert!(
                bytes / 4 <= max || n == 1,
                "max {max}: {bytes} bytes for {n} items"
            );
            for k in ["scope", "plan", "counts", "code", "notes"] {
                assert_eq!(p[k], all[k], "{k}");
            }
            seen.extend(p["items"].as_array().unwrap().iter().cloned());
            match p.get("next") {
                None => break,
                Some(next) => {
                    let next = next.as_str().unwrap();
                    let (offset, rest) = next
                        .strip_prefix("plan1:")
                        .unwrap()
                        .split_once(':')
                        .unwrap();
                    assert_eq!(offset.parse::<usize>().unwrap(), seen.len());
                    let (digest, sel) = rest.split_once(':').unwrap();
                    assert_eq!(digest.len(), 16);
                    assert_eq!(sel, "view:w");
                    q = PlanQuery {
                        page: Some(next.to_string()),
                        max_tokens: Some(max),
                        ..Default::default()
                    };
                }
            }
        }
        assert_eq!(Value::Array(seen), all["items"], "max {max}");
        if max == 0 {
            assert_eq!(pages, total, "one item a page at budget zero");
        }
    }
    // a cursor survives another budget, but not a changed answer
    let first = plan(
        &r,
        &PlanQuery {
            max_tokens: Some(0),
            ..view_q("w")
        },
    );
    let next = first["next"].as_str().unwrap().to_string();
    let page = |max| PlanQuery {
        page: Some(next.clone()),
        max_tokens: Some(max),
        ..Default::default()
    };
    assert_eq!(
        plan(&r, &page(100_000))["items"],
        json!(all["items"].as_array().unwrap()[1..])
    );
    let item = all["items"][0]["id"].as_str().unwrap();
    answer(&mut r, "w", &[ans(item, PlanAct::Defer, Some("later"))]);
    expect_err(
        plan_of(&r, &page(0)).unwrap_err(),
        E_PAGE_STALE,
        "the change plan moved since its first page",
        "read the plan again without page",
    );
    // a later answer's provenance is in the digest too
    let first = plan(
        &r,
        &PlanQuery {
            max_tokens: Some(0),
            ..view_q("w")
        },
    );
    let next = first["next"].as_str().unwrap().to_string();
    answer(&mut r, "w", &[ans(item, PlanAct::Defer, Some("later"))]);
    assert_eq!(
        plan_of(
            &r,
            &PlanQuery {
                page: Some(next.clone()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        E_PAGE_STALE
    );
    // an offset at the end reads an empty last page; past it is stale
    let now = plan(
        &r,
        &PlanQuery {
            max_tokens: Some(0),
            ..view_q("w")
        },
    );
    let cursor = now["next"].as_str().unwrap();
    let digest = cursor.split(':').nth(2).unwrap();
    let at = |offset: usize| PlanQuery {
        page: Some(format!("plan1:{offset}:{digest}:view:w")),
        ..Default::default()
    };
    let end = plan(&r, &at(total));
    assert_eq!(end["items"], json!([]));
    assert!(end.get("next").is_none());
    assert_eq!(plan_of(&r, &at(total + 1)).unwrap_err().code, E_PAGE_STALE);
    // a page goes alone
    expect_err(
        plan_of(
            &r,
            &PlanQuery {
                page: Some(cursor.to_string()),
                view: Some("w".into()),
                ..Default::default()
            },
        )
        .unwrap_err(),
        E_BAD_INPUT,
        "page continues a plan: pass it alone",
        "pass page and max_tokens only",
    );
}

/// A proposal version reads from its recorded canon, its changes and its bound plan's HEADs and
/// answers: canon, the view and the checkout moving on change nothing. A version with no bound
/// plan (O19) reads the HEADs captured once as both ends, with no answer.
#[test]
fn cp8_proposal_read_uses_recorded_canon_and_heads() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    let h1 = commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.a),
    );
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let code = pi(&s.a, "app:f.rs", PlanKind::UpdateImplementation);
    // the version and the plan its propose binds, with one deferral, in one batch, as the
    // propose composition writes them (no engine path binds a plan yet)
    let st = r.read();
    let w = Scope::View("w".into());
    let deferral = PlanAnswer {
        item: item_hash(&id(&s.a), &s.b, PlanKind::Confirm),
        act: PlanAct::Defer,
        reason: Some("after the review".into()),
        subject: Some(st.block(&w, &id(&s.b)).unwrap().content_hash()),
        cause: st.block(&w, &id(&s.a)).unwrap().content_hash(),
        by: prov(),
    };
    drop(st);
    let version = r.store_mut().propose("w", "edit a", prov()).unwrap();
    let pref = format!("P-{}@1", version.short());
    let bound = Plan {
        stable: None,
        prev: None,
        view: "w".into(),
        proposal: Some(version),
        answers: vec![deferral],
        heads: vec![PlanHead {
            repo: "app".into(),
            head: Some(h1.clone()),
        }],
        provenance: prov(),
        seal: None,
    };
    let bh = put_plan_frames(r.store_mut(), &bound);
    r.invalidate();
    r.fsck().unwrap();
    let pq = PlanQuery {
        proposal: Some(pref.clone()),
        ..Default::default()
    };
    let before = plan(&r, &pq);
    assert_eq!(before["plan"], bh.to_string());
    assert_eq!(row(&before, &confirm)["state"], "deferred");
    assert_eq!(row(&before, &code)["state"], "open");
    assert_eq!(before["code"], json!({"files": 1}));

    // canon, the view and the checkout move on
    commit(
        repo.path(),
        "f.rs",
        &format!("follow\n\nImplements: {}", s.a),
    );
    edit(&mut r, "v2", &s.c, "C stands firm.");
    land(&mut r, "v2");
    edit(
        &mut r,
        "w",
        &s.p,
        &format!("Wait 60 days per {{{{{}}}}}.", s.a),
    );
    assert_eq!(plan(&r, &pq), before, "the version reads as recorded");
    // the view's own plan reads the checkout as it is now: the later commit closes the code item
    let now = whole(&r, "w");
    let c = row(&now, &code);
    assert_eq!(
        (&c["state"], &c["how"]),
        (&json!("closed"), &json!("committed"))
    );

    // a version with no bound plan (the raw store writer: every engine propose binds one): no
    // plan, no answer, the captured HEADs as both ends
    let included = r.store().included("w").unwrap().as_ref().clone();
    r.store_mut()
        .propose_next(&version, included, "edit a and p", None, prov(), None)
        .unwrap();
    r.invalidate();
    let unbound = plan(
        &r,
        &PlanQuery {
            proposal: Some(format!("P-{}@2", version.short())),
            ..Default::default()
        },
    );
    assert_eq!(unbound["plan"], Value::Null);
    assert_eq!(unbound["notes"], json!([]));
    assert_eq!(row(&unbound, &confirm)["state"], "open");
    assert!(row(&unbound, &confirm).get("answer").is_none());
    assert_eq!(row(&unbound, &code)["state"], "open");
    assert_eq!(plan_ref(&r, "w"), Some(bh), "a read writes no plan");
    r.fsck().unwrap();
}

/// O6: a chain whose head is bound to a version that landed with no seal is over; the view's
/// next answer starts a new chain with the HEADs captured then as its baseline.
#[test]
fn cp8_empty_landing_ends_an_unsealed_chain() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    let h1 = commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.c),
    );
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let first = answer(&mut r, "w", &[ans(&confirm, PlanAct::Defer, Some("later"))]);
    // the version and its bound plan, which continues the chain, in one batch, as the propose
    // composition writes them (no engine path binds a plan yet)
    let version = r.store_mut().propose("w", "edit a", prov()).unwrap();
    let answered = r.read().reader().plan(&first).unwrap();
    let bound = Plan {
        stable: Some(first),
        prev: Some(first),
        proposal: Some(version),
        ..answered
    };
    let bh = put_plan_frames(r.store_mut(), &bound);
    r.invalidate();
    r.fsck().unwrap();
    assert_eq!(whole(&r, "w")["plan"], bh.to_string());
    // a landing with no seal (the raw store landing: the engine seals a version with items)
    r.store_mut()
        .land(&version, approver().provenance())
        .unwrap();
    r.invalidate();
    assert_eq!(plan_ref(&r, "w"), Some(bh), "no seal was written");
    // the chain ended with the landing: the view reads no plan and no answer
    edit(&mut r, "w", &s.c, "C stands firm.");
    let v = whole(&r, "w");
    assert_eq!(v["plan"], Value::Null);
    let cb = pi(&s.c, &s.b, PlanKind::Confirm);
    let code = pi(&s.c, "app:f.rs", PlanKind::UpdateImplementation);
    assert_eq!(row(&v, &cb)["state"], "open");
    let h2 = commit(repo.path(), "g.rs", "unrelated");
    let next = answer(&mut r, "w", &[ans(&cb, PlanAct::Confirm, Some("holds"))]);
    let p = r.read().reader().plan(&next).unwrap();
    assert_eq!((p.stable, p.prev), (None, None), "a new chain");
    assert_eq!(
        p.heads,
        vec![PlanHead {
            repo: "app".into(),
            head: Some(h2.clone()),
        }],
        "the new baseline is the HEAD captured now"
    );
    assert_ne!(Some(h1), p.heads[0].head);
    assert_eq!(
        p.answers.len(),
        1,
        "the old chain's answers stay in its own versions"
    );
    // a commit after the new baseline closes the code item; one before it never could
    let v = whole(&r, "w");
    assert_eq!(row(&v, &code)["state"], "open");
    commit(
        repo.path(),
        "f.rs",
        &format!("follow\n\nImplements: {}", s.c),
    );
    let v = whole(&r, "w");
    assert_eq!(row(&v, &code)["how"], "committed");
    // fsck reads the new chain as the plan writer does (O6)
    r.fsck().unwrap();
}

/// D-4JMPH: a plan answer moves no tension and no suspect: its batch is exactly the PLAN frame
/// and its ref and the marker, with no TRANSITION, where a landing that moves a suspect logs
/// one. (`transitions::can_move` leaves PLAN and `plan/` out: its own unit test.)
#[test]
fn cp8_plan_only_write_logs_no_transition() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let walk = |r: &Rdy, from| {
        let mut kinds = vec![];
        r.store()
            .reader()
            .for_each_frame_from(from, r.store().log_end(), |_, kind, _, _, _| {
                kinds.push(kind);
                Ok(())
            })
            .unwrap();
        kinds
    };
    // the control: a landing that makes b and p suspect in canon logs a TRANSITION
    edit(&mut r, "v", &s.c, "C stands firm.");
    let p = r
        .propose(&writer(), "v", "v", Some(common::TEST_SUMMARY))
        .unwrap();
    let start = r.store().log_end();
    r.land(&approver(), &p.id).unwrap();
    let landed = walk(&r, start);
    assert!(landed.contains(&kinds::TRANSITION), "{landed:?}");
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let end = r.store().log_end();
    answer(
        &mut r,
        "w",
        &[ans(
            &pi(&s.a, &s.b, PlanKind::Confirm),
            PlanAct::Defer,
            Some("later"),
        )],
    );
    let frames: Vec<u8> = r.store().last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(frames, [kinds::PLAN, kinds::REF_UPDATE]);
    assert_eq!(
        walk(&r, end),
        [kinds::PLAN, kinds::REF_UPDATE, kinds::BATCH_COMMIT],
        "one batch, its two frames and the marker"
    );
}

/// The plan cursor reuses the briefing's tail parser and its 16-digit digest: `plan1:`, one
/// offset, the digest, then the selector last, colons and all. A malformed cursor is refused;
/// the briefing's cursors keep their bytes and errors.
#[test]
fn cp8_cursor_reuses_briefing_tail_and_digest_width() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let info = r
        .propose(&writer(), "w", "edit a", Some(common::TEST_SUMMARY))
        .unwrap();
    // a selector with colons pages through
    let pq = PlanQuery {
        proposal: Some(info.r#ref.clone()),
        max_tokens: Some(0),
        ..Default::default()
    };
    let p = plan(&r, &pq);
    let next = p["next"].as_str().unwrap().to_string();
    let tail = next.strip_prefix("plan1:1:").unwrap();
    let (digest, sel) = tail.split_once(':').unwrap();
    assert!(
        digest.len() == 16
            && digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(sel, format!("proposal:{}", info.r#ref));
    let rest = plan(
        &r,
        &PlanQuery {
            page: Some(next.clone()),
            max_tokens: Some(0),
            ..Default::default()
        },
    );
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert!(rest.get("next").is_none());
    let bad = |cursor: String| {
        expect_err(
            plan_of(
                &r,
                &PlanQuery {
                    page: Some(cursor),
                    ..Default::default()
                },
            )
            .unwrap_err(),
            E_BAD_INPUT,
            "page is not a cursor from rdy_plan's next",
            "pass the next of a cut rdy_plan, alone",
        )
    };
    bad(format!("plan1::{digest}:{sel}"));
    bad(format!("plan1:1,1:{digest}:{sel}"));
    bad(format!("plan1:x:{digest}:{sel}"));
    bad(format!("plan1:1:{}:{sel}", &digest[..15]));
    bad(format!("plan1:1:{}:{sel}", digest.to_uppercase()));
    bad(format!("plan1:1:{digest}:canon"));
    bad(format!(
        "plan1:1:{digest}:proposal:P-{}",
        &info.stable[..12]
    ));
    bad(format!("plan9:1:{digest}:{sel}"));
    bad(format!("plan1:1:{digest}:view:{}", "w".repeat(4096)));
    // a briefing's cursor is still `<token>:<eleven offsets>:<16 hex>:<scope>`
    let st = r.read();
    let w = Scope::View("w".into());
    let b = st
        .brief(
            &w,
            &Coverage {
                ids: vec![s.a.clone()],
                path: None,
            },
            Some(0),
        )
        .unwrap();
    b.register(&r.briefs());
    let cursor = b.next.clone().expect("a cut briefing");
    let parts: Vec<&str> = cursor.splitn(4, ':').collect();
    assert_eq!(parts[0].len(), 64);
    assert_eq!(parts[1].split(',').count(), 11);
    assert_eq!(parts[2].len(), 16);
    assert_eq!(parts[3], "view:w");
    st.brief_page(&w, &r.briefs(), &cursor, Some(100_000))
        .unwrap();
    let ten = format!(
        "{}:{}:{}:{}",
        parts[0],
        parts[1].rsplit_once(',').unwrap().0,
        parts[2],
        parts[3]
    );
    expect_err(
        st.brief_page(&w, &r.briefs(), &ten, None).unwrap_err(),
        E_BAD_INPUT,
        "page is not a cursor from rdy_brief's next",
        "pass the next of a cut rdy_brief, alone",
    );
    // a plan cursor is no briefing cursor
    assert_eq!(
        st.brief_page(&w, &r.briefs(), &next, None)
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
}

/// Every new refusal's exact text, and the input bounds.
#[test]
fn cp8_input_limits_have_exact_text() {
    let both = PlanQuery {
        view: Some("w".into()),
        decision: Some("D-AAAAA".into()),
        ..Default::default()
    };
    let one = [ans("pi-0", PlanAct::Defer, Some("x"))];
    expect_err(
        check_plan_request(&both, &[]).unwrap_err(),
        E_BAD_INPUT,
        "plan takes at most one of view, proposal, landing and decision",
        "pass one selector, or omit them for the session view",
    );
    for q in [
        PlanQuery {
            proposal: Some("P-abcd@1".into()),
            ..Default::default()
        },
        PlanQuery {
            landing: Some("abcdef".into()),
            ..Default::default()
        },
        PlanQuery {
            decision: Some("D-AAAAA".into()),
            ..Default::default()
        },
    ] {
        expect_err(
            check_plan_request(&q, &one).unwrap_err(),
            E_BAD_INPUT,
            "plan answers go with view only",
            "answer the plan in its view",
        );
    }
    check_plan_request(&view_q("w"), &one).unwrap();
    check_plan_request(&PlanQuery::default(), &one).unwrap();
    for q in [
        PlanQuery {
            page: Some("plan1:1:0123456789abcdef:view:w".into()),
            view: Some("w".into()),
            ..Default::default()
        },
        PlanQuery {
            page: Some("plan1:1:0123456789abcdef:view:w".into()),
            ..Default::default()
        },
    ] {
        let answers: &[PlanAnswerInput] = if q.view.is_some() { &[] } else { &one };
        expect_err(
            check_plan_request(&q, answers).unwrap_err(),
            E_BAD_INPUT,
            "page continues a plan: pass it alone",
            "pass page and max_tokens only",
        );
    }
    let many = vec![ans("pi-0", PlanAct::Defer, Some("x")); 100_001];
    let limit = |e: RdyError| {
        expect_err(
            e,
            E_LIMIT,
            "change plan exceeds 100000 plan answers",
            "split the answers into smaller plans",
        )
    };
    limit(check_plan_request(&view_q("w"), &many).unwrap_err());

    let (_d, mut r) = project();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    limit(r.answer_plan(&writer(), "w", &many).unwrap_err());
    expect_err(
        plan_of(&r, &both).unwrap_err(),
        E_BAD_INPUT,
        "plan takes at most one of view, proposal, landing and decision",
        "pass one selector, or omit them for the session view",
    );
    let info = r
        .propose(&writer(), "w", "edit a", Some(common::TEST_SUMMARY))
        .unwrap();
    // the propose binds its plan (D-4JMPH): the refused answers below write nothing after it
    let bound = plan_ref(&r, "w");
    assert!(bound.is_some());
    for text in [
        format!("P-{}", &info.stable[..12]),
        info.id.clone(),
        "nope".to_string(),
    ] {
        expect_err(
            plan_of(
                &r,
                &PlanQuery {
                    proposal: Some(text),
                    ..Default::default()
                },
            )
            .unwrap_err(),
            E_BAD_INPUT,
            "plan proposal needs an explicit version",
            "pass P-<id>@<n>",
        );
    }
    expect_err(
        plan_of(
            &r,
            &PlanQuery {
                max_tokens: Some(100_001),
                ..view_q("w")
            },
        )
        .unwrap_err(),
        E_LIMIT,
        "max_tokens 100001 is over the limit of 100000",
        "ask for at most 100000 tokens and page through the rest",
    );
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let update = pi(&s.a, &s.p, PlanKind::Update);
    let not_item = |e: RdyError, item: &str| {
        expect_err(
            e,
            E_PLAN_ITEM,
            &format!("{item} is not an item of the change plan for view:w"),
            "read the plan with rdy_plan / rdy plan and pass an item id it lists",
        )
    };
    let gone = pi(&s.c, &s.b, PlanKind::Confirm);
    for item in [
        gone.as_str(),
        "pi-",
        "pi-XYZ",
        "cq-0123456789abcdef",
        "c-AAAAA",
        "",
    ] {
        not_item(
            r.answer_plan(&writer(), "w", &[ans(item, PlanAct::Defer, Some("x"))])
                .unwrap_err(),
            item,
        );
    }
    expect_err(
        r.answer_plan(
            &writer(),
            "w",
            &[
                ans(&confirm, PlanAct::Defer, Some("x")),
                ans(&confirm, PlanAct::Defer, Some("x")),
            ],
        )
        .unwrap_err(),
        E_BAD_INPUT,
        &format!("duplicate plan answer for {confirm}"),
        "answer each item once per call",
    );
    for (act, reason) in [
        (PlanAct::Confirm, None),
        (PlanAct::Confirm, Some("   ".to_string())),
        (PlanAct::Defer, Some("x".repeat(2001))),
    ] {
        expect_err(
            r.answer_plan(&writer(), "w", &[ans(&confirm, act, reason.as_deref())])
                .unwrap_err(),
            E_BAD_INPUT,
            &format!("{} needs a reason of 1 to 2000 characters", act.as_str()),
            "give a reason of 1 to 2000 characters",
        );
    }
    expect_err(
        r.answer_plan(&writer(), "w", &[ans(&update, PlanAct::Confirm, Some("x"))])
            .unwrap_err(),
        E_BAD_INPUT,
        "confirm does not answer a update item",
        "read the plan and use an act the item accepts",
    );
    assert_eq!(plan_ref(&r, "w"), bound, "nothing refused was written");
    // the bounds hold exactly: 2000 characters (not bytes) are a reason
    let h = answer(
        &mut r,
        "w",
        &[ans(&confirm, PlanAct::Defer, Some(&"é".repeat(2000)))],
    );
    assert_eq!(
        r.read().reader().plan(&h).unwrap().answers[0]
            .reason
            .as_deref()
            .map(|x| x.chars().count()),
        Some(2000)
    );
}

/// Fix round 1: a retained state reads the plan as it was published. A later answer and a later
/// landing move neither its plan ref nor its chain's landing status: the plan ref is captured
/// with the state, and a landing counts only when its frame lies before the state's sequence.
#[test]
fn cp8_retained_reads_keep_their_plan_across_answers_and_landings() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let q = PlanQuery {
        max_tokens: Some(100_000),
        ..view_q("w")
    };
    let read_at = |st: &rdy_engine::ReadState| {
        serde_json::to_value(st.change_plan(&q, "work").unwrap()).unwrap()
    };
    // an answer after the state was published
    let captured = r.read();
    let before = read_at(&captured);
    assert_eq!(before["plan"], Value::Null);
    assert_eq!(row(&before, &confirm)["state"], "open");
    let first = answer(
        &mut r,
        "w",
        &[ans(&confirm, PlanAct::Confirm, Some("holds"))],
    );
    assert_eq!(
        read_at(&captured),
        before,
        "the retained state reads no later answer"
    );
    let now = whole(&r, "w");
    assert_eq!(now["plan"], first.to_string());
    assert_eq!(row(&now, &confirm)["state"], "closed");

    // a landing after the state was published: the bound proposal had not landed at its
    // sequence, so its chain stays live there
    let changes = r.store_mut().included("w").unwrap().as_ref().clone();
    let version = r
        .store_mut()
        .propose_changes("w", "edit a", Some("test summary"), prov(), changes, None)
        .unwrap();
    let answered = r.read().reader().plan(&first).unwrap();
    let bound = Plan {
        stable: Some(first),
        prev: Some(first),
        proposal: Some(version),
        ..answered
    };
    let bh = put_plan_frames(r.store_mut(), &bound);
    r.invalidate();
    let captured = r.read();
    let before = read_at(&captured);
    assert_eq!(before["plan"], bh.to_string());
    assert_eq!(row(&before, &confirm)["state"], "closed");
    r.land(&approver(), &version.to_string()).unwrap();
    assert_eq!(
        read_at(&captured),
        before,
        "the retained state reads no later landing"
    );
    assert_eq!(
        whole(&r, "w")["plan"],
        Value::Null,
        "the landing ended the chain"
    );
    r.fsck().unwrap();
}

/// O6 with answer-only versions after the bound one: the chain ends when its last bound
/// proposal lands without a seal, even when later versions bind none. The next answer starts a
/// new chain (`stable = prev = None`), drops the old chain's answers and takes the HEADs
/// captured then as its baseline, so a commit made before the landing closes nothing. The store
/// refuses a version that continues the ended chain, and fsck reads the new chain as the writer
/// does.
#[test]
fn cp8_answer_only_successors_keep_the_last_bound_proposal() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let s = spec(&mut r);
    commit(
        repo.path(),
        "f.rs",
        &format!("start\n\nImplements: {}", s.a),
    );
    // a version with no item: b's edit is the cause of nothing
    edit(
        &mut r,
        "w",
        &s.b,
        &format!("B builds firmly on {{{{{}}}}} and {{{{{}}}}}.", s.a, s.c),
    );
    let changes = r.store_mut().included("w").unwrap().as_ref().clone();
    let version = r
        .store_mut()
        .propose_changes("w", "edit b", Some("test summary"), prov(), changes, None)
        .unwrap();
    let pref = format!("P-{}@1", version.short());
    let bound = Plan {
        stable: None,
        prev: None,
        view: "w".into(),
        proposal: Some(version),
        answers: vec![],
        heads: vec![PlanHead {
            repo: "app".into(),
            head: Some(git(repo.path(), &["rev-parse", "HEAD"])),
        }],
        provenance: prov(),
        seal: None,
    };
    let bh = put_plan_frames(r.store_mut(), &bound);
    r.invalidate();
    r.fsck().unwrap();
    let pv = plan(
        &r,
        &PlanQuery {
            proposal: Some(pref),
            ..Default::default()
        },
    );
    assert_eq!(
        pv["items"],
        json!([]),
        "the bound version has no item: {pv}"
    );

    // another change in the view, and an answer: an answer-only successor of the bound version
    edit(&mut r, "w", &s.a, "A holds 60 days.");
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    let code = pi(&s.a, "app:f.rs", PlanKind::UpdateImplementation);
    let a1 = answer(&mut r, "w", &[ans(&confirm, PlanAct::Defer, Some("later"))]);
    let p1 = r.read().reader().plan(&a1).unwrap();
    assert_eq!(
        (p1.stable, p1.prev, p1.proposal),
        (Some(bh), Some(bh), None)
    );
    // a commit before the landing
    commit(
        repo.path(),
        "f.rs",
        &format!("follow\n\nImplements: {}", s.a),
    );
    assert_eq!(
        row(&whole(&r, "w"), &code)["how"],
        "committed",
        "on the old baseline"
    );

    // the bound version lands with no seal: the chain is over though its head binds nothing
    r.land(&approver(), &version.to_string()).unwrap();
    assert_eq!(plan_ref(&r, "w"), Some(a1), "no seal was written");
    let v = whole(&r, "w");
    assert_eq!(v["plan"], Value::Null, "{v}");
    assert_eq!(row(&v, &confirm)["state"], "open", "the old answer is gone");
    assert!(row(&v, &confirm).get("answer").is_none());
    assert_eq!(
        row(&v, &code)["state"],
        "open",
        "the pre-landing commit closes nothing"
    );

    // the store refuses a version that continues the ended chain
    let mut cont = p1.clone();
    cont.prev = Some(a1);
    cont.heads = vec![];
    let e = r.store_mut().put_plan(&cont).unwrap_err();
    assert!(
        e.to_string().ends_with(": invalid chain"),
        "a version after an ended chain: {e}"
    );

    // the next answer starts a new chain, with the HEAD captured now as its baseline
    let head_now = git(repo.path(), &["rev-parse", "HEAD"]);
    let next = answer(&mut r, "w", &[ans(&code, PlanAct::Defer, Some("soon"))]);
    let p = r.read().reader().plan(&next).unwrap();
    assert_eq!((p.stable, p.prev), (None, None), "a new chain");
    assert_eq!(
        p.heads,
        vec![PlanHead {
            repo: "app".into(),
            head: Some(head_now),
        }]
    );
    assert_eq!(
        p.answers.iter().map(|a| a.item).collect::<Vec<_>>(),
        vec![item_hash(
            &id(&s.a),
            "app:f.rs",
            PlanKind::UpdateImplementation
        )],
        "the old chain's answers stay in its own versions"
    );
    let v = whole(&r, "w");
    assert_eq!(v["plan"], next.to_string());
    assert_eq!(row(&v, &confirm)["state"], "open");
    assert_eq!(row(&v, &code)["state"], "deferred");
    // a commit after the new baseline closes the code item
    commit(
        repo.path(),
        "f.rs",
        &format!("again\n\nImplements: {}", s.a),
    );
    assert_eq!(row(&whole(&r, "w"), &code)["how"], "committed");
    // fsck reads the new chain as the plan writer does (O6)
    r.fsck().unwrap();
}

/// Final review F3: a publish whose batch holds a PLAN re-reads only the `plan/<view>` refs of
/// the views its PLAN frames name, and shares the rest of the captured heads; the other views'
/// chains (live, sealed or ended) add no ref read. A full publish reads every plan ref.
#[test]
fn cp8_fix1_a_plan_write_reads_only_its_own_views_refs() {
    use rdy_engine::plan::api::plan_ref_reads_for_tests as reads;
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let confirm = pi(&s.a, &s.b, PlanKind::Confirm);
    for n in 0..8 {
        let v = format!("w{n}");
        edit(&mut r, &v, &s.a, &format!("A holds {} days.", 40 + n));
        answer(&mut r, &v, &[ans(&confirm, PlanAct::Defer, Some("later"))]);
    }
    let all = r.read().reader().refs_with_prefix("plan/").unwrap().len();
    assert!(all >= 8, "{all} plan refs");
    let captured = r.read();
    let at = reads();
    let h = answer(
        &mut r,
        "w3",
        &[ans(&confirm, PlanAct::Confirm, Some("holds"))],
    );
    assert_eq!(reads() - at, 1, "one ref read for the one view written");
    assert_eq!(whole(&r, "w3")["plan"], h.to_string());
    assert_eq!(plan_ref(&r, "w3"), Some(h));
    // the other views keep their heads, and the captured state its own
    for n in [0, 7] {
        let v = format!("w{n}");
        assert_eq!(whole(&r, &v)["plan"], plan_ref(&r, &v).unwrap().to_string());
    }
    let q = PlanQuery {
        max_tokens: Some(100_000),
        ..view_q("w3")
    };
    let old = serde_json::to_value(captured.change_plan(&q, "work").unwrap()).unwrap();
    assert_ne!(
        old["plan"],
        h.to_string(),
        "the captured state reads no later answer"
    );
    // a full publish reads them all
    let at = reads();
    r.invalidate();
    let _ = r.read();
    assert_eq!(reads() - at, all as u64);
    assert_eq!(whole(&r, "w3")["plan"], h.to_string());
}
