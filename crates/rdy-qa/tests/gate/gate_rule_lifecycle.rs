//! rtc2 (FW-MH.rules, D-ZSRBN): one complete rule change and its recoverable view. A rule request
//! writes its default doc, its decision and its instruction in one normal write to a private view
//! named by the request, then proposes it. Target and safety refusals come before any write; a
//! failed step closes only that view; a repeated request reconciles the view it finds.
use rdy_engine::instructions::{BOUNDS, Layer, LoadRequest, collect_with, merge_with};
use rdy_engine::rule_types::{RuleRequest, parse_request};
use rdy_engine::rules::{
    Destination, Prepared, PreparedState, RuleCaller, RuleFault, RuleMutation,
    with_rule_fault_for_tests,
};
use rdy_engine::{
    E_BAD_INPUT, E_BRIEF_REQUIRED, E_INTERNAL, E_NOT_FOUND, E_STALE_BASE, E_VIEW_CLOSED, Rdy,
    RdyError, Scope, Session,
};
use rdy_model::{BlockId, FieldValue, Hash, Op};
use rdy_qa::fixtures::*;
use rdy_qa::intake::{accept_deferrals, confirm_exceptions};
use rdy_store::view::ViewOpKind;
use serde_json::{Value, json};
use std::path::Path;

const STABLE_FIX: &str =
    "brief the returned blocks and use the normal write, propose and answer path";
const SHARED_FIX: &str = "brief the returned blocks and use the normal decision workflow";

fn me() -> Session {
    agent("agent:a", Some("r1"))
}

fn req(v: Value) -> RuleRequest {
    parse_request(&v).unwrap_or_else(|e| panic!("{v}: {e}"))
}

fn prepare_in(r: &mut Rdy, d: Destination, v: Value) -> Result<Prepared, RdyError> {
    let s = me();
    let q = req(v);
    r.rule_prepare(
        &RuleCaller {
            session: &s,
            person: "human:qa",
            surface: "mcp",
            destination: d,
        },
        RuleMutation::of(&q).unwrap(),
    )
}

fn prepare(r: &mut Rdy, v: Value) -> Result<Prepared, RdyError> {
    prepare_in(r, Destination::Project, v)
}

fn add(text: &str) -> Value {
    json!({"scope": "project", "op": "add", "text": text, "why": "They asked.",
           "evidence": "yes, keep it", "question": "Save this?", "session_ref": "chat-1"})
}

fn edit(id: &str, text: &str) -> Value {
    json!({"scope": "project", "op": "edit", "id": id, "text": text, "why": "Wider.",
           "evidence": "change it", "session_ref": "chat-1"})
}

fn remove(id: &str) -> Value {
    json!({"scope": "project", "op": "remove", "id": id, "why": "No longer wanted.",
           "evidence": "drop it", "session_ref": "chat-1"})
}

/// Lands the proposal `p` stopped at, the normal way: the person's cards, then the landing.
fn land_prepared(r: &mut Rdy, p: &Prepared) -> String {
    let PreparedState::Proposed { reference, version } = &p.state else {
        panic!("{:?} is not proposed", p.state);
    };
    accept_deferrals(r, &approver(), version, reference);
    confirm_exceptions(r, &approver(), reference);
    r.land(&approver(), version)
        .unwrap_or_else(|e| panic!("landing {reference}: {e} {:?}", e.issues))
}

fn added(r: &mut Rdy, text: &str) -> Prepared {
    let p = prepare(r, add(text)).unwrap_or_else(|e| panic!("add: {e}"));
    land_prepared(r, &p);
    p
}

/// What must not move when a request is refused: canon, every view's head and the log's end.
fn state_of(dir: &Path, r: &Rdy) -> (Option<Hash>, Vec<(String, Hash)>, u64) {
    let st = r.read();
    let heads = st
        .view_names()
        .into_iter()
        .map(|v| {
            let h = st.view_head(&v).unwrap().head;
            (v, h)
        })
        .collect();
    let log = log_bytes(&dir.join(".rdy"));
    assert!(log > 0, "the log is measured");
    (st.canon_head(), heads, log)
}

/// The bytes of every segment under `dir`: the log's length.
fn log_bytes(dir: &Path) -> u64 {
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let path = e.path();
        if e.file_type().unwrap().is_dir() {
            if e.file_name() != "index" {
                n += log_bytes(&path);
            }
        } else if e.file_name().to_string_lossy().starts_with("seg-") {
            n += e.metadata().unwrap().len();
        }
    }
    n
}

