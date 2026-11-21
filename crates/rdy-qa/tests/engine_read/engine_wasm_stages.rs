//! wck6 (D-ZRXY0, D-52PJG, D-727FK, D-QVTJ9): module results through the stage entry points.
//! A loaded pack's verdict cache serves every read of every state published under it: a later
//! queue read of a version runs no module for an input it judged, and a cached scan gives the
//! uncached scan's rows, causes, fixes, order and omitted counts, a rule budget that runs out
//! and a smaller allowance for a cached input included. Changed module or input bytes miss.
//! The change stage judges ordinary writes, includes, excludes, undos and a full view movement;
//! a landing runs no module, and an approval moves when the module bytes change though the
//! result does not.
use rdy_engine::check::module_run::module_runs_on_this_thread_for_tests;
use rdy_engine::check::rules::rule_scans_for_tests;
use rdy_engine::check::{RuleIssue, Stage, evaluate_rules};
use rdy_engine::engine::Local;
use rdy_engine::pending::{CARD_MAX_TOKENS, PendingQuery};
use rdy_engine::*;
use rdy_mcp::tools::{rdy_session, rdy_write};
use rdy_mcp::view_tools::rdy_view;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items, confirm_before_landing};
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{ag, j_typo, put_j, upd};
use rdy_schema::Issue;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const PASS: &str = include_str!("../../fixtures/wasm/pass.b64");
const PASS_OTHER: &str = include_str!("../../fixtures/wasm/pass-other.b64");
const FAIL: &str = include_str!("../../fixtures/wasm/fail.b64");
const MARKER: &str = include_str!("../../fixtures/wasm/marker.b64");
const BURN_MARKER: &str = include_str!("../../fixtures/wasm/burn-marker.b64");

/// Pack `wck6` with module rule `m` on claims, running `module`.
fn pack(module: &str) -> String {
    format!(
        "name: wck6\nversion: \"1\"\nextends: rdy-standard\nrules:\n  - name: m\n    select: type = claim\n    module: {}\n    message: m message\n    fix: m fix\n",
        module.trim()
    )
}

