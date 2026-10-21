//! sdc5: follow-ups from changed literals (D-CFC6V, D-SES4J), the complete edge set of a cause
//! (D-Q69VB), the retained links of a fix write and the write's comparison base (D-EGXAS).
use rdy_engine::brief::Coverage;
use rdy_engine::consequence::{
    KEPT_CHANGED_FIX, KEPT_REPLACED_FIX, LINEAGE_FIX, REAFFIRM_FIX, consequence_id,
    land_and_report, propose_and_report, write_and_report,
};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::*;
use rdy_model::{BlockId, Hash, Justification};
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::{confirm_before_landing, summary_for};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const SUP_CONFIRM: &str = "this block states no removed value. Check that it still holds, then point each listed link at the decision in force.";
const REV_CONFIRM: &str = "this block states no value of the decision. Check that it still holds, then remove each listed link or point it elsewhere.";
const SUP_UPDATE: &str =
    "update the block for the removed values. Point each listed link at the decision in force.";
const REV_UPDATE: &str =
    "update the block for the removed values. Remove each listed link or point it elsewhere.";
const SUP_CUT: &str = "rdy checked only the first 16384 bytes. Check the rest, then point each listed link at the decision in force.";
const REV_CUT: &str = "rdy checked only the first 16384 bytes. Check the rest, then remove each listed link or point it elsewhere.";
const EDIT_CUT: &str =
    "rdy checked only the first 16384 bytes. Check the rest of the block and update it if needed.";

const D30_TITLE: &str = "Keep raw error events for 30 days";
const D30_STATEMENT: &str = "Tally keeps each raw error event for 30 days after it arrives. Then the purge job deletes it. Issues stay after their events go.";
const D7_TITLE: &str = "Keep raw error events for 7 days";
const D7_STATEMENT: &str = "Tally keeps each raw error event for 7 days after it arrives. Then the purge job deletes it. Issues stay after their events go.";

// ---------------------------------------------------------------- helpers

fn created(v: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&v.created[name]).unwrap_or_else(|_| panic!("no {name} in {v:?}"))
}

fn created_in(v: &Value, name: &str) -> BlockId {
    BlockId::parse(
        v["created"][name]
            .as_str()
            .unwrap_or_else(|| panic!("no {name}: {v}")),
    )
    .unwrap()
}

fn view(name: &str) -> Scope {
    Scope::View(name.into())
}

/// Briefs `ids` in `scope` through `e`, registered as the tools register it; as JSON.
fn brief_in(e: &dyn Engine, scope: &Scope, ids: &[&BlockId], max: Option<usize>) -> Value {
    let cov = Coverage {
        ids: ids.iter().map(|i| i.to_string()).collect(),
        path: None,
    };
    let b = e.read().brief(scope, &cov, max).expect("brief");
    b.register(&e.briefs());
    serde_json::to_value(&b).unwrap()
}

fn token(b: &Value) -> String {
    b["base"]["token"].as_str().unwrap().to_string()
}

/// A write as the human writer in `view` through `e`, with its whole reply.
fn write_in(
    e: &Local,
    view: &str,
    ops: Value,
    implements: &[&BlockId],
    brief: Option<String>,
) -> Value {
    let ops = parse_test_ops(ops).unwrap();
    let justification = (!implements.is_empty())
        .then(|| Justification::Implements(implements.iter().map(|b| (*b).clone()).collect()));
    let v = view.to_string();
    write_and_report(e, view, None, move |r| {
        r.write_briefed(
            &writer(),
            WriteRequest {
                view: v,
                ops,
                justification,
                message: String::new(),
            },
            brief,
        )
    })
    .unwrap()
}

/// A supersede of `old` in `view` through `e`, with its whole reply.
fn supersede_in(
    e: &Local,
    view: &str,
    old: &BlockId,
    part: Option<&str>,
    d: DecisionInput,
) -> Value {
    let (v, old, part) = (view.to_string(), old.to_string(), part.map(str::to_string));
    write_and_report(e, view, None, move |r| {
        r.supersede(&writer(), &v, &old, part, d)
    })
    .unwrap()
}

fn revoke_in(
    e: &Local,
    view: &str,
    target: &BlockId,
    parent: &BlockId,
    brief: Option<String>,
) -> Value {
    let (v, t, p) = (view.to_string(), target.to_string(), parent.to_string());
    write_and_report(e, view, None, move |r| {
        r.revoke(&writer(), &v, &t, "not needed", &p, brief)
    })
    .unwrap()
}

fn items(v: &Value) -> &Vec<Value> {
    v["consequences"]
        .as_array()
        .unwrap_or_else(|| panic!("no consequences: {v:#}"))
}

/// Every entry for `block`.
fn entries<'a>(v: &'a Value, block: &BlockId) -> Vec<&'a Value> {
    items(v)
        .iter()
        .filter(|c| c["block"] == block.to_string())
        .collect()
}

/// The one entry for `block`.
fn entry<'a>(v: &'a Value, block: &BlockId) -> &'a Value {
    match entries(v, block).as_slice() {
        [one] => one,
        other => panic!("{} entries for {block}: {v:#}", other.len()),
    }
}

/// Whether `v` has an entry for `block` caused by `target`.
fn names(v: &Value, block: &BlockId, target: &BlockId) -> bool {
    items(v)
        .iter()
        .any(|c| c["block"] == block.to_string() && c["because"]["changed"] == target.to_string())
}

fn change_of(v: &Value) -> Hash {
    v["change"].as_str().unwrap().parse().unwrap()
}

/// `v`'s consequences without their ids (another batch gives the same entries other ids).
fn without_ids(v: &Value) -> Vec<Value> {
    items(v)
        .iter()
        .map(|c| {
            let mut c = c.clone();
            c.as_object_mut().unwrap().remove("id");
            c
        })
        .collect()
}

/// Proposes `view` through `e` (with a summary when it writes a decision), then lands it after
/// the exceptions are confirmed; the propose reply and the approval reply.
fn propose_and_land(e: &Local, view: &str) -> (Value, Value) {
    let slot: Arc<Mutex<Option<ProposalInfo>>> = Arc::default();
    let inner = Arc::clone(&slot);
    let v = view.to_string();
    let p = propose_and_report(e, None, move |r| {
        let summary = summary_for(r, &v);
        let p = r.propose(&writer(), &v, &v, summary)?;
        *inner.lock().unwrap() = Some(p.clone());
        Ok(p)
    })
    .unwrap();
    let info = slot.lock().unwrap().take().unwrap();
    let l = land_and_report(e, move |r| {
        confirm_before_landing(r, &approver(), &info);
        r.land_report(&approver(), &info.id)
    })
    .unwrap();
    (p, l)
}

// ---------------------------------------------------------------- the Tally sample

/// The Tally sample's blocks (D-Q69VB), before the supersede: five stable docs, the glossary's
/// three terms, the overview's two components, the retention decision D30 (accepted), and the
/// requirement and the flow that implement it.
#[derive(Clone, Debug)]
struct Tally {
    s_dec: BlockId,
    t_event: BlockId,
    d30: BlockId,
    r_size: BlockId,
    f_purge: BlockId,
    /// The informative prose that repeats the flow's second step (`tally_with(true)` only).
    prose: Option<BlockId>,
}

fn steps(d: &BlockId, days: &str) -> Value {
    json!([
        "The purge job starts every night at 02:00 UTC.",
        format!(
            "It deletes each raw error event that arrived more than {days} ago, as {{{{{d}|the retention decision}}}} sets."
        ),
        "It keeps every issue, also when it deletes all the events in that issue."
    ])
}

fn r_size_body(d: &BlockId) -> String {
    format!(
        "The event store must hold every raw error event for the retention period that {{{{{d}|the retention decision}}}} sets, with 20% free space above that amount."
    )
}

fn tally() -> (tempfile::TempDir, Rdy, Tally) {
    tally_with(false)
}

