//! ok10: replay through the shared view dispatcher. `run_view` takes `{op: "replay", view?,
//! changes, max_tokens?}`, runs the engine's replay as one view op with its consequences, and
//! refuses every other field. `view show` names each change's replay source in `replayed_from`.
//! A refused replay publishes nothing.
mod common;

use rdy_engine::engine::{Engine, Local};
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::sync::Arc;

const HUMAN: &str = "human:founder";

fn human() -> Session {
    Session::human(HUMAN.into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human(HUMAN.into(), Role::Approver).unwrap()
}

fn agent(actor: &str, run: &str) -> Session {
    Session::agent(actor.into(), Some(run.into()), Role::Writer).unwrap()
}

fn write(r: &mut Rdy, s: &Session, view: &str, ops: Value) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&human(), view, "ok10", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

fn req(v: Value) -> ViewRequest {
    serde_json::from_value(v).unwrap()
}

/// A landed canon with prose `x` and `y`, agent b's two updates in view `src`, and view `mine`
/// owned by agent a.
struct P {
    _dir: tempfile::TempDir,
    e: Local,
    x: String,
    y: String,
    /// b's update of x, and of y.
    cx: String,
    cy: String,
    a: Session,
}

fn project() -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        &human(),
        "seed",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area"}},
            {"op": "create", "type": "doc", "id": "$one", "parent": "$f", "fields": {"name": "one", "title": "One", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$one", "body": "Text x."},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$one", "body": "Text y."}
        ]),
    );
    land(&mut r, "seed");
    let (x, y) = (w.created["$x"].clone(), w.created["$y"].clone());
    let b = agent("agent:b", "b1");
    let cx = write(
        &mut r,
        &b,
        "src",
        json!([{"op": "update", "id": x, "body": "X by b."}]),
    )
    .change;
    let cy = write(
        &mut r,
        &b,
        "src",
        json!([{"op": "update", "id": y, "body": "Y by b."}]),
    )
    .change;
    let a = agent("agent:a", "a1");
    r.view_create(&a, "mine", None, false).unwrap();
    P {
        _dir: dir,
        e: Local::new(r),
        x,
        y,
        cx,
        cy,
        a,
    }
}

impl P {
    fn run(&self, default_view: &str, v: Value) -> Result<Value, RdyError> {
        run_view(&self.e, &self.a, HUMAN, default_view, req(v))
    }

    fn write_a(&self, view: &str, ops: Value) -> String {
        let (a, view) = (self.a.clone(), view.to_string());
        let out = self
            .e
            .write(Box::new(move |r| {
                Ok(json!(write(r, &a, &view, ops).change))
            }))
            .unwrap();
        out.as_str().unwrap().to_string()
    }

