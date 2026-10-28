//! rg1 (D-VF4RH, D-ME2HD): a change records the briefing token it cited, as `briefing` on the
//! change's links. A write records the token only when the registry holds it and the briefing
//! check found it current (O4): a draft write with no token, or with an unknown or stale one,
//! records none. `view_resolve_landed` briefs the block it resolves and records that token (O3).
//! A replay copies its source change's token.
mod common;

use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::brief::{Briefing, Coverage, W_BRIEF_STALE};
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn agent(actor: &str) -> Session {
    Session::agent(actor.into(), Some("r1".into()), Role::Writer).unwrap()
}

fn ops(v: Value) -> Vec<OpInput> {
    parse_ops(serde_json::from_value(v).unwrap()).unwrap()
}

fn write_briefed(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    v: Value,
    brief: Option<&str>,
) -> Result<WriteResult, RdyError> {
    r.write_briefed(
        s,
        WriteRequest {
            view: view.into(),
            ops: ops(v),
            justification: None,
            message: format!("write to {view}"),
        },
        brief.map(str::to_string),
    )
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e}"));
}

/// A registered briefing of `ids` in `scope`.
fn brief(r: &Rdy, scope: &Scope, ids: &[&str]) -> Briefing {
    let b = r
        .read()
        .brief(
            scope,
            &Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap_or_else(|e| panic!("brief {ids:?}: {e}"));
    b.register(&r.briefs());
    b
}

fn briefing_of(r: &Rdy, change: &str) -> Option<Hash> {
    r.read()
        .reader()
        .change(&change.parse().unwrap())
        .unwrap()
        .links
        .briefing
}

/// A landed canon: a stable doc holding section `s` (claim `c` and question `q` in it), and a
/// draft doc holding claim `dc`.
struct P {
    _dir: tempfile::TempDir,
    r: Rdy,
    s: String,
    c: String,
    q: String,
    dc: String,
}

fn project() -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write_briefed(
        &mut r,
        &writer(),
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Refunds"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Refunds take five days."},
            {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {
                "prompt": "Who approves refunds?",
                "title": "Who approves refunds?",
                "why": "Support waits on this",
                "options": ["Finance", "Support lead"],
                "recommended": "Finance",
                "recommend_reason": "Finance owns the ledger"
            }},
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "claim", "id": "$dc", "parent": "$n", "body": "A draft note."}
        ]),
        None,
    )
    .unwrap();
    land(&mut r, "seed");
    let id = |k: &str| w.created[k].clone();
    P {
        s: id("$s"),
        c: id("$c"),
        q: id("$q"),
        dc: id("$dc"),
        _dir: dir,
        r,
    }
}

#[test]
fn rg1_a_briefed_stable_write_records_its_token() {
    let mut p = project();
    let edit = json!([{"op": "update", "id": p.c, "body": "Refunds take three days."}]);
    // the claim is stable content: a write with no briefing is refused
    let e = write_briefed(&mut p.r, &writer(), "work", edit.clone(), None).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    let b = brief(&p.r, &Scope::Canon, &[&p.c]);
    let w = write_briefed(&mut p.r, &writer(), "work", edit, Some(&b.base.token)).unwrap();
    assert_eq!(briefing_of(&p.r, &w.change), Some(b.record.token));
    assert_eq!(b.record.token.to_string(), b.base.token);
}

#[test]
fn rg1_a_draft_write_with_no_token_records_none() {
    let mut p = project();
    let w = write_briefed(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "update", "id": p.dc, "body": "Another draft note."}]),
        None,
    )
    .unwrap();
    assert!(w.warnings.iter().all(|i| i.code != W_BRIEF_STALE), "{w:?}");
    assert_eq!(briefing_of(&p.r, &w.change), None);
}

#[test]
fn rg1_a_draft_write_with_an_unknown_token_records_none() {
    let mut p = project();
    let unknown = "ab".repeat(32);
    assert!(p.r.briefs().get(&unknown).is_none());
    let w = write_briefed(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "update", "id": p.dc, "body": "Another draft note."}]),
        Some(&unknown),
    )
    .unwrap();
    assert!(w.warnings.iter().any(|i| i.code == W_BRIEF_STALE), "{w:?}");
    assert_eq!(briefing_of(&p.r, &w.change), None);
}

#[test]
fn rg1_a_draft_write_with_a_stale_token_records_none() {
    let mut p = project();
    p.r.view_create(&writer(), "work", None, false).unwrap();
    let b = brief(&p.r, &Scope::View("work".into()), &[&p.dc]);
    let first = write_briefed(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "update", "id": p.dc, "body": "Another draft note."}]),
        Some(&b.base.token),
    )
    .unwrap();
    assert_eq!(briefing_of(&p.r, &first.change), Some(b.record.token));
    // the registry still holds the token, but dc moved since the briefing: it is stale
    assert!(p.r.briefs().get(&b.base.token).is_some());
    let w = write_briefed(
        &mut p.r,
        &writer(),
        "work",
        json!([{"op": "update", "id": p.dc, "body": "A third draft note."}]),
        Some(&b.base.token),
    )
    .unwrap();
    assert!(w.warnings.iter().any(|i| i.code == W_BRIEF_STALE), "{w:?}");
    assert_eq!(briefing_of(&p.r, &w.change), None);
}