/// The sample through the write and decide paths; with `prose`, an informative prose block
/// under the purge runbook that states the flow's second step and is decided by D30.
fn tally_with(prose: bool) -> (tempfile::TempDir, Rdy, Tally) {
    let (dir, mut r) = project();
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d_glossary", "fields": {"name": "glossary", "title": "Glossary", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "doc", "id": "$d_overview", "fields": {"name": "overview", "title": "Overview", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "doc", "id": "$d_dec", "fields": {"name": "decisions", "title": "Decisions", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "doc", "id": "$d_store", "fields": {"name": "storage", "title": "Storage", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "doc", "id": "$d_purge", "fields": {"name": "purge-runbook", "title": "Purge runbook", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$s_dec", "parent": "$d_dec", "fields": {"title": "Retention"}},
        {"op": "create", "type": "term", "id": "$t_event", "parent": "$d_glossary",
         "fields": {"name": "Raw error event", "aliases": ["error event", "raw event"]},
         "body": "One error report as the app sent it: the error message, the stack trace, the app version and the device details. It can hold personal data, such as a user ID or an email address."},
        {"op": "create", "type": "term", "id": "$t_issue", "parent": "$d_glossary", "fields": {"name": "Issue"},
         "body": "A group of raw error events that share one cause. Tally keeps an issue after it deletes the raw error events in it."},
        {"op": "create", "type": "term", "id": "$t_ret", "parent": "$d_glossary", "fields": {"name": "Retention period"},
         "body": "The time that Tally keeps a raw error event after the event arrives. When the time ends, the purge job deletes the event."},
        {"op": "create", "type": "component", "id": "$m_store", "parent": "$d_overview", "fields": {"name": "Event store"},
         "body": "The database that holds the raw error events and the issues. The {{$t_ret|retention period}} sets how much it holds."},
        {"op": "create", "type": "component", "id": "$m_purge", "parent": "$d_overview", "fields": {"name": "Purge job"},
         "body": "A nightly job that deletes each raw error event whose {{$t_ret|retention period}} has ended. The {{$d_purge|purge job runbook}} says how to run it."}
    ]))
    .expect("the Tally docs");
    land(&mut r, "base").expect("land the docs");
    let id = |n: &str| created(&w, n);
    let (store, purge) = (id("$d_store"), id("$d_purge"));
    let s_dec = id("$s_dec");
    let (t_event, t_issue, t_ret) = (id("$t_event"), id("$t_issue"), id("$t_ret"));
    let (m_store, m_purge) = (id("$m_store"), id("$m_purge"));
    let t = brief_of(&r, &Scope::Canon, &[&s_dec]).base.token;
    let d = r
        .decide(
            &writer(),
            "base2",
            DecisionInput {
                parent: s_dec.to_string(),
                title: Some(D30_TITLE.into()),
                statement: D30_STATEMENT.into(),
                why: Some("Most teams release once a month. With 30 days, a team can see when an error started in the current release. The storage cost stays known.".into()),
                options: vec![
                    "Keep raw error events for 7 days".into(),
                    "Keep raw error events for 30 days".into(),
                    "Keep raw error events for 90 days".into(),
                ],
                body: "Rejected: 7 days. A team that releases once a month loses the events from the start of the release. 90 days. It triples the storage cost, and it keeps personal data longer than a team needs it.".into(),
                kind: Some("product".into()),
                reversibility: Some("two_way".into()),
                affects: vec![t_event.to_string(), m_purge.to_string(), m_store.to_string()],
                brief: Some(t),
                ..Default::default()
            },
        )
        .expect("decide D30");
    let d30 = created(&d, "$decision");
    let t = brief_of(&r, &view("base2"), &[&store, &purge]).base.token;
    let mut ops = vec![
        json!({"op": "create", "type": "requirement", "id": "$r_size", "parent": store.to_string(),
         "fields": {"priority": "must"}, "body": r_size_body(&d30),
         "links": {"decided_by": [{"target": d30.to_string()}],
                   "describes": [{"target": m_store.to_string()}, {"target": t_event.to_string()}, {"target": t_ret.to_string()}]}}),
        json!({"op": "create", "type": "flow", "id": "$f_purge", "parent": purge.to_string(),
         "fields": {"name": "Nightly purge", "steps": steps(&d30, "30 days")},
         "links": {"decided_by": [{"target": d30.to_string()}],
                   "describes": [{"target": m_purge.to_string()}, {"target": t_event.to_string()}, {"target": t_issue.to_string()}]}}),
    ];
    if prose {
        ops.push(
            json!({"op": "create", "type": "prose", "id": "$prose", "parent": purge.to_string(),
            "body": steps(&d30, "30 days")[1].as_str().unwrap(),
            "links": {"decided_by": [{"target": d30.to_string()}]}}),
        );
    }
    let w = r
        .write_briefed(
            &writer(),
            WriteRequest {
                view: "base2".into(),
                ops: parse_test_ops(json!(ops)).unwrap(),
                justification: Some(Justification::Implements(vec![d30.clone()])),
                message: String::new(),
            },
            Some(t),
        )
        .expect("the requirement and the flow");
    let (r_size, f_purge) = (created(&w, "$r_size"), created(&w, "$f_purge"));
    let prose = prose.then(|| created(&w, "$prose"));
    land(&mut r, "base2").expect("accept and land D30 with its content");
    (
        dir,
        r,
        Tally {
            s_dec,
            t_event,
            d30,
            r_size,
            f_purge,
            prose,
        },
    )
}

fn d7(t: &Tally, brief: Option<String>) -> DecisionInput {
    DecisionInput {
        parent: t.s_dec.to_string(),
        title: Some(D7_TITLE.into()),
        statement: D7_STATEMENT.into(),
        brief,
        ..Default::default()
    }
}

/// Steps 4 and 5 in view `w`: the D30 briefing, then the supersede that cites it; the
/// briefing, the supersede reply and D7.
fn superseded(e: &Local, t: &Tally) -> (Value, Value, BlockId) {
    let b = brief_in(e, &view("w"), &[&t.d30, &t.s_dec], None);
    let v = supersede_in(e, "w", &t.d30, None, d7(t, Some(token(&b))));
    let d7 = created_in(&v, "$decision");
    (b, v, d7)
}

// ---------------------------------------------------------------- tests