fn block(r: &Rdy, scope: &Scope, id: &str) -> rdy_model::Block {
    r.read()
        .block(scope, &BlockId::parse(id).unwrap())
        .unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn text_field(b: &rdy_model::Block, name: &str) -> String {
    b.field_str(name)
        .unwrap_or_else(|| panic!("{} has no {name}", b.id))
        .to_string()
}

fn selectors(b: &rdy_model::Block) -> Vec<String> {
    match b.fields.get("applies_to") {
        Some(FieldValue::List(items)) => items
            .iter()
            .map(|i| i.as_str().unwrap().to_string())
            .collect(),
        other => panic!("applies_to is {other:?}"),
    }
}

fn link_targets(b: &rdy_model::Block, kind: &str) -> Vec<String> {
    b.links
        .get(kind)
        .into_iter()
        .flatten()
        .map(|l| l.target.to_string())
        .collect()
}

/// The doc at the top-level path `path` in `scope`.
fn doc_at(r: &Rdy, scope: &Scope, path: &str) -> rdy_model::Block {
    let m = r.read().materialized(scope).unwrap();
    let id = m.child_named(None, path).unwrap();
    m.get(&id).unwrap().clone()
}

fn children(r: &Rdy, scope: &Scope, id: &BlockId) -> Vec<rdy_model::Block> {
    let m = r.read().materialized(scope).unwrap();
    m.children(Some(id)).into_iter().cloned().collect()
}

/// The ids of the required rows and the menu's hints of doc `doc`, from canon.
fn delivered(r: &Rdy) -> (Vec<String>, Vec<(String, bool, Vec<String>)>) {
    let load = LoadRequest::default();
    let part = collect_with(&r.read(), Layer::Project, &load, &BOUNDS).unwrap();
    let reply = merge_with(vec![part], &load, &BOUNDS).unwrap();
    (
        reply
            .instructions
            .iter()
            .map(|i| i.id.to_string())
            .collect(),
        reply
            .menu
            .iter()
            .map(|m| {
                (
                    m.id.to_string(),
                    m.required,
                    m.hints.iter().map(ToString::to_string).collect(),
                )
            })
            .collect(),
    )
}

fn close_reason(r: &Rdy, view: &str) -> Option<String> {
    let st = r.read();
    let head = st.view_head(view)?;
    if !head.closed {
        return None;
    }
    let mut cur = Some(head.head);
    while let Some(h) = cur {
        let op = st.reader().view_op(&h).unwrap();
        if let ViewOpKind::Close { reason } = op.op {
            return Some(reason);
        }
        cur = op.prev;
    }
    None
}

fn store_format(dir: &Path) -> Vec<u8> {
    std::fs::read(dir.join(".rdy/store/FORMAT")).unwrap()
}

#[test]
fn rtc2_add_creates_required_draft_default_once() {
    let (dir, mut r) = project();
    let format = store_format(dir.path());
    let text = "Run the tests\n\n  before you push.  ";
    let p = prepare(&mut r, add(text)).unwrap();
    let scope = Scope::View(p.view.clone());

    // the default doc: every field exact, and the pack's skeleton
    let doc = doc_at(&r, &scope, "project-rules");
    assert_eq!(text_field(&doc, "title"), "Project rules");
    assert_eq!(text_field(&doc, "lifecycle"), "draft");
    assert_eq!(text_field(&doc, "key"), "project-rules");
    assert_eq!(text_field(&doc, "doc_type"), "overview");
    assert_eq!(doc.fields.get("required"), Some(&FieldValue::Bool(true)));
    assert_eq!(p.rule.ruleset, doc.id.to_string());
    let kids = children(&r, &scope, &doc.id);
    let sections: Vec<String> = kids
        .iter()
        .filter(|b| b.block_type == "section")
        .map(|b| text_field(b, "title"))
        .collect();
    assert_eq!(sections, ["Summary", "Goals", "Non-goals", "Context"]);

    // the rule: exact bytes, default selector, unlocked, one reason
    let rule = block(&r, &scope, &p.rule.id);
    assert_eq!(rule.block_type, "instruction");
    assert_eq!(rule.body, text);
    assert_eq!(selectors(&rule), ["all"]);
    assert_eq!(rule.fields.get("locked"), Some(&FieldValue::Bool(false)));
    assert_eq!(rule.parent, Some(doc.id.clone()));
    assert_eq!(link_targets(&rule, "decided_by"), [p.decision.clone()]);
    assert_eq!(p.rule.text, text);
    assert!(!p.rule.retired);

    // the reason: at the root, a direct ruling on the person's relayed words
    let d = block(&r, &scope, &p.decision);
    assert_eq!(d.parent, None);
    assert_eq!(text_field(&d, "statement"), format!("Rule: {text}"));
    assert_eq!(text_field(&d, "status"), "proposed");
    assert_eq!(text_field(&d, "authority_basis"), "direct_ruling");
    assert_eq!(text_field(&d, "built"), "none");
    assert_eq!(text_field(&d, "reversibility"), "two_way");
    assert_eq!(text_field(&d, "why"), "They asked.");
    assert_eq!(
        text_field(&d, "attested_by"),
        "human:qa, relayed by agent:a (run r1); evidence: yes, keep it"
    );
    assert!(d.body.contains("Save this?"), "{}", d.body);
    assert!(d.body.contains("chat-1"), "{}", d.body);
    assert!(!d.fields.contains_key("kind"));
    assert!(!d.fields.contains_key("recommend"));

    // one change holds it all, canon holds none of it, and the caller's own view is untouched
    assert_eq!(r.read().included(&p.view).len(), 1);
    assert!(r.read().canon_head().is_none());
    assert!(r.read().view_head(&me().default_view()).is_none());
    assert!(matches!(p.state, PreparedState::Proposed { .. }));
    assert_eq!(p.scope.as_str(), "project");
    assert_eq!(p.project.repo_id, r.read().repo_id().unwrap().to_string());

    // it lands, is delivered as required, and activates no store feature
    land_prepared(&mut r, &p);
    let (rows, menu) = delivered(&r);
    assert_eq!(rows, [p.rule.id.clone()]);
    assert_eq!(menu.len(), 1);
    assert!(menu[0].1, "a default doc is required");
    assert_eq!(store_format(dir.path()), format);

    // the second rule reuses the doc: no other doc, and the doc's version does not move
    let doc_version = version_of(&r, &Scope::Canon, doc.id.as_str());
    let second = added(&mut r, "Keep commits small.");
    assert_eq!(second.rule.ruleset, doc.id.to_string());
    assert_eq!(version_of(&r, &Scope::Canon, doc.id.as_str()), doc_version);
    let docs = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .filter(|b| b.block_type == "doc")
        .count();
    assert_eq!(docs, 1);

    // the meta project's default has its own name
    let (_m, mut meta) = project();
    let mine = prepare_in(
        &mut meta,
        Destination::Meta,
        json!({"scope": "person", "op": "add", "text": "Be brief.", "why": "Asked.", "evidence": "yes"}),
    )
    .unwrap();
    let scope = Scope::View(mine.view.clone());
    let doc = doc_at(&meta, &scope, "my-rules");
    assert_eq!(text_field(&doc, "title"), "My rules");
    assert_eq!(text_field(&doc, "key"), "my-rules");
    assert_eq!(mine.scope.as_str(), "person");
    assert_eq!(
        text_field(&block(&meta, &scope, &mine.decision), "attested_by"),
        "human:qa, relayed by agent:a (run r1); evidence: yes"
    );

    // a doc at the default path that is not a draft required ruleset is never changed
    let (d3, mut r3) = project();
    for fields in [
        json!({"name": "project-rules", "title": "Mine", "doc_type": "page"}),
        json!({"name": "project-rules", "title": "Mine", "doc_type": "page", "lifecycle": "draft"}),
        json!({"name": "project-rules", "title": "Mine", "doc_type": "page", "required": true}),
    ] {
        let view = format!("seed-{}", fields.as_object().unwrap().len());
        write(
            &mut r3,
            &view,
            json!([{"op": "create", "type": "doc", "fields": fields}]),
        )
        .unwrap();
        land(&mut r3, &view).unwrap();
        let before = state_of(d3.path(), &r3);
        let e = prepare(&mut r3, add("A rule.")).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(
            e.message,
            "default ruleset project-rules is not a draft required ruleset"
        );
        assert_eq!(
            e.fix,
            "name a suitable ruleset, or change this doc through the normal path"
        );
        assert_eq!(state_of(d3.path(), &r3), before);
        let id = doc_at(&r3, &Scope::Canon, "project-rules").id.to_string();
        write(&mut r3, "gone", json!([{"op": "delete", "id": id}])).unwrap();
        land(&mut r3, "gone").unwrap();
    }

    // the decision home decides the reason's parent, and the rule keeps its own doc
    let (d4, r4) = project();
    let mut r4 = r4;
    let w = write(&mut r4, "seed", json!([
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "log", "title": "Log", "doc_type": "page"}}
    ]))
    .unwrap();
    land(&mut r4, "seed").unwrap();
    r4.close().unwrap();
    home_at(d4.path(), "log");
    let mut r4 = Rdy::open(d4.path()).unwrap();
    let p = prepare(&mut r4, add("Homed.")).unwrap();
    let d = block(&r4, &Scope::View(p.view.clone()), &p.decision);
    assert_eq!(
        d.parent.as_ref().map(ToString::to_string),
        Some(w.created["$l"].clone())
    );
}