#[test]
fn rg1_a_decision_and_a_question_answer_record_the_token() {
    let mut p = project();
    // a decision under the stable section, briefed
    let b = brief(&p.r, &Scope::Canon, &[&p.s]);
    let w =
        p.r.decide(
            &writer(),
            "dv",
            DecisionInput {
                parent: p.s.clone(),
                statement: "Finance approves refunds".into(),
                brief: Some(b.base.token.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(briefing_of(&p.r, &w.change), Some(b.record.token));
    // the person's answer to the question, under the same stable section, briefed
    let s = writer();
    let q =
        p.r.read()
            .pending(&PendingQuery {
                person: s.actor.clone(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap();
    let item = q
        .items
        .iter()
        .find(|i| i.kind == ItemKind::Answer && i.r#ref.starts_with(&format!("{}@", p.q)))
        .unwrap_or_else(|| panic!("the question's item in {:?}", q.items))
        .r#ref
        .clone();
    let b = brief(&p.r, &Scope::Canon, &[&p.s]);
    p.r.answer(
        &s,
        &s.actor,
        AnswerRequest {
            r#ref: item.clone(),
            option: "o1".into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: Some(b.base.token.clone()),
        },
    )
    .unwrap();
    let (_, a) = p.r.store().reader().answer_of(&item).unwrap().unwrap();
    assert_eq!(a.act, AnswerAct::AnswerQuestion);
    assert_eq!(
        briefing_of(&p.r, &a.targets[0].to_string()),
        Some(b.record.token)
    );
}

/// A landed stable decisions-log doc; returns its section's id.
fn stable_log(r: &mut Rdy) -> String {
    let log = write_briefed(
        r,
        &writer(),
        "logs",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
        ]),
        None,
    )
    .unwrap();
    land(r, "logs");
    log.created["$s"].clone()
}

/// Agent `a` creates proposed decision D under section `s` in view `a`, briefed; returns D
/// and the change that created it.
fn create_decision(r: &mut Rdy, a: &Session, s: &str) -> (String, String) {
    let b = brief(r, &Scope::Canon, &[s]);
    let w = write_briefed(
        r,
        a,
        "a",
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": s,
            "fields": {"statement": "D", "status": "proposed"}}]),
        Some(&b.base.token),
    )
    .unwrap();
    (w.created["$x"].clone(), w.change)
}

#[test]
fn rg1_a_resolve_to_the_landed_side_records_a_token() {
    let mut p = project();
    let r = &mut p.r;
    let (a, b) = (agent("agent:a"), agent("agent:b"));
    let s = stable_log(r);
    let (d, _) = create_decision(r, &a, &s);
    let id = BlockId::parse(&d).unwrap();
    let vd = r
        .read()
        .block(&Scope::View("a".into()), &id)
        .unwrap()
        .version_hash()
        .to_string();
    r.view_fork(&b, "a", "b").unwrap();
    let scope = Scope::View("b".into());
    // D is stable content in view b: its update is refused unbriefed, and briefed there
    let update = json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by b"}}]);
    let e = write_briefed(r, &b, "b", update.clone(), None).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    let bb = brief(r, &scope, &[&d]);
    write_briefed(r, &b, "b", update, Some(&bb.base.token)).unwrap();
    land(r, "a");
    assert!(r.read().conflict_info(&scope, &id).unwrap().is_some());
    let res = r.view_resolve_landed(&b, "b", &d).unwrap();
    assert!(briefing_of(r, &res.change).is_some(), "{res:?}");
}

#[test]
fn rg1_a_resolve_after_a_replay_of_a_landed_create_records_a_token() {
    let mut p = project();
    let r = &mut p.r;
    let (a, b) = (agent("agent:a"), agent("agent:b"));
    let s = stable_log(r);
    let (d, created) = create_decision(r, &a, &s);
    let id = BlockId::parse(&d).unwrap();
    land(r, "a");
    // view b starts from canon, which already holds D; replaying a's create conflicts on D
    // with no base, so view b holds no version of D
    r.view_create(&b, "b", None, false).unwrap();
    r.view_replay(&b, "b", std::slice::from_ref(&created))
        .unwrap();
    let scope = Scope::View("b".into());
    let c = r.read().conflict_info(&scope, &id).unwrap();
    assert!(c.is_some(), "a conflict on {d}");
    let res = r.view_resolve_landed(&b, "b", &d).unwrap();
    assert!(r.read().conflict_info(&scope, &id).unwrap().is_none());
    // view b reads canon's version of D
    let canon = r.read().block(&Scope::Canon, &id).unwrap().version_hash();
    let view = r.read().block(&scope, &id).unwrap().version_hash();
    assert_eq!((view, res.version.clone()), (canon, canon.to_string()));
    assert!(briefing_of(r, &res.change).is_some(), "{res:?}");
}

#[test]
fn rg1_a_replay_keeps_its_source_token() {
    let mut p = project();
    let b = brief(&p.r, &Scope::Canon, &[&p.c]);
    let w = write_briefed(
        &mut p.r,
        &writer(),
        "src",
        json!([{"op": "update", "id": p.c, "body": "Refunds take three days."}]),
        Some(&b.base.token),
    )
    .unwrap();
    p.r.view_create(&writer(), "t", None, false).unwrap();
    let res =
        p.r.view_replay(&writer(), "t", std::slice::from_ref(&w.change))
            .unwrap();
    assert_eq!(res.replayed.len(), 1, "{res:?}");
    let replayed = &res.replayed[0].change;
    assert_eq!(briefing_of(&p.r, replayed), briefing_of(&p.r, &w.change));
    // since rg1 the source records its briefing, so the copy is the token itself
    assert_eq!(briefing_of(&p.r, replayed), Some(b.record.token));
    let c =
        p.r.read()
            .reader()
            .change(&replayed.parse().unwrap())
            .unwrap();
    assert_eq!(c.links.replayed_from, Some(w.change.parse().unwrap()));
}