#[test]
fn sdc5_tally_requirement_confirms_and_runbook_updates() {
    let (_d, r, t) = tally();
    let lex = r.read().lexicon(&Scope::Canon).unwrap();
    let e = Local::new(r);
    // step 4: both content blocks depend on D30 and will go stale with update
    let (b, v, d7) = superseded(&e, &t);
    for block in [&t.r_size, &t.f_purge] {
        assert!(
            b["dependents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["block"] == block.to_string() && d["on"] == t.d30.to_string()),
            "{block} is a dependent: {b:#}"
        );
        let rows: Vec<&Value> = b["will_go_stale"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["block"] == block.to_string())
            .collect();
        assert!(!rows.is_empty(), "{block} will go stale: {b:#}");
        for w in rows {
            assert_eq!(w["follow_up"], "update", "{w}");
            assert!(!w["states"].as_array().expect("states").is_empty(), "{w}");
        }
    }
    // the freeze rule: both decisions name the same entities in their title and statement
    let entities = |id: &BlockId| -> std::collections::BTreeSet<String> {
        let b = e.read().block(&view("w"), id).unwrap();
        lex.mentions(&b)
            .into_iter()
            .filter(|m| m.field == "title" || m.field == "statement")
            .map(|m| m.entity)
            .collect()
    };
    assert!(!entities(&t.d30).is_empty());
    assert_eq!(entities(&t.d30), entities(&d7));
    // step 5: exactly the two content blocks, both dependents of D30
    assert_eq!(items(&v).len(), 2, "{v:#}");
    for block in [&t.r_size, &t.f_purge] {
        let c = entry(&v, block);
        assert_eq!(c["kind"], "dependent", "{c}");
        assert_eq!(c["because"]["changed"], t.d30.to_string(), "{c}");
        assert_eq!(c["because"]["values"], json!(["30 days"]), "{c}");
        assert!(c["because"].get("values_omitted").is_none(), "{c}");
        assert_eq!(
            c["because"]["edges"],
            json!(["decided_by", "implements"]),
            "the reference to D30 stays out of the set (O13): {c}"
        );
    }
    let req = entry(&v, &t.r_size);
    assert_eq!(req["follow_up"], "confirm", "{req}");
    assert!(req.get("matched").is_none() && req.get("matched_omitted").is_none());
    assert_eq!(req["fix"], SUP_CONFIRM);
    let flow = entry(&v, &t.f_purge);
    assert_eq!(flow["follow_up"], "update", "{flow}");
    let line = steps(&t.d30, "30 days")[1].as_str().unwrap().to_string();
    assert_eq!(
        flow["matched"],
        json!([{"value": "30 days", "field": "steps[1]", "line": 1, "span": [55, 62], "text": line}]),
        "{flow}"
    );
    assert!(flow.get("matched_omitted").is_none(), "{flow}");
    assert_eq!(flow["fix"], SUP_UPDATE);
    // step 6: the fix write moves both links of each block to D7
    let b = brief_in(&e, &view("w"), &[&t.f_purge, &t.r_size], None);
    let fix = write_in(
        &e,
        "w",
        json!([
            {"op": "update", "id": t.f_purge.to_string(), "fields": {"steps": steps(&d7, "7 days")},
             "links": {"decided_by": [{"target": d7.to_string()}], "implements": [{"target": d7.to_string()}]}},
            {"op": "update", "id": t.r_size.to_string(), "body": r_size_body(&d7),
             "links": {"decided_by": [{"target": d7.to_string()}], "implements": [{"target": d7.to_string()}]}}
        ]),
        &[&d7],
        Some(token(&b)),
    );
    for block in [&t.r_size, &t.f_purge] {
        assert!(!names(&fix, block, &t.d30), "no retained link: {fix:#}");
    }
    // step 7 (O12): no content block is left to fix
    let check = |e: &Local, scope: &Scope, what: &str| {
        let b = brief_in(e, scope, &[&t.d30], None);
        assert_eq!(b["will_go_stale"], json!([]), "{what}: {b:#}");
        assert_eq!(b["suspects"], json!([]), "{what}: {b:#}");
        for block in [&t.r_size, &t.f_purge] {
            assert!(
                !b["dependents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["block"] == block.to_string()),
                "{what}: {b:#}"
            );
        }
        assert!(
            b["lineage"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l["block"] == d7.to_string()
                    && l["kind"] == "supersedes"
                    && l["direction"] == "in"),
            "{what}: the successor's lineage stays: {b:#}"
        );
    };
    check(&e, &view("w"), "the view");
    let (p, l) = propose_and_land(&e, "w");
    for v in [&p, &l] {
        for block in [&t.r_size, &t.f_purge] {
            assert!(!names(v, block, &t.d30), "{v:#}");
        }
    }
    check(&e, &Scope::Canon, "canon");
}

#[test]
fn sdc5_informative_prose_with_the_removed_literal_updates() {
    let (_d, r, t) = tally_with(true);
    let e = Local::new(r);
    let (_, v, _) = superseded(&e, &t);
    let prose = t.prose.clone().unwrap();
    let c = entry(&v, &prose);
    assert_eq!(
        c["follow_up"], "update",
        "an informative type updates too: {c}"
    );
    let rows = c["matched"].as_array().expect("matched");
    assert_eq!(rows.len(), 1, "{c}");
    assert_eq!(rows[0]["value"], "30 days");
    assert_eq!(rows[0]["field"], "body");
    assert_eq!(rows[0]["line"], 1);
    assert_eq!(rows[0]["span"], json!([55, 62]));
}

/// The reply of a fix write that moves decided_by and the reference to D7 and keeps
/// implements on D30 (it names its decision, so the stable write opens no tension: the
/// justification adds D7 beside D30).
fn partial_fix(e: &Local, t: &Tally, d7: &BlockId) -> Value {
    let b = brief_in(e, &view("w"), &[&t.r_size], None);
    write_in(
        e,
        "w",
        json!([{"op": "update", "id": t.r_size.to_string(), "body": r_size_body(d7),
                "links": {"decided_by": [{"target": d7.to_string()}]}}]),
        &[d7],
        Some(token(&b)),
    )
}

#[test]
fn sdc5_fix_write_reports_the_remaining_implements_link() {
    let (_d, r, t) = tally();
    let e = Local::new(r);
    let (_, _, d7) = superseded(&e, &t);
    let v = partial_fix(&e, &t, &d7);
    let c = entry(&v, &t.r_size);
    assert_eq!(c["kind"], "dependent", "{c}");
    assert_eq!(c["already_written"], true, "{c}");
    assert_eq!(c["because"]["edge"], "implements", "{c}");
    assert_eq!(c["because"]["changed"], t.d30.to_string(), "{c}");
    assert_eq!(c["because"]["edges"], json!(["implements"]), "{c}");
    assert_eq!(c["because"]["values"], json!(["30 days"]), "{c}");
    assert_eq!(c["follow_up"], "confirm", "{c}");
    assert_eq!(c["fix"], SUP_CONFIRM);
    assert_eq!(
        c["id"],
        consequence_id(&change_of(&v), &format!("dependent {}", t.d30), &t.r_size)
    );
    // O11: on_change stays the tension delta, with no dependency list
    let on = v["on_change"].as_object().expect("on_change");
    assert!(
        on.keys()
            .all(|k| ["tensions", "resolved", "detail_cut", "collapsed"].contains(&k.as_str())),
        "{v:#}"
    );
    assert!(
        !v["on_change"].to_string().contains(&t.r_size.to_string()),
        "{v:#}"
    );
    // the second fix moves implements too: the entry is gone
    let b = brief_in(&e, &view("w"), &[&t.r_size, &t.f_purge], None);
    let v = write_in(
        &e,
        "w",
        json!([
            {"op": "update", "id": t.r_size.to_string(), "links": {"implements": [{"target": d7.to_string()}]}},
            {"op": "update", "id": t.f_purge.to_string(), "fields": {"steps": steps(&d7, "7 days")},
             "links": {"decided_by": [{"target": d7.to_string()}], "implements": [{"target": d7.to_string()}]}}
        ]),
        &[&d7],
        Some(token(&b)),
    );
    assert!(!names(&v, &t.r_size, &t.d30), "{v:#}");
    let (p, l) = propose_and_land(&e, "w");
    for v in [&p, &l] {
        assert!(!names(v, &t.r_size, &t.d30), "{v:#}");
        assert!(!names(v, &t.f_purge, &t.d30), "{v:#}");
    }
}

#[test]
fn sdc5_each_written_content_block_reports_its_implements_link() {
    let (_d, r, t) = tally();
    let e = Local::new(r);
    let (_, _, d7) = superseded(&e, &t);
    // one write moves decided_by of both; the justification adds D7 beside D30 in implements
    let b = brief_in(&e, &view("w"), &[&t.r_size, &t.f_purge], None);
    let v = write_in(
        &e,
        "w",
        json!([
            {"op": "update", "id": t.r_size.to_string(), "body": r_size_body(&d7),
             "links": {"decided_by": [{"target": d7.to_string()}]}},
            {"op": "update", "id": t.f_purge.to_string(), "fields": {"steps": steps(&d7, "7 days")},
             "links": {"decided_by": [{"target": d7.to_string()}]}}
        ]),
        &[&d7],
        Some(token(&b)),
    );
    for block in [&t.r_size, &t.f_purge] {
        let c = entry(&v, block);
        assert_eq!(c["because"]["edges"], json!(["implements"]), "{c}");
        assert_eq!(c["already_written"], true, "{c}");
    }
    // fixing one does not hide the other's retained link
    let b = brief_in(&e, &view("w"), &[&t.r_size, &t.f_purge], None);
    let v = write_in(
        &e,
        "w",
        json!([
            {"op": "update", "id": t.r_size.to_string(), "links": {"implements": [{"target": d7.to_string()}]}},
            {"op": "update", "id": t.f_purge.to_string(), "fields": {"name": "Nightly purge job"}}
        ]),
        &[],
        Some(token(&b)),
    );
    assert!(!names(&v, &t.r_size, &t.d30), "{v:#}");
    let c = entry(&v, &t.f_purge);
    assert_eq!(c["because"]["changed"], t.d30.to_string(), "{c}");
    assert_eq!(c["because"]["edges"], json!(["implements"]), "{c}");
}

/// The spec doc and, in canon, claim `$x` decided by a decision whose statement is
/// `statement`, with `body`; the decision and the claim.
fn decided_claim(r: &mut Rdy, s: &Spec, statement: &str, body: &str) -> (BlockId, BlockId) {
    let d = r
        .decide(
            &writer(),
            "d",
            DecisionInput {
                parent: s.section.to_string(),
                statement: statement.into(),
                ..Default::default()
            },
        )
        .unwrap();
    let d = created(&d, "$decision");
    let x = write(
        r,
        "d",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.section.to_string(),
                "body": body, "links": {"decided_by": [{"target": d.to_string()}]}}]),
    )
    .unwrap();
    let x = created(&x, "$x");
    land(r, "d").unwrap();
    (d, x)
}

#[test]
fn sdc5_supersede_and_revoke_confirm_have_actionable_fixes() {
    let long = format!(
        "{} The claim ends here.",
        "Filler words go on. ".repeat(900)
    );
    assert!(long.len() > 16_384);
    let cases: [(&str, &str, &str, &str); 3] = [
        ("Keep the logs", "The logs stay.", SUP_CONFIRM, REV_CONFIRM),
        (
            "Keep the logs for 30 days",
            "The logs stay for 30 days.",
            SUP_UPDATE,
            REV_UPDATE,
        ),
        ("Keep the logs", &long, SUP_CUT, REV_CUT),
    ];
    for (statement, body, sup, rev) in cases {
        for revoke in [false, true] {
            let (_d, mut r) = project();
            let s = spec_doc(&mut r);
            let (d, x) = decided_claim(&mut r, &s, statement, body);
            let e = Local::new(r);
            let v = if revoke {
                revoke_in(&e, "w", &d, &s.section, None)
            } else {
                supersede_in(
                    &e,
                    "w",
                    &d,
                    None,
                    DecisionInput {
                        parent: s.section.to_string(),
                        statement: "Keep the logs for 7 days".into(),
                        ..Default::default()
                    },
                )
            };
            let c = entry(&v, &x);
            let want = if sup == SUP_CONFIRM {
                "confirm"
            } else {
                "update"
            };
            assert_eq!(c["follow_up"], want, "{statement} {revoke}: {c}");
            assert_eq!(c["fix"], if revoke { rev } else { sup }, "{c}");
            assert_eq!(c["because"]["edges"], json!(["decided_by"]), "{c}");
            if sup == SUP_UPDATE {
                assert_eq!(c["because"]["values"], json!(["30 days"]), "{c}");
                assert_eq!(c["matched"][0]["value"], "30 days", "{c}");
            } else {
                assert!(c.get("matched").is_none(), "no invented match row: {c}");
            }
            if revoke || sup != SUP_CONFIRM {
                continue;
            }
            // a repin alone does not clear the retired-target link: rdy refuses it, and a
            // rewrite that keeps the link reports it again
            let xs = x.to_string();
            let rep = write_and_report(&e, "w", None, move |r| {
                write(r, "w", json!([{"op": "repin", "id": xs}]))
            })
            .unwrap_err();
            assert_eq!(rep.code, "E_BAD_INPUT", "{rep}");
            let kept = write_in(
                &e,
                "w",
                json!([{"op": "update", "id": x.to_string(), "body": "The logs stay, still."}]),
                &[],
                None,
            );
            let c = entry(&kept, &x);
            assert_eq!(c["because"]["changed"], d.to_string(), "{kept:#}");
            assert_eq!(c["already_written"], true, "{c}");
            assert_eq!(c["fix"], SUP_CONFIRM, "{c}");
        }
    }
    // an ordinary edit with a cut dependent and no match asks for an update
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let big = write(
        &mut r,
        "d",
        json!([{"op": "create", "type": "claim", "id": "$big", "parent": s.section.to_string(),
                "body": format!("{long} It cites {{{{{}}}}}.", s.c)}]),
    )
    .unwrap();
    let big = created(&big, "$big");
    land(&mut r, "d").unwrap();
    let e = Local::new(r);
    let v = write_in(
        &e,
        "w",
        json!([{"op": "update", "id": s.c.to_string(), "body": "C stands with others."}]),
        &[],
        None,
    );
    let c = entry(&v, &big);
    assert_eq!(c["follow_up"], "update", "{c}");
    assert_eq!(c["fix"], EDIT_CUT, "{c}");
    assert!(c.get("matched").is_none(), "{c}");
    assert_eq!(c["because"]["values"], json!([]), "{c}");
}