/// Points `decisions.home` of the project at `dir` to `home`.
fn home_at(dir: &Path, home: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join(".rdy.yaml"))
        .unwrap();
    writeln!(f, "decisions:\n  home: {home}").unwrap();
}

#[test]
fn rtc2_named_ruleset_keeps_required_and_selector_semantics() {
    let (_dir, mut r) = project();
    let w = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "named", "title": "Named", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "S"}}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let named = w.created["$n"].clone();
    let named_version = version_of(&r, &Scope::Canon, &named);

    // a narrow rule in an optional doc is a hint, not a row; so is the same doc by its other names
    let narrow = |text: &str, ruleset: &str| {
        json!({"scope": "project", "op": "add", "text": text, "why": "w", "evidence": "e",
               "ruleset": ruleset, "applies_to": ["type:claim"]})
    };
    let one = prepare(&mut r, narrow("Narrow one.", "named")).unwrap();
    assert_eq!(one.rule.ruleset, named);
    land_prepared(&mut r, &one);
    let two = prepare(&mut r, narrow("Narrow two.", &named)).unwrap();
    land_prepared(&mut r, &two);
    let three = prepare(&mut r, narrow("Narrow three.", "project:named")).unwrap();
    land_prepared(&mut r, &three);
    let (rows, menu) = delivered(&r);
    assert!(rows.is_empty(), "{rows:?}");
    assert_eq!(
        menu,
        [(
            format!("project:{named}"),
            false,
            vec!["type:claim".to_string()]
        )]
    );
    assert_eq!(
        version_of(&r, &Scope::Canon, &named),
        named_version,
        "a named doc does not change"
    );
    assert_eq!(
        selectors(&block(&r, &Scope::Canon, &one.rule.id)),
        ["type:claim"]
    );

    // an "all" rule in the same optional doc is delivered on its own
    let all = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "For all.", "why": "w", "evidence": "e",
               "ruleset": "named"}),
    )
    .unwrap();
    land_prepared(&mut r, &all);
    assert_eq!(delivered(&r).0, [all.rule.id.clone()]);

    // a narrow rule in the required default doc is delivered, and keeps its hint
    let default = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "Narrow but required.", "why": "w", "evidence": "e",
               "applies_to": ["type:claim"]}),
    )
    .unwrap();
    land_prepared(&mut r, &default);
    let (rows, menu) = delivered(&r);
    assert!(rows.contains(&default.rule.id), "{rows:?}");
    let entry = menu
        .iter()
        .find(|m| m.0 == format!("project:{}", default.rule.ruleset))
        .unwrap();
    assert!(entry.1);
    assert_eq!(entry.2, ["type:claim"]);

    // an unknown area warns as ever, and a missing or mistyped ruleset stops
    let warned = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "Unknown area.", "why": "w", "evidence": "e",
               "applies_to": ["area:nowhere"]}),
    )
    .unwrap();
    assert!(
        warned
            .warnings
            .iter()
            .any(|w| w.code == "W_INSTRUCTION_SELECTOR"),
        "{:?}",
        warned.warnings
    );
    let e = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "x", "why": "w", "evidence": "e", "ruleset": "nowhere"}),
    )
    .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    assert_eq!(e.message, "rule target nowhere was not found");
    assert_eq!(
        e.fix,
        "read the selected project and use an existing instruction or ruleset"
    );
    let section = w.created["$s"].clone();
    let e = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "x", "why": "w", "evidence": "e", "ruleset": section}),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(
        e.message,
        format!("rule target {section} is a section, not doc")
    );
    assert_eq!(e.fix, "use an instruction id for id and a doc for ruleset");
}