/// A project of pack `wck6` running `module`, with a landed draft doc and its section; the
/// section's id.
fn world(module: &str) -> (tempfile::TempDir, Rdy, String) {
    let (d, r) = project();
    drop(r);
    use_pack(
        d.path(),
        "wck6",
        &pack(module),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    let mut r = Rdy::open(d.path()).unwrap();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let s = w.created["$s"].clone();
    (d, r, s)
}

fn claims(parent: &str, bodies: impl IntoIterator<Item = String>) -> Value {
    Value::Array(
        bodies
            .into_iter()
            .map(|b| json!({"op": "create", "type": "claim", "parent": parent, "body": b}))
            .collect(),
    )
}

fn propose_as(r: &mut Rdy, view: &str) -> ProposalInfo {
    answer_open_items(r, &ag(view), view);
    r.propose(&ag(view), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"))
}

/// A row as (block, message, fix).
type Row = (String, String, String);

fn row_of(i: &Issue) -> Row {
    (
        i.block
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        i.message.clone(),
        i.fix.clone(),
    )
}

/// A version's rule warnings, in their order, and the warnings left out.
fn warnings_of(p: &ProposalInfo) -> (Vec<Row>, u64) {
    let rows = p
        .warnings
        .iter()
        .filter(|w| w.code == W_RULE_CHECK)
        .map(row_of)
        .collect();
    let v = serde_json::to_value(p).unwrap();
    (rows, v["checks_omitted"]["warnings"].as_u64().unwrap_or(0))
}

/// The uncached scan of view `view`'s state: its rules compiled for this call, with an empty
/// cache, on every block, at `propose`. The rules are modules, which read no suspect, tension
/// or coverage set.
fn uncached(r: &Rdy, view: &str) -> (Vec<Row>, u64) {
    let st = r.read();
    let scope = Scope::View(view.into());
    let m = st.materialized(&scope).unwrap();
    let g = st.graph(&scope).unwrap();
    let (empty, none) = (BTreeSet::new(), BTreeMap::new());
    let inputs = rdy_engine::filter::FilterInputs {
        pack: st.pack(),
        m: &m,
        graph: &g,
        areas: st.area_matcher(),
        suspects: &empty,
        tensions: &empty,
        coverage: &none,
    };
    let scan = evaluate_rules(st.pack(), inputs, m.ids().cloned(), Stage::Propose).unwrap();
    assert!(scan.blocking.is_empty());
    let rows = scan
        .warnings
        .iter()
        .map(|w: &RuleIssue| row_of(&w.issue()))
        .collect();
    (rows, scan.warnings_omitted as u64)
}

fn pending_items(r: &Rdy) -> Vec<rdy_engine::pending::Item> {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
}

#[test]
fn wck6_second_queue_read_runs_no_module() {
    let (_d, mut r, s) = world(FAIL);
    put_j(
        &mut r,
        "v",
        claims(&s, (0..3).map(|k| format!("Claim {k}."))),
        j_typo(),
    );
    let runs = module_runs_on_this_thread_for_tests();
    let p = propose_as(&mut r, "v");
    assert_eq!(
        module_runs_on_this_thread_for_tests() - runs,
        3,
        "each module runs once per input"
    );
    let (rows, _) = warnings_of(&p);
    assert_eq!(rows.len(), 3);
    // queue reads of the version, on new snapshots of the same loaded pack, scan again and run
    // no module
    let facts = |r: &Rdy| -> Vec<(String, String)> {
        pending_items(r)
            .into_iter()
            .find(|i| i.r#ref == p.r#ref)
            .expect("the card")
            .facts
            .into_iter()
            .map(|f| (f.name.to_string(), f.value))
            .collect()
    };
    let (runs, scans) = (
        module_runs_on_this_thread_for_tests(),
        rule_scans_for_tests(),
    );
    let first = facts(&r);
    // a landing elsewhere publishes a new state under the same pack
    put_j(
        &mut r,
        "other",
        json!([{"op": "create", "type": "doc", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}}]),
        j_typo(),
    );
    land(&mut r, "other").unwrap();
    let second = facts(&r);
    assert!(
        rule_scans_for_tests() - scans >= 1,
        "the queue reads scan the version's rules"
    );
    assert_eq!(
        module_runs_on_this_thread_for_tests() - runs,
        0,
        "no module runs for an input it judged"
    );
    let checks = |facts: &[(String, String)]| -> Vec<(String, String)> {
        facts
            .iter()
            .filter(|(n, _)| n.starts_with("Rule check"))
            .cloned()
            .collect()
    };
    assert_eq!(checks(&first), checks(&second));
    assert_eq!(checks(&first).len(), 3, "{first:#?}");
    // the rows the cache gives are the rows of the uncached scan
    let (want, omitted) = uncached(&r, "v");
    assert_eq!(rows, want);
    assert_eq!(omitted, 0);
}

#[test]
fn wck6_cached_and_uncached_scans_give_the_same_rows() {
    // the module burns a block's whole fuel on a marked input and passes any other: 100 marked
    // claims use all of the rule's fuel
    let (d, mut r, s) = world(BURN_MARKER);
    put_j(
        &mut r,
        "v",
        claims(&s, (0..100).map(|k| format!("Marked {k} ~!~."))),
        j_typo(),
    );
    let runs = module_runs_on_this_thread_for_tests();
    let p1 = propose_as(&mut r, "v");
    assert_eq!(module_runs_on_this_thread_for_tests() - runs, 100);
    let fuel =
        |b: &str| format!("rule m could not judge {b}: the module used all of its 10000000 fuel");
    let rule_fuel = |b: &str| {
        format!("rule m could not judge {b}: the rule used all of its 1000000000 fuel in this run")
    };
    let fix = "fix the module of rule m in the pack file, or make the rule advisory".to_string();
    let (rows, omitted) = warnings_of(&p1);
    assert_eq!(rows.len(), 100);
    assert!(
        rows.iter().all(|(b, m, f)| *m == fuel(b) && *f == fix),
        "{rows:#?}"
    );
    assert_eq!((rows, omitted), uncached(&r, "v"));
    // a changed selection: an unmarked claim passes on a little fuel. When it is judged before
    // the last marked claim, that cached input has less than a block's fuel left
    let marked: BTreeSet<String> = r
        .read()
        .materialized(&Scope::View("v".into()))
        .unwrap()
        .iter()
        .filter(|b| b.body.contains("~!~"))
        .map(|b| b.id.to_string())
        .collect();
    let last = marked.iter().next_back().unwrap().clone();
    let mut extra = 0;
    let p2 = loop {
        let w = put_j(
            &mut r,
            "v",
            claims(&s, [format!("Plain {extra}.")]),
            j_typo(),
        );
        extra += 1;
        let plain = w.created.values().next().unwrap().clone();
        let runs = module_runs_on_this_thread_for_tests();
        let p = propose_as(&mut r, "v");
        if plain < last {
            assert_eq!(
                module_runs_on_this_thread_for_tests() - runs,
                1,
                "only the new input runs"
            );
            break p;
        }
        // a plain claim judged after the last marked one meets an empty rule budget: remove it
        put_j(
            &mut r,
            "v",
            json!([{"op": "delete", "id": plain}]),
            j_typo(),
        );
        assert!(extra < 5, "every plain claim sorted last");
    };
    let (rows, omitted) = warnings_of(&p2);
    assert_eq!(omitted, 0);
    let by_block: BTreeMap<&str, &str> = rows
        .iter()
        .map(|(b, m, _)| (b.as_str(), m.as_str()))
        .collect();
    assert_eq!(by_block.len(), 100, "the plain claims pass: {rows:#?}");
    assert_eq!(by_block[last.as_str()], rule_fuel(&last));
    assert!(
        marked
            .iter()
            .filter(|b| **b != last)
            .all(|b| by_block[b.as_str()] == fuel(b)),
        "{rows:#?}"
    );
    assert_eq!((rows, omitted), uncached(&r, "v"));
    // a changed input misses the cache
    let one = marked.iter().next().unwrap().clone();
    put_j(
        &mut r,
        "v",
        upd(&BlockId::parse(&one).unwrap(), "Marked again ~!~."),
        j_typo(),
    );
    let runs = module_runs_on_this_thread_for_tests();
    let p3 = propose_as(&mut r, "v");
    assert!(module_runs_on_this_thread_for_tests() - runs >= 1);
    assert_eq!(warnings_of(&p3), uncached(&r, "v"));
    // changed module bytes miss it too: the module that fails a marked input runs on every claim
    use_pack(
        d.path(),
        "wck6",
        &pack(MARKER),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    let runs = module_runs_on_this_thread_for_tests();
    let p4 = propose_as(&mut r, "v");
    assert_eq!(
        module_runs_on_this_thread_for_tests() - runs,
        101,
        "every claim, the 100 marked and the one plain, runs the new module"
    );
    let (rows, omitted) = warnings_of(&p4);
    assert_eq!(rows.len(), 100);
    assert!(
        rows.iter()
            .all(|(_, m, f)| m.ends_with(": the input holds the mark") && f == "remove the mark")
    );
    assert_eq!((rows, omitted), uncached(&r, "v"));
}

fn p<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// The blocks of a reply's `on_change.checks`.
fn judged(v: &Value) -> BTreeSet<String> {
    v["on_change"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("on_change.checks: {v:#}"))
        .iter()
        .map(|r| r["block"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn wck6_land_runs_no_module_and_holder_runs_none() {
    // the change stage runs modules on writes, includes, excludes, undos and a full view
    // movement
    let (_d, r, s) = world(FAIL);
    let e = Arc::new(Local::new(r));
    let mut st = rdy_mcp::tools::McpState::new(e.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:a", "run": "a", "role": "writer"})),
    )
    .unwrap();
    let first = |v: &Value| -> String {
        v["created"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    };
    let v = rdy_write(
        &mut st,
        p(json!({"view": "w", "ops": claims(&s, ["A.".to_string()])})),
    )
    .unwrap();
    let a = first(&v);
    assert_eq!(judged(&v), BTreeSet::from([a.clone()]), "a write: {v:#}");
    let change = v["change"].as_str().unwrap().to_string();
    let v = rdy_view(
        &mut st,
        p(json!({"op": "include", "view": "inc", "changes": [change]})),
    )
    .unwrap();
    assert_eq!(judged(&v), BTreeSet::from([a.clone()]), "an include: {v:#}");
    let v = rdy_view(&mut st, p(json!({"op": "undo", "view": "inc"}))).unwrap();
    assert!(v["on_change"]["checks"].is_array(), "an undo: {v:#}");
    let v = rdy_view(
        &mut st,
        p(json!({"op": "exclude", "view": "w", "changes": [change]})),
    )
    .unwrap();
    assert!(v["on_change"]["checks"].is_array(), "an exclude: {v:#}");
    // a full view movement: a closed view behind canon reopens on canon's head
    // and the diff of its whole state is judged: the claim the landing made
    rdy_write(
        &mut st,
        p(json!({"view": "f", "ops": claims(&s, ["F.".to_string()])})),
    )
    .unwrap();
    rdy_view(
        &mut st,
        p(json!({"op": "close", "name": "f", "reason": "later"})),
    )
    .unwrap();
    let section = s.clone();
    let l = rdy_qa::inbox::on(&e, move |r| {
        let w = put_j(r, "l", claims(&section, ["L.".to_string()]), j_typo());
        land(r, "l").unwrap();
        w.created.values().next().unwrap().clone()
    });
    let v = rdy_view(&mut st, p(json!({"op": "undo", "view": "f"}))).unwrap();
    assert_eq!(v["rebased"], true, "{v:#}");
    assert_eq!(judged(&v), BTreeSet::from([l]), "a full movement: {v:#}");
    // a landing runs no module and scans no rule
    let (_d, mut r, s) = world(PASS);
    put_j(&mut r, "x", claims(&s, ["X.".to_string()]), j_typo());
    let px = propose_as(&mut r, "x");
    assert!(px.ready, "{:#?}", px.issues);
    confirm_before_landing(&mut r, &approver(), &px);
    let (runs, scans) = (
        module_runs_on_this_thread_for_tests(),
        rule_scans_for_tests(),
    );
    r.land_report(&approver(), &px.id).unwrap();
    assert_eq!(
        module_runs_on_this_thread_for_tests(),
        runs,
        "a landing runs no module"
    );
    assert_eq!(rule_scans_for_tests(), scans, "nor scans a rule");
    // changed module bytes with the same pass move an approval: the version's rows do not
    // change, but its pack did
    let (d, mut r, s) = world(PASS);
    put_j(&mut r, "v", claims(&s, ["V.".to_string()]), j_typo());
    let pv = propose_as(&mut r, "v");
    assert!(pv.ready, "{:#?}", pv.issues);
    use_pack(
        d.path(),
        "wck6",
        &pack(PASS_OTHER),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    put_j(&mut r, "y", claims(&s, ["Y.".to_string()]), j_typo());
    assert!(
        warnings_of(&propose_as(&mut r, "y")).0.is_empty(),
        "the new module passes too"
    );
    confirm_before_landing(&mut r, &approver(), &pv);
    let (runs, scans) = (
        module_runs_on_this_thread_for_tests(),
        rule_scans_for_tests(),
    );
    let e = r.land_report(&approver(), &pv.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert_eq!(
        e.message,
        format!(
            "proposal {} moved: the pack changed after its assessment",
            pv.r#ref
        )
    );
    let _ = (runs, scans);
    // a holder of the same project's pack files runs no module
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("holder");
    let runs = module_runs_on_this_thread_for_tests();
    drop(Rdy::create_holder(&root, rdy_model::RepoId([9; 16])).unwrap());
    std::fs::create_dir_all(root.join(".rdy/packs")).unwrap();
    use_pack(&root, "wck6", &pack(FAIL), "");
    let held = Rdy::open_holder(&root).unwrap();
    assert!(held.compiled_rules().is_empty());
    drop(held);
    assert_eq!(
        module_runs_on_this_thread_for_tests(),
        runs,
        "a holder runs no module"
    );
}