/// An accepted decision depending on A (decided in view `d` and landed).
fn accepted_on(r: &mut Rdy, s: &Spec, statement: &str) -> BlockId {
    let d = r
        .decide(
            &writer(),
            "d",
            DecisionInput {
                parent: s.section.to_string(),
                statement: statement.into(),
                depends_on: vec![s.a.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let d = created(&d, "$decision");
    land(r, "d").unwrap();
    d
}

#[test]
fn sdc5_decision_and_deleted_target_precedence_is_unchanged() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let live = accepted_on(&mut r, &s, "Live on A");
    let partly = accepted_on(&mut r, &s, "Partly on A");
    let fully = accepted_on(&mut r, &s, "Fully on A");
    let revoked = accepted_on(&mut r, &s, "Revoked on A");
    r.supersede(
        &writer(),
        "x",
        &partly.to_string(),
        Some("scope".into()),
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Narrow it".into(),
            ..Default::default()
        },
    )
    .unwrap();
    r.supersede(
        &writer(),
        "x",
        &fully.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Replace it".into(),
            ..Default::default()
        },
    )
    .unwrap();
    r.revoke(
        &writer(),
        "x",
        &revoked.to_string(),
        "not needed",
        &s.section.to_string(),
        None,
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let rejected = rdy_qa::golden::write_as(
        &approver(),
        &mut r,
        "y",
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": s.section.to_string(),
            "fields": {"statement": "Rejected on A", "status": "rejected",
                       "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                       "options": ["yes", "no"]},
            "links": {"depends_on": [{"target": s.a.to_string()}]}}]),
    )
    .unwrap();
    let rejected = created(&rejected, "$x");
    land(&mut r, "y").unwrap();
    let proposed = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Proposed on A, for 30 days".into(),
                depends_on: vec![s.a.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let proposed = created(&proposed, "$decision");
    let e = Local::new(r);
    let v = write_in(
        &e,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A changed."}]),
        &[],
        None,
    );
    for (d, want) in [
        (&live, "reaffirm"),
        (&partly, "reaffirm"),
        (&proposed, "update"),
    ] {
        let c = entry(&v, d);
        assert_eq!(c["follow_up"], want, "{c}");
        assert!(c["because"].get("values").is_none(), "{c}");
        assert!(c.get("matched").is_none(), "{c}");
        let fix = if want == "reaffirm" {
            json!(REAFFIRM_FIX)
        } else {
            Value::Null
        };
        assert_eq!(c.get("fix").cloned().unwrap_or(Value::Null), fix, "{c}");
    }
    for d in [&fully, &revoked, &rejected] {
        assert!(entries(&v, d).is_empty(), "{d} asserts nothing live: {v:#}");
    }
    // a deleted target: update, with no values and no fix
    let n = write_in(
        &e,
        "w2",
        json!([{"op": "create", "type": "note", "id": "$n", "parent": s.section.to_string(),
                "body": format!("See {{{{{}}}}}.", s.c)}]),
        &[],
        None,
    );
    let n = created_in(&n, "$n");
    let v = write_in(
        &e,
        "w2",
        json!([{"op": "delete", "id": s.c.to_string()}]),
        &[],
        None,
    );
    let c = entry(&v, &n);
    assert_eq!(c["follow_up"], "update", "{c}");
    assert!(c["because"].get("values").is_none(), "{c}");
    assert!(c.get("fix").is_none(), "{c}");
}

#[test]
fn sdc5_refine_and_part_supersede_keep_confirm_with_matches() {
    for part in [false, true] {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        let (d, x) = decided_claim(
            &mut r,
            &s,
            "Keep the logs for 30 days",
            "The logs stay for 30 days.",
        );
        let e = Local::new(r);
        let input = DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep the logs for 7 days in staging".into(),
            refines: if part { vec![] } else { vec![d.to_string()] },
            ..Default::default()
        };
        let v = if part {
            supersede_in(&e, "w", &d, Some("in staging"), input)
        } else {
            let v = "w".to_string();
            write_and_report(&e, "w", None, move |r| r.decide(&writer(), &v, input)).unwrap()
        };
        let c = entry(&v, &x);
        assert_eq!(c["follow_up"], "confirm", "{part}: {c}");
        assert_eq!(c["fix"], LINEAGE_FIX, "{c}");
        assert_eq!(c["because"]["values"], json!(["30 days"]), "{c}");
        assert_eq!(c["matched"][0]["value"], "30 days", "{c}");
        assert_eq!(c["matched"][0]["field"], "body", "{c}");
        if part {
            // the direct decided_by row wins cause selection, and keeps the lineage rule
            assert_eq!(c["because"]["edge"], "decided_by", "{c}");
            assert_eq!(c["because"]["edges"], json!(["decided_by"]), "{c}");
        } else {
            assert_eq!(c["because"]["edge"], "refines", "{c}");
            assert!(c["because"].get("edges").is_none(), "{c}");
        }
    }
}

#[test]
fn sdc5_kept_suspects_keep_their_kind_fix_and_ids() {
    // a changed target: the write rewrites B and keeps its pin on the old A
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let (a, b) = (s.a.to_string(), s.b.to_string());
    let e = Local::new(r);
    let v = write_in(
        &e,
        "w",
        json!([
            {"op": "update", "id": a, "body": "A holds, and more."},
            {"op": "update", "id": b, "body": format!("B builds on {{{{{a}}}}} again.")}
        ]),
        &[],
        None,
    );
    let c = entry(&v, &s.b);
    assert_eq!(c["kind"], "suspect", "{c}");
    assert_eq!(c["follow_up"], "confirm", "{c}");
    assert_eq!(c["fix"], KEPT_CHANGED_FIX, "{c}");
    assert_eq!(
        c["because"],
        json!({"edge": "references", "changed": a, "edges": ["references"]}),
        "{c}"
    );
    assert_eq!(
        c["id"],
        consequence_id(&change_of(&v), &format!("suspect {a}"), &s.b)
    );
    // (a deleted target: rdy refuses any rewrite that keeps a reference or link to a gone
    // block, E_UNKNOWN_REF, so no write keeps one; kept_follow_up's table test pins its row)
    // a retired target: D superseded in canon; rewriting A keeps decided_by D
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.supersede(
        &writer(),
        "x",
        &s.decision.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, firmly".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let e = Local::new(r);
    let v = write_in(
        &e,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A holds, still."}]),
        &[],
        None,
    );
    let c = entry(&v, &s.a);
    assert_eq!(
        c["kind"], "suspect",
        "a kept suspect, not a pending row: {v:#}"
    );
    assert_eq!(c["follow_up"], "update", "{c}");
    assert_eq!(c["fix"], KEPT_REPLACED_FIX, "{c}");
    assert_eq!(c["because"]["edges"], json!(["decided_by"]), "{c}");
    assert!(c["because"].get("values").is_none(), "{c}");
    assert_eq!(
        c["id"],
        consequence_id(&change_of(&v), &format!("suspect {}", s.decision), &s.a)
    );
}

/// Canon: claim T ("Keep the logs for 30 days."), and claims X, Y, Z that reference T and state
/// 30, 14 and 7 days; T and the dependents.
fn days_fixture() -> (tempfile::TempDir, Rdy, BlockId, [BlockId; 3]) {
    let (d, mut r) = project();
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "logs", "title": "Logs", "doc_type": "page"}},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$d", "body": "Keep the logs for 30 days."},
        {"op": "create", "type": "claim", "id": "$x", "parent": "$d", "body": "X says {{$t}}: the logs stay 30 days."},
        {"op": "create", "type": "claim", "id": "$y", "parent": "$d", "body": "Y says {{$t}}: the logs stay 14 days."},
        {"op": "create", "type": "claim", "id": "$z", "parent": "$d", "body": "Z says {{$t}}: the logs stay 7 days."}
    ]))
    .unwrap();
    land(&mut r, "base").unwrap();
    let id = |n: &str| created(&w, n);
    (d, r, id("$t"), [id("$x"), id("$y"), id("$z")])
}

fn set_t(e: &Local, view: &str, t: &BlockId, days: &str) -> Value {
    write_in(
        e,
        view,
        json!([{"op": "update", "id": t.to_string(), "body": format!("Keep the logs for {days}.")}]),
        &[],
        None,
    )
}

#[test]
fn sdc5_write_compares_its_starting_version() {
    let (dir, r, t, [x, y, z]) = days_fixture();
    let e = Local::new(r);
    let first = set_t(&e, "w", &t, "14 days");
    assert_eq!(entry(&first, &x)["because"]["values"], json!(["30 days"]));
    let second = set_t(&e, "w", &t, "7 days");
    for (block, want, value) in [
        (&x, "update", Some("30 days")),
        (&y, "update", Some("14 days")),
        (&z, "confirm", None),
    ] {
        let c = entry(&second, block);
        assert_eq!(
            c["because"]["values"],
            json!(["30 days", "14 days"]),
            "base 30 and start 14: {c}"
        );
        assert_eq!(c["follow_up"], want, "{c}");
        match value {
            Some(v) => assert_eq!(
                c["matched"],
                json!([{"value": v, "field": "body", "line": 1,
                "span": [c["matched"][0]["span"][0].clone(), c["matched"][0]["span"][1].clone()],
                "text": c["matched"][0]["text"].clone()}]),
                "{c}"
            ),
            None => assert!(c.get("matched").is_none(), "{c}"),
        }
    }
    // the public read agrees with the write reply
    let public = e
        .read()
        .consequences(&view("w"), &change_of(&second), None)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&public.items).unwrap(),
        second["consequences"]
    );
    // a combined list compares its first start and the view's base with its final version
    let both = e
        .read()
        .consequences_of(&view("w"), &[change_of(&first), change_of(&second)], None)
        .unwrap();
    let both = json!({"consequences": serde_json::to_value(&both.items).unwrap()});
    assert_eq!(
        entry(&both, &x)["because"]["values"],
        json!(["30 days"]),
        "{both:#}"
    );
    assert_eq!(entry(&both, &y)["follow_up"], "confirm", "{both:#}");
    // a restored literal is removed against neither baseline
    set_t(&e, "w2", &t, "14 days");
    let back = set_t(&e, "w2", &t, "30 days");
    let c = entry(&back, &y);
    assert_eq!(c["because"]["values"], json!(["14 days"]), "{c}");
    assert_eq!(c["follow_up"], "update", "{c}");
    assert_eq!(entry(&back, &x)["follow_up"], "confirm", "{back:#}");
    // a fresh open with a rebuilt index gives the same evidence and ids (w2's edit of T has
    // opened a tension with w since the reply, so the read just before the close is the
    // reference)
    let now = e
        .read()
        .consequences(&view("w"), &change_of(&second), None)
        .unwrap();
    e.close().unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    r.reindex().unwrap();
    let again = r
        .read()
        .consequences(&view("w"), &change_of(&second), None)
        .unwrap();
    assert_eq!(again, now);
}