#[test]
fn rtc2_edit_supersedes_and_remove_retires_revokes() {
    let (_dir, mut r) = project();
    let first = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "Run the tests.", "why": "w", "evidence": "e",
               "applies_to": ["type:claim", "all"]}),
    )
    .unwrap();
    land_prepared(&mut r, &first);
    let id = first.rule.id.clone();
    let old_version = version_of(&r, &Scope::Canon, &id);

    // an edit keeps the id, the doc and the selectors, and has one current reason
    let edited = prepare(&mut r, edit(&id, "Run all the tests.")).unwrap();
    assert_eq!(edited.rule.id, id);
    assert_eq!(edited.rule.ruleset, first.rule.ruleset);
    assert_eq!(edited.rule.text, "Run all the tests.");
    let d = block(&r, &Scope::View(edited.view.clone()), &edited.decision);
    assert_eq!(link_targets(&d, "supersedes"), [first.decision.clone()]);
    assert_eq!(text_field(&d, "statement"), "Rule: Run all the tests.");
    assert_eq!(text_field(&d, "why"), "Wider.");
    land_prepared(&mut r, &edited);
    let rule = block(&r, &Scope::Canon, &id);
    assert_eq!(rule.body, "Run all the tests.");
    assert_eq!(selectors(&rule), ["type:claim", "all"]);
    assert_eq!(link_targets(&rule, "decided_by"), [edited.decision.clone()]);
    let st = r.read();
    let g = st.graph(&Scope::Canon).unwrap();
    let status = |i: &str| g.status(&BlockId::parse(i).unwrap()).unwrap().tag();
    assert_eq!(status(&first.decision), "superseded");
    assert_eq!(status(&edited.decision), "live");
    // the old version and the old reason stay in the record
    let old: Hash = old_version.parse().unwrap();
    assert_eq!(st.reader().get_block(&old).unwrap().body, "Run the tests.");
    drop(g);
    drop(st);
    assert_eq!(delivered(&r).0, [id.clone()]);

    // an edit with new selectors replaces them
    let narrowed = prepare(
        &mut r,
        json!({"scope": "project", "op": "edit", "id": id, "text": "Run every test.", "why": "w",
               "evidence": "e", "applies_to": ["type:section"]}),
    )
    .unwrap();
    land_prepared(&mut r, &narrowed);
    assert_eq!(selectors(&block(&r, &Scope::Canon, &id)), ["type:section"]);

    // a remove retires the rule and revokes its reason
    let gone = prepare(&mut r, remove(&id)).unwrap();
    assert_eq!(gone.rule.id, id);
    assert!(gone.rule.retired);
    assert_eq!(gone.rule.text, "Run every test.");
    let d = block(&r, &Scope::View(gone.view.clone()), &gone.decision);
    assert_eq!(link_targets(&d, "revokes"), [narrowed.decision.clone()]);
    assert_eq!(
        text_field(&d, "statement"),
        format!("Retire rule {id}: No longer wanted.")
    );
    assert_eq!(text_field(&d, "built"), "none");
    assert!(
        !gone.warnings.iter().any(|w| w.code.contains("UNAPPLIED")),
        "{:?}",
        gone.warnings
    );
    land_prepared(&mut r, &gone);
    assert!(
        r.read()
            .block(&Scope::Canon, &BlockId::parse(&id).unwrap())
            .is_err()
    );
    let st = r.read();
    let g = st.graph(&Scope::Canon).unwrap();
    assert_eq!(
        g.status(&BlockId::parse(&narrowed.decision).unwrap())
            .unwrap()
            .tag(),
        "revoked"
    );
    assert_eq!(
        g.status(&BlockId::parse(&gone.decision).unwrap())
            .unwrap()
            .tag(),
        "live"
    );
    drop(g);
    drop(st);
    assert!(delivered(&r).0.is_empty());

    // edit and remove need an existing instruction, with one live whole-rule reason
    let e = prepare(&mut r, edit(&id, "back")).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    assert_eq!(e.message, format!("rule target {id} was not found"));
    let doc = first.rule.ruleset.clone();
    let e = prepare(&mut r, remove(&doc)).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(
        e.message,
        format!("rule target {doc} is a doc, not instruction")
    );

    let two_reasons = {
        let d1 = r
            .decide(
                &writer(),
                "two",
                rdy_engine::DecisionInput {
                    statement: "One.".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .created["$decision"]
            .clone();
        let d2 = r
            .decide(
                &writer(),
                "two",
                rdy_engine::DecisionInput {
                    statement: "Two.".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .created["$decision"]
            .clone();
        let w = write(
            &mut r,
            "two",
            json!([
                {"op": "create", "type": "instruction", "id": "$i", "parent": doc,
                 "fields": {"applies_to": ["all"]}, "body": "Two reasons.",
                 "links": {"decided_by": [{"target": d1}, {"target": d2}]}}
            ]),
        )
        .unwrap();
        land(&mut r, "two").unwrap();
        w.created["$i"].clone()
    };
    let e = prepare(&mut r, edit(&two_reasons, "x")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(
        e.message,
        format!("instruction {two_reasons} needs one live whole-rule decision")
    );
    assert_eq!(e.fix, "use the normal decision and instruction workflow");
}

#[test]
fn rtc2_shared_decision_stops_before_any_write() {
    let (dir, mut r) = project();
    let p = added(&mut r, "Shared rule.");
    let (id, decision, doc) = (
        p.rule.id.clone(),
        p.decision.clone(),
        p.rule.ruleset.clone(),
    );

    // a prose mention and the doc's structure are no dependents: nothing stops yet
    write(&mut r, "mention", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc, "fields": {"title": "Mention"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": format!("See {{{{{decision}}}}}.")}
    ]))
    .unwrap();
    land(&mut r, "mention").unwrap();
    let probe = prepare(
        &mut r,
        json!({"scope": "project", "op": "edit", "id": id, "text": "Probe.", "why": "w", "evidence": "probe",
               "session_ref": "probe"}),
    )
    .unwrap();
    r.view_close(&me(), &[probe.view.clone()], "probe").unwrap();

    // dependents in canon and in other open views
    let canon = write(&mut r, "dependent", json!([
        {"op": "create", "type": "instruction", "id": "$i", "parent": doc,
         "fields": {"applies_to": ["all"]}, "body": "Also.", "links": {"decided_by": [{"target": decision}]}}
    ]))
    .unwrap()
    .created["$i"]
    .clone();
    land(&mut r, "dependent").unwrap();
    let in_view = r
        .decide(
            &writer(),
            "other",
            rdy_engine::DecisionInput {
                statement: "Rests on it.".into(),
                depends_on: vec![decision.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let in_second = write(&mut r, "third", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc, "fields": {"title": "Applied"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Applies it.",
         "links": {"implements": [{"target": decision}]}}
    ]))
    .unwrap()
    .created["$c"]
    .clone();
    let mut blocks = vec![canon.clone(), in_view.clone(), in_second.clone()];
    blocks.sort();

    for v in [edit(&id, "New words."), remove(&id)] {
        let before = state_of(dir.path(), &r);
        let e = prepare(&mut r, v).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e}");
        assert_eq!(
            e.message,
            format!("decision {decision} also governs other blocks")
        );
        assert_eq!(e.fix, SHARED_FIX);
        let corrected = e.corrected.expect("the blockers");
        assert_eq!(corrected["state"], json!("normal_path"));
        assert_eq!(corrected["scope"], json!("project"));
        assert_eq!(corrected["blocks"], json!(blocks));
        assert_eq!(state_of(dir.path(), &r), before, "nothing moved");
    }

    // a governing affects edge counts too
    let (d2, mut r2) = project();
    let seeded = write(&mut r2, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "n", "title": "N", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "c"}
    ]))
    .unwrap();
    let claim = seeded.created["$c"].clone();
    let dec = r2
        .decide(
            &writer(),
            "seed",
            rdy_engine::DecisionInput {
                statement: "Governs.".into(),
                affects: vec![claim.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let rule = write(&mut r2, "seed", json!([
        {"op": "create", "type": "instruction", "id": "$i", "parent": seeded.created["$d"],
         "fields": {"applies_to": ["all"]}, "body": "R.", "links": {"decided_by": [{"target": dec}]}}
    ]))
    .unwrap()
    .created["$i"]
    .clone();
    land(&mut r2, "seed").unwrap();
    let before = state_of(d2.path(), &r2);
    let e = prepare(&mut r2, remove(&rule)).unwrap_err();
    assert_eq!(
        e.message,
        format!("decision {dec} also governs other blocks")
    );
    assert_eq!(e.corrected.unwrap()["blocks"], json!([claim]));
    assert_eq!(state_of(d2.path(), &r2), before);
}

/// Lands a doc `name` with `lifecycle`, holding one rule by hand, and its reason at the root.
/// Returns the doc, the reason and the rule.
fn seed_rule(r: &mut Rdy, name: &str, lifecycle: &str) -> (String, String, String) {
    let dec = r
        .decide(
            &writer(),
            "seed",
            rdy_engine::DecisionInput {
                statement: format!("Reason in {name}."),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let ops = parse_test_ops(json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": name, "doc_type": "page", "lifecycle": lifecycle}},
        {"op": "create", "type": "instruction", "id": "$i", "parent": "$d",
         "fields": {"applies_to": ["all"]}, "body": format!("Rule in {name}."),
         "links": {"decided_by": [{"target": dec}]}}
    ]))
    .unwrap();
    // a stable rule needs its justification to land
    let w = r
        .write(
            &writer(),
            rdy_engine::WriteRequest {
                view: "seed".into(),
                ops,
                justification: Some(rdy_model::Justification::Implements(vec![
                    BlockId::parse(&dec).unwrap(),
                ])),
                message: "seed".into(),
            },
        )
        .unwrap();
    land(r, "seed").unwrap();
    (w.created["$d"].clone(), dec, w.created["$i"].clone())
}

fn assert_stable_stop(dir: &Path, r: &mut Rdy, v: Value, blocks: &[&str]) {
    let before = state_of(dir, r);
    let e = prepare(r, v).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    assert_eq!(e.message, "rdy_rule cannot change stable content");
    assert_eq!(e.fix, STABLE_FIX);
    let mut want: Vec<&str> = blocks.to_vec();
    want.sort_unstable();
    let corrected = e.corrected.expect("the blockers");
    assert_eq!(corrected["state"], json!("normal_path"));
    assert_eq!(corrected["blocks"], json!(want));
    assert_eq!(state_of(dir, r), before, "nothing moved");
}

#[test]
fn rtc2_stable_target_stops_before_any_write() {
    // the rule itself and its doc
    let (d, mut r) = project();
    let (doc, _, rule) = seed_rule(&mut r, "held", "stable");
    assert_stable_stop(d.path(), &mut r, edit(&rule, "x"), &[&doc, &rule]);
    assert_stable_stop(d.path(), &mut r, remove(&rule), &[&doc, &rule]);

    // a named ruleset, by add and as a move target
    let add_there = json!({"scope": "project", "op": "add", "text": "x", "why": "w", "evidence": "e", "ruleset": "held"});
    assert_stable_stop(d.path(), &mut r, add_there, &[&doc]);

    // both parents of a move
    let (d2, mut r2) = project();
    let (_, _, draft_rule) = seed_rule(&mut r2, "soft", "draft");
    let w = write(&mut r2, "more", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hard", "title": "hard", "doc_type": "page", "lifecycle": "stable"}}
    ]))
    .unwrap();
    land(&mut r2, "more").unwrap();
    let hard = w.created["$d"].clone();
    let to_stable = json!({"scope": "project", "op": "edit", "id": draft_rule, "text": "x", "why": "w", "evidence": "e", "ruleset": "hard"});
    assert_stable_stop(d2.path(), &mut r2, to_stable, &[&draft_rule, &hard]);
    let (d3, mut r3) = project();
    let (held, _, held_rule) = seed_rule(&mut r3, "held", "stable");
    let w = write(&mut r3, "more", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "soft", "title": "soft", "doc_type": "page", "lifecycle": "draft"}}
    ]))
    .unwrap();
    land(&mut r3, "more").unwrap();
    let _soft = w.created["$d"].clone();
    let to_draft = json!({"scope": "project", "op": "edit", "id": held_rule, "text": "x", "why": "w", "evidence": "e", "ruleset": "soft"});
    assert_stable_stop(d3.path(), &mut r3, to_draft, &[&held, &held_rule]);

    // the decision home
    let (d4, mut r4) = project();
    let w = write(&mut r4, "seed", json!([
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "log", "title": "Log", "doc_type": "page", "lifecycle": "stable"}}
    ]))
    .unwrap();
    land(&mut r4, "seed").unwrap();
    r4.close().unwrap();
    home_at(d4.path(), "log");
    let mut r4 = Rdy::open(d4.path()).unwrap();
    assert_stable_stop(d4.path(), &mut r4, add("x"), &[&w.created["$l"]]);

    // the old decision, under stable content, with the rule in a draft
    let (d5, mut r5) = project();
    let w = write(&mut r5, "seed", json!([
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "log", "title": "Log", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "decision", "id": "$o", "parent": "$l", "fields": {"statement": "Old.", "status": "proposed"}},
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "soft", "title": "soft", "doc_type": "page", "lifecycle": "draft"}},
        {"op": "create", "type": "instruction", "id": "$i", "parent": "$d",
         "fields": {"applies_to": ["all"]}, "body": "R.", "links": {"decided_by": [{"target": "$o"}]}}
    ]))
    .unwrap();
    let (old, rule) = (w.created["$o"].clone(), w.created["$i"].clone());
    land(&mut r5, "seed").unwrap();
    assert_stable_stop(d5.path(), &mut r5, edit(&rule, "x"), &[&old]);

    // a rule is appended: a stable sibling is never reordered, so it neither stops nor moves
    let (_d6, mut r6) = project();
    let w = write(&mut r6, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "soft", "title": "soft", "doc_type": "page", "lifecycle": "draft"}},
        {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "A", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "B", "lifecycle": "stable"}}
    ]))
    .unwrap();
    land(&mut r6, "seed").unwrap();
    let (a, b) = (w.created["$a"].clone(), w.created["$b"].clone());
    let (va, vb) = (
        version_of(&r6, &Scope::Canon, &a),
        version_of(&r6, &Scope::Canon, &b),
    );
    let p = prepare(
        &mut r6,
        json!({"scope": "project", "op": "add", "text": "Next to stable.", "why": "w", "evidence": "e", "ruleset": "soft"}),
    )
    .unwrap();
    let st = r6.read();
    let change = st.reader().change(&st.included(&p.view)[0]).unwrap();
    let written: Vec<String> = change
        .ops
        .iter()
        .map(|o| o.block_id().to_string())
        .collect();
    assert_eq!(written.len(), 2, "{written:?}");
    assert!(
        !written.contains(&a) && !written.contains(&b),
        "{written:?}"
    );
    drop(st);
    land_prepared(&mut r6, &p);
    assert_eq!(version_of(&r6, &Scope::Canon, &a), va);
    assert_eq!(version_of(&r6, &Scope::Canon, &b), vb);
}