    fn head(&self, view: &str) -> String {
        self.e.read().view_head(view).unwrap().head.to_string()
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("not a list: {v}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn refused(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

#[test]
fn ok10_the_dispatcher_replays_into_the_view_with_its_consequences() {
    let p = project();
    let v = p
        .run(
            "mine",
            json!({"op": "replay", "changes": [p.cx, p.cy], "max_tokens": 2000}),
        )
        .unwrap();
    assert_eq!(v["view"], "mine");
    // every ReplayResult field, and the consequences beside them
    for key in [
        "head",
        "changes",
        "replayed",
        "added",
        "added_deps",
        "removed",
        "cascaded",
        "conflicts",
        "consequences",
    ] {
        assert!(v.get(key).is_some(), "no {key}: {v}");
    }
    assert_eq!(v["changes"], 2);
    assert_eq!(v["head"], p.head("mine"), "the head is the one include op");
    let mut sources: Vec<String> = v["replayed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["source"].as_str().unwrap().to_string())
        .collect();
    sources.sort();
    let mut want = vec![p.cx.clone(), p.cy.clone()];
    want.sort();
    assert_eq!(sources, want);
    let news: Vec<String> = v["replayed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["change"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(strings(&v["added"]), news, "added holds the new changes");
    assert!(news.iter().all(|n| *n != p.cx && *n != p.cy));
    assert_eq!(v["conflicts"], json!([]), "{v}");
    assert!(v["consequences"].is_array(), "{v}");
    // the view includes exactly the two new changes
    let shown = p.run("mine", json!({"op": "show"})).unwrap();
    let mut ids: Vec<String> = shown["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    let mut news_sorted = news.clone();
    news_sorted.sort();
    assert_eq!(ids, news_sorted);
    // the view op is an include
    assert_eq!(shown["ops"][0]["kind"], "include", "{shown}");
}

#[test]
fn ok10_the_default_view_is_the_sessions_view() {
    let p = project();
    // `view` omitted: the dispatcher's default view
    let v = p
        .run("mine", json!({"op": "replay", "changes": [p.cx]}))
        .unwrap();
    assert_eq!(v["view"], "mine");
    // a named view overrides the default
    let e = p
        .run("nope", json!({"op": "replay", "changes": [p.cy]}))
        .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    let v = p
        .run(
            "nope",
            json!({"op": "replay", "view": "mine", "changes": [p.cy]}),
        )
        .unwrap();
    assert_eq!(v["view"], "mine");
    assert_eq!(v["changes"], 2);
}

#[test]
fn ok10_a_divergent_replay_reports_the_conflict_with_its_base_and_sides() {
    let p = project();
    let base =
        p.e.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .version_of(&BlockId::parse(&p.x).unwrap())
            .unwrap()
            .to_string();
    let mine = p.write_a(
        "mine",
        json!([{"op": "update", "id": p.x, "body": "X by a."}]),
    );
    let v = p
        .run("mine", json!({"op": "replay", "changes": [p.cx]}))
        .unwrap();
    let new = v["replayed"][0]["change"].as_str().unwrap().to_string();
    let conflicts = v["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{v}");
    let c = &conflicts[0];
    assert_eq!(c["block"], p.x);
    assert_eq!(c["base"], base, "the conflict keeps the source's base");
    let side_changes: Vec<&str> = c["sides"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["change"].as_str())
        .collect();
    assert!(
        side_changes.contains(&mine.as_str()) && side_changes.contains(&new.as_str()),
        "both sides are named: {c}"
    );
    // the consequences list the conflict to resolve, beside the reply's conflict
    let entry = v["consequences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "conflict" && e["block"] == p.x)
        .unwrap_or_else(|| panic!("no conflict consequence: {v}"));
    assert_eq!(entry["conflict"], c["id"]);
    assert_eq!(entry["follow_up"], "resolve");
    // a later read gives the same conflict: base, sides and id
    let shown = p.run("mine", json!({"op": "show"})).unwrap();
    assert_eq!(shown["conflicts"], v["conflicts"], "{shown}");
}

#[test]
fn ok10_replay_takes_only_op_view_changes_and_max_tokens() {
    let p = project();
    let before = p.head("mine");
    // every other field, alone
    let others: [(&str, Value); 20] = [
        ("name", json!("n")),
        ("from", json!("f")),
        ("base", json!("b")),
        ("pin", json!(false)),
        ("views", json!(["v"])),
        ("cascade", json!(false)),
        ("a", json!("a")),
        ("b", json!("b")),
        ("all", json!(false)),
        ("stale", json!(3)),
        ("reason", json!("r")),
        ("owner", json!({"actor": "agent:z"})),
        ("none", json!(true)),
        ("target", json!("t")),
        ("block", json!("c-AAAAA")),
        ("landed", json!(true)),
        ("evidence", json!("words")),
        ("question", json!("q")),
        ("session_ref", json!("s")),
        ("relayed_by", json!("agent:a")),
    ];
    for (field, value) in &others {
        let mut r = json!({"op": "replay", "changes": [p.cx]});
        r[field] = value.clone();
        let e = p.run("mine", r).unwrap_err();
        refused(
            &e,
            E_BAD_INPUT,
            &format!("replay does not take {field}"),
            &format!("drop {field}, or use the view operation that takes it"),
        );
    }
    // the first supplied field in declaration order wins, whatever the key order
    let e = p
        .run(
            "mine",
            json!({"op": "replay", "changes": [p.cx], "relayed_by": "agent:a", "cascade": true, "from": "x", "evidence": "w"}),
        )
        .unwrap_err();
    assert_eq!(e.message, "replay does not take from", "{e}");
    let e = p
        .run(
            "mine",
            json!({"op": "replay", "changes": [p.cx], "evidence": "w", "relayed_by": "agent:a"}),
        )
        .unwrap_err();
    assert_eq!(e.message, "replay does not take evidence", "{e}");
    // no changes, or an empty list: the engine's one text
    for r in [
        json!({"op": "replay"}),
        json!({"op": "replay", "changes": []}),
    ] {
        let e = p.run("mine", r).unwrap_err();
        refused(
            &e,
            E_BAD_INPUT,
            "name at least one change to replay",
            "call rdy_view with op replay and changes, or rdy view replay <view> <change>...",
        );
    }
    // more changes than one replay takes
    let many: Vec<String> = (0..1001).map(|_| p.cx.clone()).collect();
    let e = p
        .run("mine", json!({"op": "replay", "changes": many}))
        .unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e}");
    assert_eq!(p.head("mine"), before, "nothing was written");
}

#[test]
fn ok10_view_show_names_the_replay_source_of_each_change() {
    let p = project();
    let own = p.write_a(
        "mine",
        json!([{"op": "update", "id": p.y, "body": "Y by a."}]),
    );
    let v = p
        .run("mine", json!({"op": "replay", "changes": [p.cx]}))
        .unwrap();
    let new = v["replayed"][0]["change"].as_str().unwrap().to_string();
    let shown = p.run("mine", json!({"op": "show"})).unwrap();
    let rows = shown["changes"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{shown}");
    for row in rows {
        // every row has the key; a full hash for the replay, null for any other change
        let from = row
            .get("replayed_from")
            .unwrap_or_else(|| panic!("no replayed_from: {row}"));
        if row["id"] == new {
            assert_eq!(*from, json!(p.cx), "the full source hash");
            assert_eq!(p.cx.len(), 64);
        } else {
            assert_eq!(row["id"], own);
            assert_eq!(*from, Value::Null);
        }
    }
    // the stored op kind stays include
    assert_eq!(shown["ops"][0]["kind"], "include", "{shown}");
}

#[test]
fn ok10_a_refused_replay_publishes_nothing() {
    let p = project();
    let before: Arc<_> = p.e.read();
    let head = p.head("mine");
    let src_before = p.head("src");
    // one valid change and one that does not exist
    let ghost = "ab".repeat(32);
    for r in [
        json!({"op": "replay", "changes": [p.cx, ghost]}),
        json!({"op": "replay", "changes": ["nonsense"]}),
        json!({"op": "replay", "changes": [p.cx], "cascade": true}),
        json!({"op": "replay"}),
    ] {
        let e = p.run("mine", r).unwrap_err();
        assert!(e.code == E_BAD_INPUT || e.code == E_NOT_FOUND, "{e}");
    }
    // someone else's view
    let other = Session::agent("agent:c".into(), Some("c1".into()), Role::Writer).unwrap();
    let e = run_view(
        &p.e,
        &other,
        HUMAN,
        "mine",
        req(json!({"op": "replay", "changes": [p.cx]})),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let after = p.e.read();
    assert!(
        Arc::ptr_eq(&before, &after),
        "no state was published after the refusals"
    );
    assert_eq!(p.head("mine"), head);
    assert_eq!(p.head("src"), src_before);
    assert!(after.included("mine").is_empty());
}

#[test]
fn ok10_one_undo_removes_a_whole_replay() {
    let p = project();
    let v = p
        .run("mine", json!({"op": "replay", "changes": [p.cx, p.cy]}))
        .unwrap();
    assert_eq!(v["changes"], 2);
    let head = v["head"].as_str().unwrap().to_string();
    let u = p.run("mine", json!({"op": "undo"})).unwrap();
    assert_eq!(u["kind"], "include", "{u}");
    assert_eq!(u["undone"], head, "the undo target is the replay's op");
    assert_eq!(
        strings(&u["excluded"]).len(),
        2,
        "one undo takes out both new changes: {u}"
    );
    assert!(p.e.read().included("mine").is_empty());
    // the source view still holds b's changes
    assert_eq!(p.e.read().included("src").len(), 2);
}