#[test]
fn sdc5_propose_compares_canon_and_landed_version_uses_its_prev() {
    let (_d, r, t, [x, _, _]) = days_fixture();
    let e = Local::new(r);
    set_t(&e, "p", &t, "7 days");
    // canon moves on its own: X no longer states 30 days there
    write_in(
        &e,
        "q",
        json!([{"op": "update", "id": x.to_string(), "body": format!("X says {{{{{t}}}}}: the logs stay a while.")}]),
        &[],
        None,
    );
    let q = "q".to_string();
    land_and_report(&e, move |r| {
        let p = r.propose(&writer(), &q, &q, None)?;
        r.land_report(&approver(), &p.id)
    })
    .unwrap();
    let (p, l) = propose_and_land(&e, "p");
    let c = entry(&p, &x);
    assert_eq!(c["follow_up"], "confirm", "compared with canon's X: {c}");
    assert_eq!(c["because"]["values"], json!(["30 days"]), "{c}");
    assert_eq!(p["consequences"], l["consequences"], "{p:#} {l:#}");
    // a later landing changes X and its links; the old version's consequences stay
    let version: Hash = p["id"].as_str().unwrap().parse().unwrap();
    let before = e.read().landing_consequences(&version, None).unwrap();
    write_in(
        &e,
        "later",
        json!([{"op": "update", "id": x.to_string(), "body": "X stands alone for 30 days."}]),
        &[],
        None,
    );
    propose_and_land(&e, "later");
    let after = e.read().landing_consequences(&version, None).unwrap();
    assert_eq!(after, before);
    assert_eq!(
        serde_json::to_value(&after.items).unwrap(),
        l["consequences"]
    );
}

#[test]
fn sdc5_view_ops_compare_their_actual_transition() {
    use rdy_engine::view_api::{ViewRequest, run_view};
    let (_d, mut r, t, [x, _, _]) = days_fixture();
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "vb",
        json!([{"op": "update", "id": t.to_string(), "body": "Keep the logs for 7 days."}]),
    )
    .unwrap()
    .change;
    let e = Local::new(r);
    let a = as_agent("agent:a", "r1");
    let op = |op: Value| -> Value {
        run_view(
            &e,
            &a,
            "human:qa",
            "r1",
            serde_json::from_value::<ViewRequest>(op).unwrap(),
        )
        .unwrap()
    };
    let inc = op(json!({"op": "include", "changes": [cb]}));
    let c = entry(&inc, &x);
    assert_eq!(c["follow_up"], "update", "{c}");
    assert_eq!(c["because"]["values"], json!(["30 days"]), "{c}");
    assert_eq!(c["matched"][0]["value"], "30 days", "{c}");
    let head: Hash = inc["head"].as_str().unwrap().parse().unwrap();
    assert_eq!(c["id"], consequence_id(&head, "suspect", &x));
    // excluding it again removes 7 days, which X does not state
    let exc = op(json!({"op": "exclude", "changes": [cb]}));
    let c = entry(&exc, &x);
    assert_eq!(c["because"]["values"], json!(["7 days"]), "{c}");
    assert_eq!(c["follow_up"], "confirm", "{c}");
    // undoing the exclude puts 7 days back: 30 days is removed again
    let undo = op(json!({"op": "undo"}));
    let c = entry(&undo, &x);
    assert_eq!(c["because"]["values"], json!(["30 days"]), "{c}");
    assert_eq!(c["follow_up"], "update", "{c}");
}

#[test]
fn sdc5_value_display_and_plain_refs_differ() {
    let (_d, mut r) = project();
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "logs", "title": "Logs", "doc_type": "page"}},
        {"op": "create", "type": "value", "id": "$v", "parent": "$d", "fields": {"name": "Log retention", "value": "30", "unit": "days"}},
        {"op": "create", "type": "term", "id": "$e", "parent": "$d", "fields": {"name": "Ledger"}, "body": "The book of entries."},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$d", "body": "The Ledger is kept."},
        {"op": "create", "type": "claim", "id": "$plain", "parent": "$d", "body": "Keep the logs for {{$v}}."},
        {"op": "create", "type": "claim", "id": "$shown", "parent": "$d", "body": "Keep the logs for {{$v|30 days}}."},
        {"op": "create", "type": "claim", "id": "$ref", "parent": "$d", "body": "See {{$t}}, owned by {{$e}}."},
        {"op": "create", "type": "claim", "id": "$named", "parent": "$d", "body": "See {{$t}}: the Ledger."}
    ]))
    .unwrap();
    land(&mut r, "base").unwrap();
    let id = |n: &str| created(&w, n);
    let e = Local::new(r);
    let v = write_in(
        &e,
        "w",
        json!([{"op": "update", "id": id("$v").to_string(), "fields": {"value": "7"}}]),
        &[],
        None,
    );
    let plain = entry(&v, &id("$plain"));
    assert_eq!(plain["follow_up"], "confirm", "{plain}");
    assert_eq!(plain["because"]["values"], json!(["30 days"]), "{plain}");
    let shown = entry(&v, &id("$shown"));
    assert_eq!(shown["follow_up"], "update", "{shown}");
    assert_eq!(shown["matched"][0]["value"], "30 days", "{shown}");
    let v = write_in(
        &e,
        "w2",
        json!([{"op": "update", "id": id("$t").to_string(), "body": "The book is kept."}]),
        &[],
        None,
    );
    let r = entry(&v, &id("$ref"));
    assert_eq!(
        r["follow_up"], "confirm",
        "a plain reference names no entity: {r}"
    );
    assert_eq!(r["because"]["values"], json!(["Ledger"]), "{r}");
    assert_eq!(entry(&v, &id("$named"))["follow_up"], "update", "{v:#}");
}

#[test]
fn sdc5_evidence_and_briefing_bounds_are_independent() {
    let (_d, mut r) = project();
    let numbers: Vec<String> = (1..=40).map(|i| format!("{i} s")).collect();
    let stated: String = (1..=10)
        .map(|i| format!("{i} s and {i} s again. "))
        .collect();
    let long = format!("{} The end.", "Filler words go on. ".repeat(900));
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "b", "title": "Bounds", "doc_type": "page"}},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$d", "body": format!("Waits: {}.", numbers.join(", "))},
        {"op": "create", "type": "claim", "id": "$many", "parent": "$d", "body": format!("{stated}It cites {{{{$t}}}}.")},
        {"op": "create", "type": "claim", "id": "$cut", "parent": "$d", "body": format!("{long} It cites {{{{$t}}}}.")}
    ]))
    .unwrap();
    land(&mut r, "base").unwrap();
    let id = |n: &str| created(&w, n);
    let e = Local::new(r);
    // the briefing: a cut dependent with no stated literal asks for an update, states []
    let b = brief_in(&e, &view("w"), &[&id("$t")], None);
    let row = |block: &BlockId| -> Value {
        b["will_go_stale"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["block"] == block.to_string())
            .cloned()
            .unwrap_or_else(|| panic!("no row for {block}: {b:#}"))
    };
    let cut = row(&id("$cut"));
    assert_eq!(cut["follow_up"], "update", "{cut}");
    assert_eq!(cut["states"], json!([]), "{cut}");
    let many = row(&id("$many"));
    assert_eq!(many["follow_up"], "update", "{many}");
    assert_eq!(
        many["states"],
        json!(["1 s", "2 s", "3 s", "4 s", "5 s", "6 s", "7 s", "8 s"]),
        "at most eight: {many}"
    );
    // the consequences: 32 inspected literals, 8 shown; 8 rows of 20, the rest counted
    let v = write_in(
        &e,
        "w",
        json!([{"op": "update", "id": id("$t").to_string(), "body": "Waits: none."}]),
        &[],
        None,
    );
    let c = entry(&v, &id("$many"));
    assert_eq!(
        c["because"]["values"],
        json!(["1 s", "2 s", "3 s", "4 s", "5 s", "6 s", "7 s", "8 s"]),
        "{c}"
    );
    assert_eq!(
        c["because"]["values_omitted"], 24,
        "40 literals cut to 32: {c}"
    );
    assert_eq!(c["matched"].as_array().unwrap().len(), 8, "{c}");
    assert_eq!(c["matched_omitted"], 12, "{c}");
    let c = entry(&v, &id("$cut"));
    assert_eq!(c["follow_up"], "update", "{c}");
    assert_eq!(c["fix"], EDIT_CUT, "{c}");
    assert!(c.get("matched").is_none(), "{c}");
    // the outer budget counts rows, never literals
    let small = e
        .read()
        .consequences(&view("w"), &change_of(&v), Some(60))
        .unwrap();
    assert_eq!(small.items.len() + small.omitted.values().sum::<usize>(), 2);
}