#[test]
fn rtc2_complete_change_has_no_partial_decision_or_rule() {
    let (dir, mut r) = project();
    let seeded = added(&mut r, "Seed rule.");
    let cases = [
        // a first use in the meta destination writes the doc and its skeleton too
        (
            Destination::Meta,
            json!({"scope": "person", "op": "add", "text": "New.", "why": "w", "evidence": "e"}),
            7,
        ),
        (Destination::Project, add("Second."), 2),
        (Destination::Project, edit(&seeded.rule.id, "Edited."), 2),
        (Destination::Project, remove(&seeded.rule.id), 2),
    ];
    for (d, v, count) in cases {
        for i in 0..count {
            let before = state_of(dir.path(), &r);
            let e =
                with_rule_fault_for_tests(RuleFault::BadOp(i), || prepare_in(&mut r, d, v.clone()))
                    .unwrap_err();
            assert_ne!(e.code, E_INTERNAL, "op {i} of {v}: {e}");
            assert_eq!(state_of(dir.path(), &r), before, "op {i} of {v}");
        }
        // past the last op no fault fires: the whole change is written, in one change
        let p =
            with_rule_fault_for_tests(RuleFault::BadOp(count), || prepare_in(&mut r, d, v.clone()))
                .unwrap_or_else(|e| panic!("{v}: {e}"));
        let st = r.read();
        let included = st.included(&p.view);
        assert_eq!(included.len(), 1, "{v}");
        let ops = st.reader().change(&included[0]).unwrap().ops;
        assert_eq!(ops.len(), count, "{v}");
        let m = st.materialized(&Scope::View(p.view.clone())).unwrap();
        assert!(m.get(&BlockId::parse(&p.decision).unwrap()).is_some());
        if !p.rule.retired {
            assert_eq!(
                m.get(&BlockId::parse(&p.rule.id).unwrap()).unwrap().body,
                p.rule.text
            );
        }
        drop(m);
        drop(st);
        r.view_close(&me(), &[p.view.clone()], "next case").unwrap();
    }
}

