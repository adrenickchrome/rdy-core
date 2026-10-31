//! rf9 (D-4WC6R, D-Z8P09): a briefing lists the sources the covered blocks reflect (a paged
//! section) and the findings the covered decisions depend on (a capped part), each one level
//! deep. The blocks carry explicit links written as typed store commits (no raw reflects input).
use rdy_engine::brief::{Coverage, FINDINGS_CAP, PAGE_SECTIONS};
use rdy_engine::*;
use rdy_model::{Block, BlockId, Change, Link, Op, Provenance, Role};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

/// A typed store commit of one op in `v`: every change `v` includes is a dep.
fn commit(r: &mut Rdy, v: &str, op: impl FnOnce(&Rdy) -> Op) {
    let op = op(r);
    let deps = r.read().included(v).to_vec();
    r.store_mut()
        .commit_to_view(
            v,
            Change {
                view: v.into(),
                ops: vec![op],
                deps,
                provenance: Provenance {
                    actor: "human:qa".into(),
                    run: None,
                    role: Role::Writer,
                    at_ms: 5,
                },
                justification: None,
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
}

/// Rewrites `b` in `v` with `edit` (a typed store commit).
fn rewrite(r: &mut Rdy, v: &str, b: &BlockId, edit: impl FnOnce(&mut Block)) {
    commit(r, v, |r| {
        let mut next = r.read().block(&view(v), b).unwrap();
        let base = next.version_hash();
        edit(&mut next);
        Op::Update { base, block: next }
    });
}

/// Sets link kind `kind` of `b` in `v` to `targets`, each pinned at its content in `v`.
fn link(r: &mut Rdy, v: &str, b: &BlockId, kind: &str, targets: &[&BlockId]) {
    let st = r.read();
    let t = std::sync::Arc::clone(st.reader().tracking());
    let pins: Vec<(BlockId, rdy_model::Hash)> = targets
        .iter()
        .map(|x| {
            (
                (*x).clone(),
                st.block(&view(v), x).unwrap().content_hash_with(&t),
            )
        })
        .collect();
    drop(st);
    rewrite(r, v, b, |blk| {
        blk.links.insert(
            kind.to_string(),
            targets
                .iter()
                .map(|x| Link {
                    target: (*x).clone(),
                    part: None,
                })
                .collect(),
        );
        blk.pins.extend(pins);
    });
}

fn decide(
    r: &mut Rdy,
    v: &str,
    parent: &BlockId,
    statement: &str,
    depends_on: &[&BlockId],
) -> BlockId {
    let w = r
        .decide(
            &writer(),
            v,
            DecisionInput {
                parent: parent.to_string(),
                statement: statement.into(),
                depends_on: depends_on.iter().map(|b| b.to_string()).collect(),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide {statement}: {e}"));
    id(&w.created["$decision"])
}

fn brief(r: &Rdy, scope: &Scope, ids: &[&BlockId], max: Option<usize>) -> brief::Briefing {
    r.read()
        .brief(
            scope,
            &Coverage {
                ids: ids.iter().map(|b| b.to_string()).collect(),
                path: None,
            },
            max,
        )
        .unwrap()
}

fn json_of(b: &brief::Briefing) -> Value {
    serde_json::to_value(b).unwrap()
}

/// Canon: doc `spec` (section Rules with claims x, y and w), doc `refs` (section Sources with
/// sources s1 release, s2 spec and s3 commit, and findings f1, f2 and f3). Then, in view `k`
/// (not landed): x reflects s1 and s2; y reflects s1 and the claim w (not a source); da depends on
/// f2 then f1 and is conditional on f3; db depends on da.
struct Fx {
    sec: BlockId,
    x: BlockId,
    y: BlockId,
    w: BlockId,
    s1: BlockId,
    s2: BlockId,
    s3: BlockId,
    f1: BlockId,
    f2: BlockId,
    f3: BlockId,
    da: BlockId,
    db: BlockId,
}

fn fixture(r: &mut Rdy) -> Fx {
    let c = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$sec", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$sec", "body": "X holds 30 days."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$sec", "body": "Y holds."},
            {"op": "create", "type": "claim", "id": "$w", "parent": "$sec", "body": "W holds."},
            {"op": "create", "type": "doc", "id": "$r", "fields": {"name": "refs", "title": "Refs", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$r", "fields": {"title": "Sources"}},
            {"op": "create", "type": "source", "id": "$s1", "parent": "$rs", "fields": {"title": "Release 1", "kind": "release", "locator": "v1"}, "body": "Retention is 30 days."},
            {"op": "create", "type": "source", "id": "$s2", "parent": "$rs", "fields": {"title": "Spec 2", "kind": "spec", "locator": "spec-2"}, "body": "Spec two."},
            {"op": "create", "type": "source", "id": "$s3", "parent": "$rs", "fields": {"title": "Commit 3", "kind": "commit", "locator": "abc"}, "body": "Commit three."},
            {"op": "create", "type": "finding", "id": "$f1", "parent": "$rs", "fields": {"statement": "First finding.", "evidence": [{"quote": "q1"}]}},
            {"op": "create", "type": "finding", "id": "$f2", "parent": "$rs", "fields": {"statement": "Second finding.", "evidence": [{"quote": "q2"}]}},
            {"op": "create", "type": "finding", "id": "$f3", "parent": "$rs", "fields": {"statement": "Third finding.", "evidence": [{"quote": "q3"}]}}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
    let g = |k: &str| id(&c.created[k]);
    let (sec, x, y, w) = (g("$sec"), g("$x"), g("$y"), g("$w"));
    let (s1, s2, s3) = (g("$s1"), g("$s2"), g("$s3"));
    let (f1, f2, f3) = (g("$f1"), g("$f2"), g("$f3"));
    link(r, "k", &x, "reflects", &[&s1, &s2]);
    link(r, "k", &y, "reflects", &[&s1, &w]);
    let da = decide(r, "k", &sec, "Keep what the findings say", &[&f2, &f1]);
    link(r, "k", &da, "conditional_on", &[&f3]);
    let db = decide(r, "k", &sec, "Build on da", &[&da]);
    Fx {
        sec,
        x,
        y,
        w,
        s1,
        s2,
        s3,
        f1,
        f2,
        f3,
        da,
        db,
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn rf9_brief_lists_sources_and_finding_premises_once() {
    let (_d, mut r) = project();
    let fx = fixture(&mut r);
    let k = view("k");
    let b = brief(&r, &k, &[&fx.x, &fx.y, &fx.da], None);
    let v = json_of(&b);
    // sources: held source targets, grouped by source id; blocks sorted; the claim w is no source
    let mut rows = vec![
        (
            fx.s1.to_string(),
            json!({"source": fx.s1.to_string(), "kind": "release", "title": "Release 1",
                   "blocks": sorted(vec![fx.x.to_string(), fx.y.to_string()]),
                   "existence": "unchecked"}),
        ),
        (
            fx.s2.to_string(),
            json!({"source": fx.s2.to_string(), "kind": "spec", "title": "Spec 2",
                   "blocks": [fx.x.to_string()], "existence": "unchecked"}),
        ),
    ];
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let rows: Vec<Value> = rows.into_iter().map(|r| r.1).collect();
    assert_eq!(v["sources"], Value::Array(rows), "{v:#}");
    // findings: the direct depends_on findings of da by (for, finding); conditional_on adds none
    let mut want = vec![
        (
            fx.f1.to_string(),
            json!({"for": fx.da.to_string(), "finding": fx.f1.to_string(), "statement": "First finding."}),
        ),
        (
            fx.f2.to_string(),
            json!({"for": fx.da.to_string(), "finding": fx.f2.to_string(), "statement": "Second finding."}),
        ),
    ];
    want.sort_by(|a, b| a.0.cmp(&b.0));
    let want: Vec<Value> = want.into_iter().map(|r| r.1).collect();
    assert_eq!(v["findings"], Value::Array(want), "{v:#}");
    assert!(v.get("findings_omitted").is_none(), "{v:#}");
    // placement: sources after suspects, findings after the related decisions and before the base
    let text = serde_json::to_string(&b).unwrap();
    let at = |k: &str| {
        text.find(&format!("\"{k}\":"))
            .unwrap_or_else(|| panic!("{k}"))
    };
    assert!(at("suspects") < at("sources") && at("sources") < at("inflight"));
    assert!(at("sources") < at("findings") && at("findings") < at("base"));
    // the registered briefing records each listed source and finding version
    for t in [&fx.s1, &fx.s2, &fx.f1, &fx.f2] {
        let now = r.read().materialized(&k).unwrap().version_of(t);
        assert!(now.is_some());
        assert_eq!(b.record.recorded.get(t), Some(&now), "{t}");
    }
    // one level only: db depends on da, which depends on findings, and db gets none
    let b = brief(&r, &k, &[&fx.db], None);
    let v = json_of(&b);
    assert!(
        v.get("findings").is_none() && v.get("findings_omitted").is_none(),
        "{v:#}"
    );
    assert!(v.get("sources").is_none(), "{v:#}");
    // empty arrays are omitted: a block with neither
    let v = json_of(&brief(&r, &k, &[&fx.w, &fx.s3, &fx.sec], None));
    assert!(
        v.get("sources").is_none() && v.get("findings").is_none(),
        "{v:#}"
    );
    // the conditional finding is recorded as a dependency but listed nowhere
    let v = json_of(&brief(&r, &k, &[&fx.da], None));
    assert!(
        !v.to_string().contains(&fx.f3.to_string()) || {
            // f3 may only appear as a recorded dependency, never in a part
            v["findings"].to_string().find(&fx.f3.to_string()).is_none()
        }
    );
}

#[test]
fn rf9_the_page_sections_name_sources_once() {
    assert_eq!(
        PAGE_SECTIONS,
        [
            "governs",
            "dependents",
            "lineage",
            "will_go_stale",
            "suspects",
            "sources",
            "inflight.views",
            "inflight.proposals",
            "inflight.intents",
            "inflight.tensions",
            "base.blocks",
        ]
    );
    assert_eq!(FINDINGS_CAP, 20);
}

/// Doc `many` in canon: section S, `n` claims that each reflect their own source (written in
/// view `k`), and a decision that depends on `m` findings. Returns the doc, the decision and the
/// findings, and the claims.
fn many(
    r: &mut Rdy,
    n: usize,
    m: usize,
) -> (BlockId, BlockId, Vec<BlockId>, Vec<BlockId>, Vec<BlockId>) {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}),
        json!({"op": "create", "type": "doc", "id": "$r", "fields": {"name": "refs", "title": "Refs", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$rs", "parent": "$r", "fields": {"title": "Sources"}}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim {i} holds.")}));
        ops.push(json!({"op": "create", "type": "source", "id": format!("$s{i}"), "parent": "$rs",
            "fields": {"title": format!("Source {i}"), "kind": "spec", "locator": format!("loc-{i}")}, "body": format!("Source {i}.")}));
    }
    for i in 0..m {
        ops.push(json!({"op": "create", "type": "finding", "id": format!("$f{i}"), "parent": "$rs",
            "fields": {"statement": format!("Finding {i}."), "evidence": [{"quote": format!("q{i}")}]}}));
    }
    let c = write(r, "seed", Value::Array(ops)).unwrap();
    land(r, "seed").unwrap();
    let g = |k: String| id(&c.created[&k]);
    let claims: Vec<BlockId> = (0..n).map(|i| g(format!("$c{i}"))).collect();
    let sources: Vec<BlockId> = (0..n).map(|i| g(format!("$s{i}"))).collect();
    let findings: Vec<BlockId> = (0..m).map(|i| g(format!("$f{i}"))).collect();
    for (c, s) in claims.iter().zip(&sources) {
        link(r, "k", c, "reflects", &[s]);
    }
    let sec = g("$s".into());
    let refs: Vec<&BlockId> = findings.iter().collect();
    let dec = if m > 0 {
        decide(r, "k", &sec, "Keep what the findings say", &refs)
    } else {
        decide(r, "k", &sec, "Keep what the findings say", &[])
    };
    (g("$d".into()), dec, findings, claims, sources)
}

fn cover_doc(doc: &BlockId) -> Coverage {
    Coverage {
        ids: vec![],
        path: Some(doc.to_string()),
    }
}

#[test]
fn rf9_new_brief_parts_page_and_go_stale() {
    let (_d, mut r) = project();
    let (doc, dec, findings, _claims, sources) = many(&mut r, 40, 21);
    let k = view("k");
    let full = r
        .read()
        .brief(&k, &cover_doc(&doc), Some(1_000_000))
        .unwrap();
    assert!(full.next.is_none(), "uncut");
    assert_eq!(full.sources.len(), 40);
    // 21 findings: the first 20 rows by (for, finding) and the count of the rest
    assert_eq!(full.findings.len(), FINDINGS_CAP);
    assert_eq!(full.findings_omitted, 1);
    let ordered = sorted(findings.iter().map(ToString::to_string).collect());
    let shown: Vec<String> = full.findings.iter().map(|f| f.finding.clone()).collect();
    assert_eq!(shown, ordered[..FINDINGS_CAP].to_vec());
    assert!(full.findings.iter().all(|f| f.r#for == dec.to_string()));

    // a cut briefing: each source occurs once across the pages; findings repeat unchanged
    let budget = 1500;
    let first = r.read().brief(&k, &cover_doc(&doc), Some(budget)).unwrap();
    first.register(&r.briefs());
    assert!(first.next.is_some(), "the briefing is cut at {budget}");
    let mut seen = first.sources.clone();
    let (mut next, mut pages) = (first.next.clone(), 1);
    assert_eq!(
        (&first.findings, first.findings_omitted),
        (&full.findings, 1)
    );
    while let Some(cursor) = next {
        let p = r
            .read()
            .brief_page(&k, &r.briefs(), &cursor, Some(budget))
            .unwrap();
        assert_eq!((&p.findings, p.findings_omitted), (&full.findings, 1));
        seen.extend(p.sources.clone());
        next = p.next.clone();
        pages += 1;
        assert!(pages < 200, "paging does not end");
    }
    assert!(pages > 1);
    assert_eq!(seen, full.sources, "every source once, in order");

    // a budget that fits nothing still makes progress through the sources
    let first = r.read().brief(&k, &cover_doc(&doc), Some(1)).unwrap();
    first.register(&r.briefs());
    let mut seen = first.sources.clone();
    let (mut next, mut pages) = (first.next.clone(), 1);
    while let Some(cursor) = next {
        let p = r
            .read()
            .brief_page(&k, &r.briefs(), &cursor, Some(1))
            .unwrap();
        seen.extend(p.sources.clone());
        next = p.next.clone();
        pages += 1;
        assert!(pages < 500, "paging does not end");
    }
    assert_eq!(seen, full.sources, "every source once at one token");
    drop(sources);
}

#[test]
fn rf9_findings_are_capped_at_twenty_and_omitted_when_none() {
    for (m, listed, omitted) in [(0usize, 0usize, 0usize), (20, 20, 0), (21, 20, 1)] {
        let (_d, mut r) = project();
        let (_doc, dec, findings, _c, _s) = many(&mut r, 1, m);
        let b = brief(&r, &view("k"), &[&dec], None);
        let v = json_of(&b);
        assert_eq!(b.findings.len(), listed, "{m} findings");
        assert_eq!(b.findings_omitted, omitted, "{m} findings");
        assert_eq!(v.get("findings").is_some(), listed > 0, "{v:#}");
        assert_eq!(v.get("findings_omitted").is_some(), omitted > 0, "{v:#}");
        let ordered = sorted(findings.iter().map(ToString::to_string).collect());
        let shown: Vec<String> = b.findings.iter().map(|f| f.finding.clone()).collect();
        assert_eq!(shown, ordered[..listed].to_vec());
    }
}

/// Briefs the doc of `many(3, m)` in view `k` cut to one token, registers it and returns its
/// cursor, with the ids a mutation needs.
fn cursor_of(
    r: &mut Rdy,
    n: usize,
    m: usize,
) -> (BlockId, BlockId, Vec<BlockId>, Vec<BlockId>, String, String) {
    let (doc, dec, findings, _claims, sources) = many(r, n, m);
    let first = r
        .read()
        .brief(&view("k"), &cover_doc(&doc), Some(1))
        .unwrap();
    first.register(&r.briefs());
    let cursor = first.next.clone().expect("cut at one token");
    (
        doc,
        dec,
        findings,
        sources,
        first.base.token.clone(),
        cursor,
    )
}

fn stale(r: &Rdy, doc: &BlockId, cursor: &str, token: &str) {
    let e = r
        .read()
        .brief_page(&view("k"), &r.briefs(), cursor, Some(1))
        .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE, "{e}");
    let again = r
        .read()
        .brief(&view("k"), &cover_doc(doc), Some(1))
        .unwrap();
    assert_ne!(again.base.token, token, "the token moved");
}

#[test]
fn rf9_a_source_edit_makes_a_cursor_stale() {
    let (_d, mut r) = project();
    let (doc, _dec, _f, sources, token, cursor) = cursor_of(&mut r, 3, 2);
    write(
        &mut r,
        "k",
        json!([{"op": "update", "id": sources[0].to_string(), "fields": {"title": "Renamed"}}]),
    )
    .unwrap();
    stale(&r, &doc, &cursor, &token);
}

#[test]
fn rf9_a_finding_edit_makes_a_cursor_stale() {
    let (_d, mut r) = project();
    let (doc, _dec, findings, _s, token, cursor) = cursor_of(&mut r, 3, 2);
    write(
        &mut r,
        "k",
        json!([{"op": "update", "id": findings[0].to_string(), "fields": {"statement": "Changed."}}]),
    )
    .unwrap();
    stale(&r, &doc, &cursor, &token);
}

#[test]
fn rf9_an_omitted_finding_edit_makes_a_cursor_stale() {
    let (_d, mut r) = project();
    let (doc, _dec, findings, _s, token, cursor) = cursor_of(&mut r, 3, 21);
    // the 21st finding by id is the one the cap leaves out
    let mut ordered = findings.clone();
    ordered.sort();
    write(
        &mut r,
        "k",
        json!([{"op": "update", "id": ordered[20].to_string(), "fields": {"statement": "Changed."}}]),
    )
    .unwrap();
    stale(&r, &doc, &cursor, &token);
}

#[test]
fn rf9_a_source_deletion_makes_a_cursor_stale() {
    let (_d, mut r) = project();
    let (doc, _dec, _f, sources, token, cursor) = cursor_of(&mut r, 3, 2);
    write(
        &mut r,
        "k",
        json!([{"op": "delete", "id": sources[0].to_string()}]),
    )
    .unwrap();
    stale(&r, &doc, &cursor, &token);
    // the deleted source is no longer listed
    let b = r
        .read()
        .brief(&view("k"), &cover_doc(&doc), Some(1_000_000))
        .unwrap();
    assert!(b.sources.iter().all(|s| s.source != sources[0].to_string()));
    assert_eq!(b.sources.len(), 2);
}

#[test]
fn rf9_a_new_finding_dependency_makes_a_cursor_stale() {
    let (_d, mut r) = project();
    let (doc, dec, findings, _s, token, cursor) = cursor_of(&mut r, 3, 2);
    let w = write(
        &mut r,
        "k",
        json!([{"op": "create", "type": "finding", "id": "$f", "parent": doc.to_string(),
                "fields": {"statement": "A late finding.", "evidence": [{"quote": "late"}]}}]),
    )
    .unwrap();
    let late = id(&w.created["$f"]);
    let mut all: Vec<&BlockId> = findings.iter().collect();
    all.push(&late);
    link(&mut r, "k", &dec, "depends_on", &all);
    stale(&r, &doc, &cursor, &token);
    let b = r
        .read()
        .brief(&view("k"), &cover_doc(&doc), Some(1_000_000))
        .unwrap();
    assert_eq!(b.findings.len(), 3);
}

#[test]
fn rf9_a_finding_deletion_stays_refused() {
    let (_d, mut r) = project();
    let (_doc, _dec, findings, _claims, _s) = many(&mut r, 1, 2);
    let e = write(
        &mut r,
        "k",
        json!([{"op": "delete", "id": findings[0].to_string()}]),
    )
    .expect_err("a finding is not deleted");
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message
            .ends_with("is a finding: findings keep their evidence and are never deleted"),
        "{e}"
    );
}

#[test]
fn rf9_the_counts_of_a_cut_briefing_carry_sources_and_findings() {
    let (_d, mut r) = project();
    let (doc, _dec, _f, _c, _s) = many(&mut r, 5, 2);
    let b = r
        .read()
        .brief(&view("k"), &cover_doc(&doc), Some(300))
        .unwrap();
    assert!(b.truncated);
    let d = &b.docs[0];
    // five sources, each reflected by one claim of this doc; two findings, for the decision
    assert_eq!((d.sources, d.findings), (5, 2), "{:?}", b.docs);
}

#[test]
fn rf9_doc_finding_counts_cover_every_premise_past_the_cap() {
    // 21 premises of one decision: the list keeps 20 rows, the document counts all 21
    let (_d, mut r) = project();
    let (doc, _dec, _f, _c, _s) = many(&mut r, 1, 21);
    let b = r
        .read()
        .brief(&view("k"), &cover_doc(&doc), Some(1))
        .unwrap();
    assert!(b.truncated);
    assert_eq!((b.findings.len(), b.findings_omitted), (FINDINGS_CAP, 1));
    let d = &b.docs[0];
    assert_eq!(d.findings, 21, "{:?}", b.docs);
}

#[test]
fn rf9_doc_finding_counts_span_a_global_cap_over_several_docs() {
    // two docs, one decision each, 15 distinct premises each: 30 rows, the list keeps 20 and
    // omits 10, and each doc still counts its own 15
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$a", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$as", "parent": "$a", "fields": {"title": "A"}}),
        json!({"op": "create", "type": "doc", "id": "$b", "fields": {"name": "beta", "title": "Beta", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$bs", "parent": "$b", "fields": {"title": "B"}}),
        json!({"op": "create", "type": "doc", "id": "$r", "fields": {"name": "refs", "title": "Refs", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$rs", "parent": "$r", "fields": {"title": "Sources"}}),
    ];
    for i in 0..30 {
        ops.push(json!({"op": "create", "type": "finding", "id": format!("$f{i}"), "parent": "$rs",
            "fields": {"statement": format!("Finding {i}."), "evidence": [{"quote": format!("q{i}")}]}}));
    }
    let c = write(&mut r, "seed", Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    let g = |k: String| id(&c.created[&k]);
    let fs: Vec<BlockId> = (0..30).map(|i| g(format!("$f{i}"))).collect();
    let (a, b) = (g("$a".into()), g("$b".into()));
    let da = decide(
        &mut r,
        "k",
        &g("$as".into()),
        "A keeps its findings",
        &fs[..15].iter().collect::<Vec<_>>(),
    );
    let db = decide(
        &mut r,
        "k",
        &g("$bs".into()),
        "B keeps its findings",
        &fs[15..].iter().collect::<Vec<_>>(),
    );
    let out = brief(&r, &view("k"), &[&da, &db], Some(1));
    assert!(out.truncated);
    assert_eq!(
        (out.findings.len(), out.findings_omitted),
        (FINDINGS_CAP, 10)
    );
    let count = |doc: &BlockId| {
        out.docs
            .iter()
            .find(|d| d.doc == doc.to_string())
            .map(|d| d.findings)
    };
    assert_eq!(count(&a), Some(15), "{:?}", out.docs);
    assert_eq!(count(&b), Some(15), "{:?}", out.docs);
}

#[test]
fn rf9_cli_brief_uses_the_same_rows() {
    let s = Served::new();
    let (claim, src, dec, finding) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let c = write(
            &mut r,
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$sec", "parent": "$d", "fields": {"title": "Rules"}},
                {"op": "create", "type": "claim", "id": "$x", "parent": "$sec", "body": "X holds."},
                {"op": "create", "type": "source", "id": "$s", "parent": "$sec",
                    "fields": {"title": "Release\u{1b}[31m 1\u{2028}# hostile", "kind": "release", "locator": "v1"}, "body": "R1."},
                {"op": "create", "type": "finding", "id": "$f", "parent": "$sec",
                    "fields": {"statement": "Found it.\n# injected\u{85}line", "evidence": [{"quote": "q"}]}}
            ]),
        )
        .unwrap();
        land(&mut r, "seed").unwrap();
        let (claim, src, finding) = (
            id(&c.created["$x"]),
            id(&c.created["$s"]),
            id(&c.created["$f"]),
        );
        let sec = id(&c.created["$sec"]);
        link(&mut r, "work", &claim, "reflects", &[&src]);
        let dec = decide(&mut r, "work", &sec, "Keep what was found", &[&finding]);
        land(&mut r, "work").unwrap();
        r.close().unwrap();
        (claim, src, dec, finding)
    };
    let text = |ids: &str| {
        let out = s.rdy(&["brief", ids]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let t = text(&claim.to_string());
    // every control character and line separator becomes a space; ck9 (D-JQD5X): the line ends
    // with the source's existence, here `unchecked` because no checkout is linked
    let want = format!(
        "sources:\n  {src} release: Release [31m 1 # hostile ({claim}); source unchecked\n"
    );
    assert!(t.contains(&want), "{t:?}\nwant {want:?}");
    assert!(!t.contains("findings:"), "no empty heading: {t}");
    let t = text(&dec.to_string());
    let want = format!("findings:\n  {dec} depends on {finding}: Found it. # injected line\n");
    assert!(t.contains(&want), "{t:?}\nwant {want:?}");
    assert!(!t.contains("sources:"), "no empty heading: {t}");
    // the lines stay on their own line: nothing a hostile text wrote starts a line
    assert!(t.lines().all(|l| !l.starts_with("# injected")), "{t}");
    s.stop();
}