/// A dependent whose only matching literal lies past the evidence cut keeps `states` empty and
/// asks for an update; `confirm` needs an empty `states` after a complete read (D-TKY89).
#[test]
fn plc6_a_literal_only_after_the_briefing_cut_keeps_states_empty() {
    let (_d, mut r) = project();
    let body = format!("{} 30 days.", "x".repeat(16_385));
    let cut_at = 16_384;
    let literal_at = body.find("30 days").unwrap();
    assert_eq!(
        literal_at, 16_386,
        "the literal starts beyond the first 16384 bytes"
    );
    assert!(literal_at >= cut_at);
    assert!(
        !body[..cut_at].contains("30 days"),
        "the checked prefix has no target literal"
    );
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plc6", "title": "Cut", "doc_type": "page"}},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$d", "body": "Keep events for 30 days."},
        {"op": "create", "type": "claim", "id": "$cut", "parent": "$d", "body": body,
            "links": {"depends_on": [{"target": "$t"}]}},
        {"op": "create", "type": "claim", "id": "$short", "parent": "$d", "body": "No duration is stated here.",
            "links": {"depends_on": [{"target": "$t"}]}}
    ]))
    .unwrap();
    land(&mut r, "base").unwrap();
    let id = |n: &str| created(&w, n);
    let e = Local::new(r);
    let b = brief_in(&e, &view("w"), &[&id("$t")], None);
    let rows = |block: &BlockId| -> Vec<Value> {
        b["will_go_stale"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["block"] == block.to_string())
            .cloned()
            .collect()
    };
    let cut = rows(&id("$cut"));
    assert_eq!(cut.len(), 1, "one row for the cut dependent: {b:#}");
    assert_eq!(cut[0]["follow_up"], "update", "{}", cut[0]);
    assert_eq!(cut[0]["states"], json!([]), "{}", cut[0]);
    let short = rows(&id("$short"));
    assert_eq!(short.len(), 1, "one row for the short dependent: {b:#}");
    assert_eq!(short[0]["follow_up"], "confirm", "{}", short[0]);
    assert_eq!(short[0]["states"], json!([]), "{}", short[0]);
}

#[test]
fn sdc5_evidence_changes_invalidate_a_briefing_page() {
    let (_d, r, t) = tally();
    let e = Local::new(r);
    let b = brief_in(&e, &view("w"), &[&t.d30], Some(150));
    let cursor = b["next"].as_str().expect("a cut briefing").to_string();
    // rename the entity D30 names: the stated literals of its dependents change
    let tb = brief_in(&e, &view("w"), &[&t.t_event], None);
    write_in(
        &e,
        "w",
        json!([{"op": "update", "id": t.t_event.to_string(),
                "fields": {"name": "Crash report", "aliases": []}}]),
        &[],
        Some(token(&tb)),
    );
    let err = e
        .read()
        .brief_page(&view("w"), &e.briefs(), &cursor, Some(150))
        .unwrap_err();
    assert_eq!(err.code, "E_PAGE_STALE", "{err}");
    // a fresh briefing still covers the decision and records its version
    let fresh = brief_in(&e, &view("w"), &[&t.d30], None);
    assert_eq!(
        fresh["base"]["blocks"][0]["id"],
        t.d30.to_string(),
        "{fresh:#}"
    );
}