#[test]
fn rtc2_failed_propose_closes_only_its_view() {
    let (dir, mut r) = project();
    // the caller's own work, and a sibling request that stays open
    let own = me().default_view();
    write_as(&mut r, &me(), &own, json!([
        {"op": "create", "type": "doc", "fields": {"name": "mine", "title": "Mine", "doc_type": "page"}}
    ]))
    .unwrap();
    let own_head = r.read().view_head(&own).unwrap().head;
    let sibling = prepare(&mut r, add("Sibling.")).unwrap();
    let sibling_head = r.read().view_head(&sibling.view).unwrap().head;
    let canon = r.read().canon_head();

    let e = with_rule_fault_for_tests(RuleFault::Propose, || prepare(&mut r, add("Failing.")))
        .unwrap_err();
    let injected = (e.code, e.message.clone(), e.fix.clone());
    assert!(e.corrected.is_none(), "{e}");

    // find it through the repeat: the view is closed, with the reason, and its change is kept
    let name = r
        .read()
        .view_names()
        .into_iter()
        .find(|v| v.starts_with("rule-") && *v != sibling.view)
        .expect("the failed request's view");
    assert_eq!(
        close_reason(&r, &name).as_deref(),
        Some("rules_from_chat: failed before proposal")
    );
    let st = r.read();
    let mut cur = Some(st.view_head(&name).unwrap().head);
    let mut commit = None;
    while let Some(h) = cur {
        let op = st.reader().view_op(&h).unwrap();
        if let ViewOpKind::Commit { change } = op.op {
            commit = Some(change);
        }
        cur = op.prev;
    }
    let kept = st.reader().change(&commit.expect("the commit")).unwrap();
    assert!(
        kept.ops
            .iter()
            .any(|o| matches!(o, Op::Create { block } if block.body == "Failing."))
    );
    drop(st);

    // everything else stands: the caller's view, the sibling, canon, and no open proposal here
    assert_eq!(r.read().view_head(&own).unwrap().head, own_head);
    assert_eq!(
        r.read().view_head(&sibling.view).unwrap().head,
        sibling_head
    );
    assert_eq!(r.read().canon_head(), canon);
    assert_eq!(r.read().open_proposals().unwrap().len(), 1);

    // the same request is closed work: no resurrection, and nothing lands
    let before = state_of(dir.path(), &r);
    let e = prepare(&mut r, add("Failing.")).unwrap_err();
    assert_eq!(e.code, E_VIEW_CLOSED, "{e}");
    assert_eq!(state_of(dir.path(), &r), before);
    // a new session reference starts new work
    let fresh = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "Failing.", "why": "They asked.",
               "evidence": "yes, keep it", "question": "Save this?", "session_ref": "chat-2"}),
    )
    .unwrap();
    assert_ne!(fresh.view, name);

    // a cleanup that cannot write keeps the original refusal and says what is pending
    let e = with_rule_fault_for_tests(RuleFault::ProposeAndClose, || {
        prepare(&mut r, add("Pending."))
    })
    .unwrap_err();
    assert_eq!((e.code, e.message.clone(), e.fix.clone()), injected);
    let corrected = e.corrected.expect("the pending cleanup");
    assert_eq!(corrected["cleanup"], json!("pending"));
    let view = corrected["view"].as_str().unwrap().to_string();
    assert!(!r.read().view_head(&view).unwrap().closed);
    // the surviving view holds the whole change, and a retry finishes it
    let again = prepare(&mut r, add("Pending.")).unwrap();
    assert_eq!(again.view, view);
    assert_eq!(r.read().included(&view).len(), 1);
    assert!(matches!(again.state, PreparedState::Proposed { .. }));
}

