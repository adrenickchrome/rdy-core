//! ap8 (D-ZWP7Q "The coverage gate", D-WB22B, D-T5S9A; M3c O17): with `coverage.gate:
//! no_lower`, readiness adds one `E_COVERAGE_LOWERED` issue for each page whose gate score a
//! version lowers, then one for the corpus. The gate score leaves out `ungrounded` issues and
//! the Governed checks of every block that is not a decision, so only an unapplied decision
//! counts in Governed. A page new in the version counts in the corpus only. With `off`, no
//! lowering refuses anything.
use rdy_engine::{
    COVERAGE_LOWERED_FIX, E_COVERAGE_LOWERED, ProposalInfo, Rdy, Scope, Session, WriteRequest,
};
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::{brief_of, write_cited_as};
use rdy_qa::fixtures::{land, parse_test_ops, writer};
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items};
use rdy_qa::tensions::{ag, j_typo, put_j};
use rdy_schema::{Issue, Severity};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Canon: a draft doc `notes` with a section `s` and a claim `x`, landed with the gate off; then
/// the project's `coverage.gate` is `gate`.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    notes: BlockId,
    s: BlockId,
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn world(gate: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "seed".into(),
                ops: parse_test_ops(json!([
                    {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
                    {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
                    {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    land(&mut r, "seed").unwrap();
    r.close().unwrap();
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        format!("pack: rdy-standard\ncoverage:\n  gate: {gate}\n"),
    )
    .unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    World {
        notes: id(&w.created["$n"]),
        s: id(&w.created["$s"]),
        _dir: dir,
        r,
    }
}

fn agent() -> Session {
    ag("a1")
}

/// Agent `a1` writes `ops` in its view `a1`, with an editorial justification.
fn put(w: &mut World, ops: Value) {
    put_j(&mut w.r, "a1", ops, j_typo());
}

fn propose(w: &mut World) -> ProposalInfo {
    w.r.propose(&agent(), "a1", "a1", Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose: {e}"))
}

/// An open question under section `s`: one Complete check with an open issue.
fn question(w: &World) -> Value {
    json!([{"op": "create", "type": "question", "parent": w.s.to_string(), "fields": {"prompt": "Is X enough?"}}])
}

/// The gate score (O17) of `scope`: of the blocks whose page is `doc`, or of every scored block.
fn gate_score(r: &Rdy, scope: &Scope, doc: Option<&BlockId>) -> u32 {
    let st = r.read();
    let m = st.materialized(scope).unwrap();
    let issues = st.coverage_issues(scope).unwrap();
    let entries = issues
        .blocks
        .iter()
        .filter(|(_, e)| doc.is_none_or(|d| e.doc.as_ref() == Some(d)));
    rdy_engine::coverage::gate_rollup(st.pack(), &m, entries).score
}

/// The `E_COVERAGE_LOWERED` issue of `p` on `what` (a doc path, or `the corpus`), from the gate
/// score of canon to the gate score of view `a1`, which holds canon and the version.
fn lowered(w: &World, p: &ProposalInfo, doc: Option<&BlockId>, what: &str) -> Issue {
    let before = gate_score(&w.r, &Scope::Canon, doc);
    let after = gate_score(&w.r, &Scope::View("a1".into()), doc);
    assert!(after < before, "{what}: {before} -> {after}");
    Issue {
        severity: Severity::Error,
        code: E_COVERAGE_LOWERED,
        block: doc.cloned(),
        message: format!(
            "{} lowers coverage of {what} from {before}% to {after}%",
            p.r#ref
        ),
        fix: COVERAGE_LOWERED_FIX.into(),
    }
}

/// The `E_COVERAGE_LOWERED` issue of `p` on `what`, for a version that writes a decision no
/// content carries: canon's gate score before, and after a score below it. View `a1` holds the
/// decision as proposed and gives it no Governed check, so its score is not the landing's; the
/// after score is read from `issue` and checked to be lower.
fn lowered_by_decision(
    w: &World,
    p: &ProposalInfo,
    doc: Option<&BlockId>,
    what: &str,
    issue: &Issue,
) -> Issue {
    let before = gate_score(&w.r, &Scope::Canon, doc);
    let prefix = format!("{} lowers coverage of {what} from {before}% to ", p.r#ref);
    let after: u32 = issue
        .message
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix('%'))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{} does not start {prefix}", issue.message));
    assert!(after < before, "{what}: {before} -> {after}");
    Issue {
        severity: Severity::Error,
        code: E_COVERAGE_LOWERED,
        block: doc.cloned(),
        message: format!("{prefix}{after}%"),
        fix: COVERAGE_LOWERED_FIX.into(),
    }
}

fn gate_issues(p: &ProposalInfo) -> Vec<&Issue> {
    p.issues
        .iter()
        .filter(|i| i.code == E_COVERAGE_LOWERED)
        .collect()
}

/// The overall score of the stored assessment of `p`: (corpus before, corpus after).
fn corpus_scores(r: &Rdy, p: &ProposalInfo) -> (u32, u32) {
    let st = r.read();
    let v: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&v, st.seq())
        .unwrap()
        .expect("a stored assessment");
    let a = st.reader().assessment(&h).unwrap();
    (
        a.coverage.corpus.before.overall,
        a.coverage.corpus.after.overall,
    )
}

#[test]
fn ap8_off_never_refuses_a_lowering() {
    let mut w = world("off");
    let q = question(&w);
    put(&mut w, q);
    let p = propose(&mut w);
    assert!(p.ready, "{:?}", p.issues);
    // the version does lower coverage
    let (before, after) = corpus_scores(&w.r, &p);
    assert!(after < before, "{before} -> {after}");
}

#[test]
fn ap8_no_lower_refuses_a_lowered_page() {
    let mut w = world("no_lower");
    let q = question(&w);
    put(&mut w, q);
    let p = propose(&mut w);
    assert!(!p.ready);
    let notes = w.notes.clone();
    assert_eq!(
        p.issues,
        vec![
            lowered(&w, &p, Some(&notes), "notes"),
            lowered(&w, &p, None, "the corpus"),
        ]
    );
    assert_eq!(p.policy, None);
}

#[test]
fn ap8_no_lower_refuses_a_lowered_corpus() {
    // a decision at the project root is on no page: only the corpus counts it
    let mut w = world("no_lower");
    put(
        &mut w,
        json!([{"op": "create", "type": "decision", "fields": {"statement": "Ideas stay short", "status": "proposed"}}]),
    );
    let p = propose(&mut w);
    assert!(!p.ready);
    assert_eq!(p.issues.len(), 1, "{:?}", p.issues);
    assert_eq!(
        p.issues,
        vec![lowered_by_decision(
            &w,
            &p,
            None,
            "the corpus",
            &p.issues[0]
        )]
    );
}

#[test]
fn ap8_ungrounded_content_does_not_lower_the_gate_score() {
    // making the section stable makes claim x ungrounded: the whole score falls, the gate score
    // does not
    let mut w = world("no_lower");
    let token = brief_of(&w.r, &Scope::Canon, &[&w.s]).base.token;
    write_cited_as(
        &mut w.r,
        &agent(),
        "a1",
        json!([{"op": "update", "id": w.s.to_string(), "fields": {"title": "Ideas", "lifecycle": "stable"}}]),
        Some(&token),
    )
    .unwrap();
    answer_open_items(&mut w.r, &agent(), "a1");
    let p = propose(&mut w);
    assert!(p.ready, "{:?}", p.issues);
    let (before, after) = corpus_scores(&w.r, &p);
    assert!(after < before, "{before} -> {after}");
    assert_eq!(
        gate_score(&w.r, &Scope::Canon, None),
        gate_score(&w.r, &Scope::View("a1".into()), None)
    );
}

#[test]
fn ap8_an_unapplied_decision_lowers_the_gate_score() {
    // D-T5S9A: an unapplied decision counts in Governed
    let mut w = world("no_lower");
    let ops = json!([{"op": "create", "type": "decision", "parent": w.s.to_string(),
            "fields": {"statement": "Ideas stay short", "status": "proposed"}}]);
    put(&mut w, ops);
    let p = propose(&mut w);
    assert!(!p.ready);
    assert_eq!(p.issues.len(), 2, "{:?}", p.issues);
    let notes = w.notes.clone();
    assert_eq!(
        p.issues,
        vec![
            lowered_by_decision(&w, &p, Some(&notes), "notes", &p.issues[0]),
            lowered_by_decision(&w, &p, None, "the corpus", &p.issues[1]),
        ]
    );
}

#[test]
fn ap8_a_new_page_is_compared_in_the_corpus_only() {
    // a new doc with an open question starts with no page in canon: no page issue names it, and
    // the corpus counts it
    let mut w = world("no_lower");
    put(
        &mut w,
        json!([
            {"op": "create", "type": "doc", "id": "$m", "fields": {"name": "more", "title": "More", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$m", "fields": {"title": "Open"}},
            {"op": "create", "type": "question", "parent": "$t", "fields": {"prompt": "Is more enough?"}}
        ]),
    );
    let p = propose(&mut w);
    assert!(!p.ready);
    assert_eq!(p.issues, vec![lowered(&w, &p, None, "the corpus")]);
    assert_eq!(gate_issues(&p).len(), 1);
}

/// Canon: page `notes` with a section `s` of two claims and a section with an open question,
/// and page `more` (returned) with a section holding an open question; landed with the gate off,
/// then the project's `coverage.gate` is `gate`. View `a1` moves `s` to `more`, so `notes` keeps
/// only its question and its gate score falls, `more` gains the claims, and the corpus holds the
/// same blocks.
fn moved(gate: &str) -> (World, BlockId) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let seed = r
        .write(
            &writer(),
            WriteRequest {
                view: "seed".into(),
                ops: parse_test_ops(json!([
                    {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
                    {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
                    {"op": "create", "type": "claim", "parent": "$s", "body": "X holds."},
                    {"op": "create", "type": "claim", "parent": "$s", "body": "Y holds."},
                    {"op": "create", "type": "section", "id": "$o", "parent": "$n", "fields": {"title": "Open"}},
                    {"op": "create", "type": "question", "parent": "$o", "fields": {"prompt": "Is X enough?"}},
                    {"op": "create", "type": "doc", "id": "$m", "fields": {"name": "more", "title": "More", "doc_type": "overview"}},
                    {"op": "create", "type": "section", "id": "$t", "parent": "$m", "fields": {"title": "Later"}},
                    {"op": "create", "type": "question", "parent": "$t", "fields": {"prompt": "Is more enough?"}}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    land(&mut r, "seed").unwrap();
    r.close().unwrap();
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        format!("pack: rdy-standard\ncoverage:\n  gate: {gate}\n"),
    )
    .unwrap();
    let mut w = World {
        notes: id(&seed.created["$n"]),
        s: id(&seed.created["$s"]),
        r: Rdy::open(dir.path()).unwrap(),
        _dir: dir,
    };
    let more = id(&seed.created["$m"]);
    let ops = json!([{"op": "move", "id": w.s.to_string(), "parent": more.to_string()}]);
    put(&mut w, ops);
    (w, more)
}

#[test]
fn ap8_a_move_that_lowers_only_the_source_page_is_refused() {
    // fix round 1: the source page is a page the version touches (its block's doc in canon), so
    // the gate compares it
    let (mut w, more) = moved("no_lower");
    let p = propose(&mut w);
    let view = Scope::View("a1".into());
    assert!(gate_score(&w.r, &Scope::Canon, Some(&more)) < gate_score(&w.r, &view, Some(&more)));
    assert_eq!(
        gate_score(&w.r, &Scope::Canon, None),
        gate_score(&w.r, &view, None)
    );
    assert!(!p.ready, "{:?}", p.issues);
    let notes = w.notes.clone();
    assert_eq!(p.issues, vec![lowered(&w, &p, Some(&notes), "notes")]);
}

#[test]
fn ap8_a_moves_stored_assessment_holds_the_source_and_target_pages() {
    // fix round 1: the stored coverage pages are the same pages the gate compares
    let (mut w, more) = moved("off");
    let p = propose(&mut w);
    assert!(p.ready, "{:?}", p.issues);
    let st = w.r.read();
    let v: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&v, st.seq())
        .unwrap()
        .expect("a stored assessment");
    let a = st.reader().assessment(&h).unwrap();
    let pages: BTreeSet<&BlockId> = a.coverage.pages.iter().map(|pg| &pg.doc).collect();
    assert_eq!(pages, BTreeSet::from([&w.notes, &more]));
    let notes = a
        .coverage
        .pages
        .iter()
        .find(|pg| pg.doc == w.notes)
        .unwrap();
    assert!(notes.after.overall < notes.before.overall, "{:?}", notes);
}