/// Spec in canon with A decided_by D and implements E (both accepted); D and E.
fn two_targets(r: &mut Rdy) -> (Spec, BlockId) {
    let s = spec_doc(r);
    let d = r
        .decide(
            &writer(),
            "d",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Ship A".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let e = created(&d, "$decision");
    write(
        r,
        "d",
        json!([{"op": "update", "id": s.a.to_string(),
                "links": {"implements": [{"target": e.to_string()}]}}]),
    )
    .unwrap();
    land(r, "d").unwrap();
    (s, e)
}

fn replace(statement: &str, s: &Spec) -> DecisionInput {
    DecisionInput {
        parent: s.section.to_string(),
        statement: statement.into(),
        ..Default::default()
    }
}

#[test]
fn sdc5_pending_replacements_are_scope_local_and_deterministic() {
    // two retained targets, each replaced by a proposed decision in w
    let (_d, mut r) = project();
    let (s, ship) = two_targets(&mut r);
    let e = Local::new(r);
    supersede_in(&e, "w", &s.decision, None, replace("Keep A, firmly", &s));
    supersede_in(&e, "w", &ship, None, replace("Ship A now", &s));
    supersede_in(
        &e,
        "w",
        &s.decision,
        None,
        replace("Keep A, firmly too", &s),
    );
    let edit = json!([{"op": "update", "id": s.a.to_string(), "body": "A holds, as fixed."}]);
    let v = write_in(&e, "w", edit.clone(), &[], None);
    let rows: Vec<(String, String)> = entries(&v, &s.a)
        .iter()
        .map(|c| {
            assert_eq!(c["kind"], "dependent", "{c}");
            assert_eq!(c["already_written"], true, "{c}");
            (
                c["because"]["changed"].as_str().unwrap().to_string(),
                c["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let mut want = vec![s.decision.to_string(), ship.to_string()];
    want.sort();
    assert_eq!(
        rows.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
        want,
        "one row per target, by target: {v:#}"
    );
    for (target, id) in &rows {
        assert_eq!(
            *id,
            consequence_id(&change_of(&v), &format!("dependent {target}"), &s.a)
        );
    }
    let d = entries(&v, &s.a)
        .into_iter()
        .find(|c| c["because"]["changed"] == s.decision.to_string())
        .unwrap();
    assert_eq!(d["because"]["edges"], json!(["decided_by"]), "{d}");
    assert_eq!(d["fix"], SUP_CONFIRM, "{d}");
    // another view has no replacement: nothing to report there
    let other = write_in(&e, "other", edit.clone(), &[], None);
    assert!(entries(&other, &s.a).is_empty(), "{other:#}");
    // a revoke wins over a supersede of the same target
    revoke_in(&e, "w", &s.decision, &s.section, None);
    let v = write_in(&e, "w", edit.clone(), &[], None);
    let d = entries(&v, &s.a)
        .into_iter()
        .find(|c| c["because"]["changed"] == s.decision.to_string())
        .unwrap()
        .clone();
    assert_eq!(d["fix"], REV_CONFIRM, "{d}");
    // the replacer's status: proposed reports, rejected does not, a landed one makes a suspect
    for status in ["rejected", "live", "partly", "superseded", "revoked"] {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        match status {
            "rejected" => {
                rdy_qa::golden::write_as(
                    &approver(),
                    &mut r,
                    "w",
                    json!([{"op": "create", "type": "decision", "id": "$x", "parent": s.section.to_string(),
                        "fields": {"statement": "Not A", "status": "rejected",
                                   "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                                   "options": ["yes", "no"]},
                        "links": {"supersedes": [{"target": s.decision.to_string()}]}}]),
                )
                .unwrap();
            }
            _ => {
                let w = r
                    .supersede(
                        &writer(),
                        "x",
                        &s.decision.to_string(),
                        None,
                        replace("Keep A, firmly", &s),
                    )
                    .unwrap();
                let rep = created(&w, "$decision");
                land(&mut r, "x").unwrap();
                match status {
                    "partly" => {
                        r.supersede(
                            &writer(),
                            "y",
                            &rep.to_string(),
                            Some("firmly".into()),
                            replace("Keep A", &s),
                        )
                        .unwrap();
                        land(&mut r, "y").unwrap();
                    }
                    "superseded" => {
                        r.supersede(
                            &writer(),
                            "y",
                            &rep.to_string(),
                            None,
                            replace("Keep A, loosely", &s),
                        )
                        .unwrap();
                        land(&mut r, "y").unwrap();
                    }
                    "revoked" => {
                        r.revoke(
                            &writer(),
                            "y",
                            &rep.to_string(),
                            "not needed",
                            &s.section.to_string(),
                            None,
                        )
                        .unwrap();
                        land(&mut r, "y").unwrap();
                    }
                    _ => {}
                }
            }
        }
        let e = Local::new(r);
        let v = write_in(&e, "w", edit_of(&s), &[], None);
        let pending: Vec<&Value> = entries(&v, &s.a)
            .into_iter()
            .filter(|c| c["kind"] == "dependent")
            .collect();
        assert!(pending.is_empty(), "{status}: {v:#}");
        let kept = entries(&v, &s.a)
            .into_iter()
            .filter(|c| c["kind"] == "suspect" && c["because"]["changed"] == s.decision.to_string())
            .count();
        assert_eq!(kept, usize::from(status != "rejected"), "{status}: {v:#}");
    }
    // insertion order does not change the order of the rows: by block, then target
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let e = Local::new(r);
    supersede_in(&e, "w", &s.decision, None, replace("Keep A, firmly", &s));
    let c = write_in(
        &e,
        "w",
        json!([{"op": "create", "type": "claim", "id": "$q", "parent": s.section.to_string(),
                "body": "Q.", "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
        &[],
        None,
    );
    let q = created_in(&c, "$q");
    let order = |first_a: bool, n: usize| -> Vec<String> {
        let mut ops = vec![
            json!({"op": "update", "id": s.a.to_string(), "body": format!("A, again {n}.")}),
            json!({"op": "update", "id": q.to_string(), "body": format!("Q, again {n}.")}),
        ];
        if !first_a {
            ops.reverse();
        }
        let v = write_in(&e, "w", json!(ops), &[], None);
        items(&v)
            .iter()
            .filter(|c| c["kind"] == "dependent")
            .map(|c| c["block"].as_str().unwrap().to_string())
            .collect()
    };
    let one = order(true, 1);
    let mut want = vec![s.a.to_string(), q.to_string()];
    want.sort();
    assert_eq!(one, want, "by block id");
    assert_eq!(order(false, 2), want);
}

fn edit_of(s: &Spec) -> Value {
    json!([{"op": "update", "id": s.a.to_string(), "body": "A holds, as fixed."}])
}

#[test]
fn sdc5_mcp_and_cli_show_the_same_evidence() {
    use rdy_daemon::{OpContext, dispatch};
    use rdy_mcp::tools::{self, McpState, SessionParams};
    let (dir, mut r, t) = tally();
    let b = brief_of(&r, &view("src"), &[&t.d30, &t.s_dec]);
    let sup = r
        .supersede(
            &writer(),
            "src",
            &t.d30.to_string(),
            None,
            d7(&t, Some(b.base.token)),
        )
        .unwrap()
        .change;
    let e = Arc::new(Local::new(r));
    let mut st = McpState::new(e.clone() as Arc<dyn Engine>);
    tools::rdy_session(
        &mut st,
        SessionParams {
            actor: "agent:m".into(),
            run: Some("m1".into()),
            role: "writer".into(),
            view: None,
            areas: None,
        },
    )
    .unwrap();
    let mcp = rdy_mcp::view_tools::rdy_view(
        &mut st,
        serde_json::from_value(json!({"op": "include", "changes": [sup]})).unwrap(),
    )
    .unwrap();
    let mcp_brief = tools::rdy_brief(
        &mut st,
        serde_json::from_value(json!({"ids": [t.d30.to_string()], "scope": "canon"})).unwrap(),
    )
    .unwrap();
    let s = as_agent("agent:c", "c1");
    let cx = OpContext {
        engine: &*e,
        session: &s,
        human: "human:t",
        cwd: dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let ctl = dispatch(&cx, "view", json!({"op": "include", "changes": [sup]})).unwrap();
    let ctl_brief = dispatch(
        &cx,
        "brief",
        json!({"ids": [t.d30.to_string()], "scope": "canon"}),
    )
    .unwrap();
    assert_eq!(without_ids(&mcp), without_ids(&ctl), "{mcp:#} {ctl:#}");
    let flow = entry(&ctl, &t.f_purge);
    assert_eq!(flow["matched"][0]["span"], json!([55, 62]), "{flow}");
    assert_eq!(mcp_brief["will_go_stale"], ctl_brief["will_go_stale"]);
    assert!(
        ctl_brief["will_go_stale"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["states"].is_array()),
        "{ctl_brief:#}"
    );
}

#[test]
fn sdc5_schema_describes_literal_followups_and_retained_links() {
    use rdy_mcp::tools::{self, CONSEQUENCES_DOC, EDIT_LOOP_DOC, McpState};
    let (_d, r) = project();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    let s = tools::rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert_eq!(s["consequences"], CONSEQUENCES_DOC);
    assert_eq!(s["edit_loop"], EDIT_LOOP_DOC);
    for fragment in [
        "Every write response lists consequences: {id, block, kind (conflict, suspect or dependent; tension (a tension the write opened: resolve it, or ask a reviewer to dismiss it)), because {edge, changed, edges?, values?, values_omitted?}, follow_up (update, confirm, reaffirm or resolve), matched?, matched_omitted?, fix?, already_written?}; max_tokens bounds them.",
        "A nondecision's follow-up comes from changed literals, not its type. The consequences topic gives the matching rules and bounds. A will_go_stale row adds states: up to eight covered literals the dependent states. It asks update when states is nonempty or rdy cut the dependent scan. It asks confirm only when states is empty after a complete scan.\n\nA decision keeps its status-based follow-up. A deleted target asks update. A full supersede lists every actionable link kind to the selected old target in because.edges. A fix write that keeps such a link reports it again in consequences with already_written: true. Change every listed link kind; an update keeps the kinds it does not name.",
        "A reach entry (FW-LP6) needs no repin: check it and update it only if it no longer holds.",
        "Every write response carries on_change",
        "A full supersede reaches only through the refines passed to rdy_supersede",
    ] {
        assert!(
            EDIT_LOOP_DOC.contains(fragment),
            "{fragment}\n---\n{EDIT_LOOP_DOC}"
        );
    }
    assert!(EDIT_LOOP_DOC.ends_with("an update keeps the kinds it does not name."));
    for fragment in [
        "because.edges lists the link kinds from the block to that target, including implements. A reference is listed only when it is because.edge.",
        "A plain value reference does not match its rendered value.",
        "Read at most 16384 dependent bytes and 32 distinct changed literals. A cut dependent with no match gets update.",
        "A repin cannot clear a link to a retired decision. A fix write reports a retained link in consequences, even before the replacement lands. on_change remains the tension delta.",
        "Refine and part-supersede reach keeps confirm for nondecisions and can carry matched evidence.",
    ] {
        assert!(
            CONSEQUENCES_DOC.contains(fragment),
            "{fragment}\n---\n{CONSEQUENCES_DOC}"
        );
    }
    assert_eq!(
        CONSEQUENCES_DOC.matches("\n\n").count(),
        8,
        "nine paragraphs"
    );
}

#[test]
fn sdc5_unlinked_blocks_are_not_scanned() {
    let scans = |extra: usize| -> usize {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        if extra > 0 {
            let ops: Vec<Value> = (0..extra)
                .map(|i| {
                    json!({"op": "create", "type": "claim", "parent": s.section.to_string(),
                                "body": format!("Unrelated claim {i} for 30 days.")})
                })
                .collect();
            write(&mut r, "other", json!(ops)).unwrap();
            land(&mut r, "other").unwrap();
        }
        let w = write(
            &mut r,
            "w",
            json!([{"op": "update", "id": s.a.to_string(), "body": "A holds for 7 days."}]),
        )
        .unwrap();
        let st = r.read();
        st.lexicon(&view("w")).unwrap();
        st.lexicon(&Scope::Canon).unwrap();
        st.graph(&view("w")).unwrap();
        let before = rdy_engine::lexicon::text_scans();
        st.consequences(&view("w"), &w.change.parse().unwrap(), None)
            .unwrap();
        rdy_engine::lexicon::text_scans() - before
    };
    let few = scans(0);
    assert!(few > 0, "the evidence scan counts its reads");
    assert_eq!(scans(200), few, "only declared dependents are read");
}

// ---------------------------------------------------------------- fix round 1

/// Fix round 1, finding 1: one reply compares the write's own replacement for a normal entry
/// and every active replacement for a retained link, each with its own set. X (untouched) is
/// compared with the new decision that keeps 30 days; Y (rewritten, keeping its link) with that
/// one and the pending 7-day replacement, which removes 30 days.
#[test]
fn sdc5_each_comparison_uses_its_own_replacements() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let (d, x) = decided_claim(
        &mut r,
        &s,
        "Keep the logs for 30 days",
        "The logs stay for 30 days.",
    );
    let y = write(
        &mut r,
        "d2",
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": s.section.to_string(),
                "body": "Y keeps the logs for 30 days.",
                "links": {"decided_by": [{"target": d.to_string()}]}}]),
    )
    .unwrap();
    let y = created(&y, "$y");
    land(&mut r, "d2").unwrap();
    let e = Local::new(r);
    supersede_in(&e, "w", &d, None, replace("Keep the logs for 7 days", &s));
    let v = write_in(
        &e,
        "w",
        json!([
            {"op": "create", "type": "decision", "id": "$n", "parent": s.section.to_string(),
             "fields": {"statement": "Keep the logs for 30 days, firmly", "status": "proposed"},
             "links": {"supersedes": [{"target": d.to_string()}]}},
            {"op": "update", "id": y.to_string(), "body": "Y keeps the logs for 30 days, still."}
        ]),
        &[],
        None,
    );
    // the normal entry: the write's own replacement keeps 30 days
    let cx = entry(&v, &x);
    assert_eq!(cx["because"]["changed"], d.to_string(), "{cx}");
    assert_eq!(cx["because"]["values"], json!([]), "{cx}");
    assert_eq!(cx["follow_up"], "confirm", "{cx}");
    assert_eq!(cx["fix"], SUP_CONFIRM, "{cx}");
    // the retained link: the pending 7-day replacement removes 30 days
    let cy = entry(&v, &y);
    assert_eq!(cy["kind"], "dependent", "{cy}");
    assert_eq!(cy["already_written"], true, "{cy}");
    assert_eq!(cy["because"]["changed"], d.to_string(), "{cy}");
    assert_eq!(cy["because"]["values"], json!(["30 days"]), "{v:#}");
    assert_eq!(cy["follow_up"], "update", "{cy}");
    assert_eq!(cy["fix"], SUP_UPDATE, "{cy}");
    assert_eq!(cy["matched"][0]["value"], "30 days", "{cy}");
}

/// Fix round 1, finding 2: a batch that only creates, citing only what it creates, still
/// reports a retained link to a decision another created decision supersedes: in the write, at
/// propose and after the landing.
#[test]
fn sdc5_an_all_created_batch_reports_its_retained_link() {
    let (_d, r) = project();
    let e = Local::new(r);
    let v = write_in(
        &e,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "logs", "title": "Logs", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "decision", "id": "$d30", "parent": "$s",
             "fields": {"statement": "Keep the logs for 30 days", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$d7", "parent": "$s",
             "fields": {"statement": "Keep the logs for 7 days", "status": "proposed"},
             "links": {"supersedes": [{"target": "$d30"}]}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s",
             "body": "The logs stay for 30 days.",
             "links": {"decided_by": [{"target": "$d30"}]}}
        ]),
        &[],
        None,
    );
    let (d30, x) = (created_in(&v, "$d30"), created_in(&v, "$x"));
    let check = |v: &Value, at: &str| {
        let c = entry(v, &x);
        assert_eq!(c["because"]["changed"], d30.to_string(), "{at}: {v:#}");
        assert_eq!(c["because"]["edges"], json!(["decided_by"]), "{at}: {c}");
        assert_eq!(c["because"]["values"], json!(["30 days"]), "{at}: {c}");
        assert_eq!(c["follow_up"], "update", "{at}: {c}");
        assert_eq!(c["fix"], SUP_UPDATE, "{at}: {c}");
        assert_eq!(c["already_written"], true, "{at}: {c}");
        assert_eq!(c["matched"][0]["value"], "30 days", "{at}: {c}");
        assert_eq!(c["kind"], "dependent", "{at}: {c}");
    };
    check(&v, "write");
    let (p, l) = propose_and_land(&e, "w");
    check(&p, "propose");
    // the landing accepted both decisions: D30 is superseded in canon, and the kept link is a
    // kept suspect, with its own follow-up and fix
    let c = entry(&l, &x);
    assert_eq!(c["kind"], "suspect", "{l:#}");
    assert_eq!(c["because"]["changed"], d30.to_string(), "{c}");
    assert_eq!(c["because"]["edges"], json!(["decided_by"]), "{c}");
    assert_eq!(c["follow_up"], "update", "{c}");
    assert_eq!(c["fix"], KEPT_REPLACED_FIX, "{c}");
    assert_eq!(c["already_written"], true, "{c}");
    assert_eq!(
        c["id"],
        consequence_id(
            &l["version"].as_str().unwrap().parse().unwrap(),
            &format!("suspect {d30}"),
            &x
        ),
        "{c}"
    );
}

/// An engine that runs `next` as a separate write right after the first write it is given
/// returns, before its caller reads the state again: a later write that lands between a write
/// and its report.
struct Interleaved {
    inner: Local,
    next: Mutex<Option<rdy_engine::engine::WriteFn>>,
}

impl Engine for Interleaved {
    fn read(&self) -> Arc<ReadState> {
        self.inner.read()
    }
    fn write(&self, f: rdy_engine::engine::WriteFn) -> Result<Value, RdyError> {
        let out = self.inner.write(f);
        if let Some(g) = self.next.lock().unwrap().take() {
            self.inner.write(g).expect("the interleaved write");
        }
        out
    }
    fn open_report(&self) -> OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> std::path::PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<rdy_engine::brief::BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
        self.inner.leases()
    }
}

/// Fix round 1, finding 3: a write's consequences come from the state it published. A second
/// write that restores T's 30 days between the first write and its report changes nothing in
/// the first reply.
#[test]
fn sdc5_a_later_write_cannot_change_a_write_reply() {
    let edit = |days: &str, t: &BlockId| {
        let (t, days) = (t.to_string(), days.to_string());
        move |r: &mut Rdy| {
            r.write(
                &writer(),
                WriteRequest {
                    view: "w".into(),
                    ops: parse_test_ops(
                        json!([{"op": "update", "id": t, "body": format!("Keep the logs for {days}.")}]),
                    )?,
                    justification: None,
                    message: String::new(),
                },
            )
        }
    };
    // the reference: the same write with nothing after it
    let (_d0, r0, t0, [x0, y0, z0]) = days_fixture();
    let alone = write_and_report(&Local::new(r0), "w", None, edit("7 days", &t0)).unwrap();
    // the same write, with a write back to 30 days between it and its report
    let (_d, r, t, [x, y, z]) = days_fixture();
    let back = edit("30 days", &t);
    let e = Interleaved {
        inner: Local::new(r),
        next: Mutex::new(Some(Box::new(move |r: &mut Rdy| {
            Ok(serde_json::to_value(back(r)?).unwrap())
        }))),
    };
    let v = write_and_report(&e, "w", None, edit("7 days", &t)).unwrap();
    assert!(e.next.lock().unwrap().is_none(), "the second write ran");
    assert_eq!(
        e.read()
            .materialized(&view("w"))
            .unwrap()
            .get(&t)
            .unwrap()
            .body,
        "Keep the logs for 30 days.",
        "the second write landed before the report"
    );
    let c = entry(&v, &x);
    assert_eq!(c["because"]["values"], json!(["30 days"]), "{v:#}");
    assert_eq!(c["follow_up"], "update", "{c}");
    // the reply equals the one with nothing after it, up to the fixtures' own ids
    let named = |v: &Value, ids: [&BlockId; 4]| -> Vec<Value> {
        let mut text = serde_json::to_string(&without_ids(v)).unwrap();
        for (id, name) in ids.iter().zip(["T", "X", "Y", "Z"]) {
            text = text.replace(&id.to_string(), name);
        }
        let mut rows: Vec<Value> = serde_json::from_str(&text).unwrap();
        rows.sort_by_key(|c| c["block"].as_str().unwrap().to_string());
        rows
    };
    assert_eq!(
        named(&v, [&t, &x, &y, &z]),
        named(&alone, [&t0, &x0, &y0, &z0]),
        "{v:#}"
    );
}

// ---------------------------------------------------------------- final review, fix round 1

/// Final review fix round 1, finding 1: two baselines with the same target text still name
/// different entities when the names moved between them (D-EGXAS, D-CFC6V). Canon has A
/// (Storage) and B (Queue), T says "Use Storage."; in the view A becomes Archive and B
/// becomes Storage, then T drops the name. The base names A, the write's start names B: both
/// are removed, so X (Archive, now A's name) and Y (Storage, now B's name) both update.
#[test]
fn sdc5_moved_names_keep_both_baselines_of_an_unchanged_target() {
    let (_d, mut r) = project();
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "parts", "title": "Parts", "doc_type": "page"}},
        {"op": "create", "type": "component", "id": "$a", "parent": "$d", "fields": {"name": "Storage"}, "body": "One part."},
        {"op": "create", "type": "component", "id": "$b", "parent": "$d", "fields": {"name": "Queue"}, "body": "Another part."},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$d", "body": "Use Storage."},
        {"op": "create", "type": "claim", "id": "$x", "parent": "$d", "body": "X follows {{$t}}: Use Archive."},
        {"op": "create", "type": "claim", "id": "$y", "parent": "$d", "body": "Y follows {{$t}}: Use Storage."}
    ]))
    .unwrap();
    land(&mut r, "base").unwrap();
    let id = |n: &str| created(&w, n);
    let (a, b, t, x, y) = (id("$a"), id("$b"), id("$t"), id("$x"), id("$y"));
    let e = Local::new(r);
    write_in(
        &e,
        "w",
        json!([
            {"op": "update", "id": a.to_string(), "fields": {"name": "Archive"}},
            {"op": "update", "id": b.to_string(), "fields": {"name": "Storage"}}
        ]),
        &[],
        None,
    );
    let v = write_in(
        &e,
        "w",
        json!([{"op": "update", "id": t.to_string(), "body": "No selected component."}]),
        &[],
        None,
    );
    for (block, matched) in [(&x, "Archive"), (&y, "Storage")] {
        let c = entry(&v, block);
        assert_eq!(
            c["because"]["values"],
            json!(["Storage", "Storage"]),
            "A from the base, B from the start: {c}"
        );
        assert_eq!(c["follow_up"], "update", "{c}");
        assert_eq!(c["matched"][0]["value"], "Storage", "{c}");
        let span = c["matched"][0]["span"].clone();
        let body = e
            .read()
            .materialized(&view("w"))
            .unwrap()
            .get(block)
            .unwrap()
            .body
            .clone();
        let [s, en] = [span[0].as_u64().unwrap(), span[1].as_u64().unwrap()];
        assert_eq!(&body[s as usize..en as usize], matched, "{c}");
    }
    // the public read agrees with the write reply
    let public = e
        .read()
        .consequences(&view("w"), &change_of(&v), None)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&public.items).unwrap(),
        v["consequences"]
    );
}