#[test]
fn rtc2_interrupted_view_is_reconciled_before_retry() {
    let (dir, mut r) = project();

    // stopped after the draft: the retry proposes the same change, and writes no other
    let e = with_rule_fault_for_tests(RuleFault::StopAfterDraft, || {
        prepare(&mut r, add("Interrupted."))
    })
    .unwrap_err();
    assert_eq!(e.code, E_INTERNAL, "{e}");
    let name = r
        .read()
        .view_names()
        .into_iter()
        .find(|v| v.starts_with("rule-"))
        .unwrap();
    assert!(!r.read().view_head(&name).unwrap().closed);
    let change = r.read().included(&name)[0];
    r.close().unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let p = prepare(&mut r, add("Interrupted.")).unwrap();
    assert_eq!(p.view, name);
    assert_eq!(*r.read().included(&name), vec![change]);
    assert!(matches!(p.state, PreparedState::Proposed { .. }));

    // stopped after the proposal: the retry returns that proposal
    r.close().unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let head = r.read().view_head(&name).unwrap().head;
    let again = prepare(&mut r, add("Interrupted.")).unwrap();
    assert_eq!(again.state, p.state);
    assert_eq!(again.rule, p.rule);
    assert_eq!(again.decision, p.decision);
    assert_eq!(r.read().view_head(&name).unwrap().head, head);

    // stopped after the landing, before the cleanup: the retry closes the view and returns it
    let landing = land_prepared(&mut r, &p);
    r.close().unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    assert!(!r.read().view_head(&name).unwrap().closed);
    let done = prepare(&mut r, add("Interrupted.")).unwrap();
    assert_eq!(
        done.state,
        PreparedState::Landed {
            landing: landing.clone()
        }
    );
    assert_eq!(done.rule, p.rule);
    assert_eq!(done.decision, p.decision);
    assert_eq!(
        close_reason(&r, &name).as_deref(),
        Some("rules_from_chat: landed")
    );

    // repeats after the cleanup write nothing, and the rule stands once
    let before = state_of(dir.path(), &r);
    let done = prepare(&mut r, add("Interrupted.")).unwrap();
    assert_eq!(
        done.state,
        PreparedState::Landed {
            landing: landing.clone()
        }
    );
    assert_eq!(state_of(dir.path(), &r), before);
    let rules = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .filter(|b| b.block_type == "instruction")
        .count();
    assert_eq!(rules, 1);

    // a cleanup that fails after the landing is a warning: the landing is still the result
    let second = prepare(&mut r, add("Second.")).unwrap();
    let landing2 = land_prepared(&mut r, &second);
    let warned =
        with_rule_fault_for_tests(RuleFault::Close, || prepare(&mut r, add("Second."))).unwrap();
    assert_eq!(warned.state, PreparedState::Landed { landing: landing2 });
    let w = warned
        .warnings
        .iter()
        .find(|w| w.message.contains(&second.view))
        .expect("the cleanup warning");
    assert!(w.fix.contains(&second.view), "{}", w.fix);
    assert!(!r.read().view_head(&second.view).unwrap().closed);

    // a remove keeps its prior text across the retry, though the rule is gone
    let id = done.rule.id.clone();
    let gone = prepare(&mut r, remove(&id)).unwrap();
    land_prepared(&mut r, &gone);
    let replay = prepare(&mut r, remove(&id)).unwrap();
    assert!(replay.rule.retired);
    assert_eq!(replay.rule.text, "Interrupted.");
    assert_eq!(replay.decision, gone.decision);
    assert!(matches!(replay.state, PreparedState::Landed { .. }));

    // a draft that a later dependent makes unsafe stops at the retry, and its view closes
    let third = prepare(&mut r, add("Third.")).unwrap();
    land_prepared(&mut r, &third);
    let v = edit(&third.rule.id, "Third, edited.");
    with_rule_fault_for_tests(RuleFault::StopAfterDraft, || prepare(&mut r, v.clone()))
        .unwrap_err();
    let doc = third.rule.ruleset.clone();
    let dependent = write(&mut r, "late", json!([
        {"op": "create", "type": "instruction", "id": "$i", "parent": doc,
         "fields": {"applies_to": ["all"]}, "body": "Late.", "links": {"decided_by": [{"target": third.decision}]}}
    ]))
    .unwrap()
    .created["$i"]
    .clone();
    let e = prepare(&mut r, v.clone()).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.corrected.unwrap()["blocks"], json!([dependent]));
    let closed = r
        .read()
        .view_names()
        .into_iter()
        .filter(|v| {
            close_reason(&r, v).as_deref() == Some("rules_from_chat: failed before proposal")
        })
        .count();
    assert_eq!(closed, 1);
    assert_eq!(block(&r, &Scope::Canon, &third.rule.id).body, "Third.");
}

#[test]
fn rtc2_stale_receipt_refuses_and_names_a_new_session_ref() {
    let (dir, mut r) = project();
    // add, remove, then the same add: the old landing must not read as this rule's state
    let first = added(&mut r, "Comes back.");
    let gone = prepare(&mut r, remove(&first.rule.id)).unwrap();
    land_prepared(&mut r, &gone);
    let before = state_of(dir.path(), &r);
    let e = prepare(&mut r, add("Comes back.")).unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
    assert_eq!(
        e.message,
        format!("rule {} changed after this request landed", first.rule.id)
    );
    assert_eq!(e.fix, "pass a new session_ref to make this request again");
    // the cleanup of the landed view is the one write the refusal may make
    assert_eq!(state_of(dir.path(), &r).0, before.0);
    let fresh = prepare(
        &mut r,
        json!({"scope": "project", "op": "add", "text": "Comes back.", "why": "They asked.",
               "evidence": "yes, keep it", "question": "Save this?", "session_ref": "chat-2"}),
    )
    .unwrap();
    assert_ne!(fresh.rule.id, first.rule.id);
    assert!(matches!(fresh.state, PreparedState::Proposed { .. }));

    // a later edit moves an add's receipt too
    let (_d2, mut r2) = project();
    let first = added(&mut r2, "Will move.");
    let moved = prepare(&mut r2, edit(&first.rule.id, "Moved.")).unwrap();
    land_prepared(&mut r2, &moved);
    let e = prepare(&mut r2, add("Will move.")).unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
}

/// A project whose pack's default doc type has `sections` skeleton sections.
fn long_skeleton(sections: usize) -> (tempfile::TempDir, Rdy) {
    let (d, r) = project();
    drop(r);
    let skeleton: Vec<String> = (0..sections)
        .map(|i| format!("{{ title: S{i}, required: false }}"))
        .collect();
    let pack = format!(
        "name: long\nversion: \"1\"\nextends: rdy-standard\ndefault_doc_type: long\ndoc_types:\n  long: {{ skeleton: [{}] }}\n",
        skeleton.join(", ")
    );
    rdy_qa::packs::use_pack(d.path(), "long", &pack, "");
    let r = Rdy::open(d.path()).unwrap();
    (d, r)
}

#[test]
fn rtc2_generated_ops_are_bounded_before_any_write() {
    // the doc, its sections, the decision and the rule: exactly 1024 ops pass
    let (_d, mut r) = long_skeleton(1021);
    let p = prepare(&mut r, add("At the bound.")).unwrap();
    let st = r.read();
    assert_eq!(
        st.reader()
            .change(&st.included(&p.view)[0])
            .unwrap()
            .ops
            .len(),
        1024
    );
    drop(st);

    let (d, mut r) = long_skeleton(1022);
    let before = state_of(d.path(), &r);
    let e = prepare(&mut r, add("Past the bound.")).unwrap_err();
    assert_eq!(e.code, rdy_engine::E_LIMIT, "{e}");
    assert_eq!(e.message, "rdy_rule: \"ops\" exceeds 1024 ops");
    assert_eq!(e.fix, "use at most 1024 ops for \"ops\"");
    assert_eq!(state_of(d.path(), &r), before);
}