/// Final review fix round 1, finding 2: a reference that the 16,384-byte cut leaves open ends
/// the literal right before it. The briefing states it, and a write that removes it matches
/// it at its stored bytes; the cut flag stays.
#[test]
fn sdc5_a_literal_before_a_cut_reference_is_stated_and_matched() {
    let (_d, mut r) = project();
    let filler = "x".repeat(17_000);
    let w = write(&mut r, "base", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "logs", "title": "Logs", "doc_type": "page"}},
        {"op": "create", "type": "component", "id": "$m", "parent": "$d", "fields": {"name": "Ledger"}, "body": "The book of entries."},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$d", "body": "Keep 30 days, set `max_retries`, use the Ledger."},
        {"op": "create", "type": "claim", "id": "$num", "parent": "$d", "body": format!("30 days{{{{$t|{filler}}}}}")},
        {"op": "create", "type": "claim", "id": "$code", "parent": "$d", "body": format!("max_retries{{{{$t|{filler}}}}}")},
        {"op": "create", "type": "claim", "id": "$ent", "parent": "$d", "body": format!("the Ledger{{{{$t|{filler}}}}}")}
    ]))
    .unwrap();
    land(&mut r, "base").unwrap();
    let id = |n: &str| created(&w, n);
    let t = id("$t");
    let cases = [
        (id("$num"), "30 days", [0, 7]),
        (id("$code"), "max_retries", [0, 11]),
        (id("$ent"), "Ledger", [4, 10]),
    ];
    let e = Local::new(r);
    let b = brief_in(&e, &view("w"), &[&t], None);
    for (block, value, _) in &cases {
        let row = b["will_go_stale"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["block"] == block.to_string())
            .unwrap_or_else(|| panic!("no row for {block}: {b:#}"));
        assert_eq!(row["states"], json!([value]), "{row}");
        assert_eq!(row["follow_up"], "update", "{row}");
    }
    let v = write_in(
        &e,
        "w",
        json!([{"op": "update", "id": t.to_string(), "body": "Keep nothing."}]),
        &[],
        None,
    );
    for (block, value, span) in &cases {
        let c = entry(&v, block);
        assert_eq!(c["follow_up"], "update", "{c}");
        assert!(c.get("fix").is_none(), "a match, not the cutoff fix: {c}");
        assert_eq!(c["matched"][0]["value"], *value, "{c}");
        assert_eq!(c["matched"][0]["field"], "body", "{c}");
        assert_eq!(c["matched"][0]["line"], 1, "{c}");
        assert_eq!(c["matched"][0]["span"], json!(span), "{c}");
    }
}
