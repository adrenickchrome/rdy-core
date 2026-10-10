//! tbrf1: a briefing's inflight lists the tensions on its blocks (amendment §6).
use rdy_engine::brief::build_every_row_for_tests;
use rdy_engine::brief::{BRIEF_DEFAULT_TOKENS, Briefing, Coverage, PAGE_SECTIONS};
use rdy_engine::tensions::info::bare_rows_built;
use rdy_engine::tensions::*;
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope, Session};
use rdy_model::{BlockId, Role};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn tbrf1_a_briefing_lists_the_tensions_on_its_blocks_and_they_are_not_in_the_token() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let cov = Coverage {
        ids: vec![s.a.to_string()],
        path: None,
    };
    let before = r.read().brief(&Scope::Canon, &cov, None).unwrap();
    assert!(before.inflight.tensions.is_empty());
    put(&mut r, "b", cite(&s.section, &s.a));
    put(&mut r, "a", upd(&s.a, "A changed."));
    let b = r.read().brief(&Scope::Canon, &cov, None).unwrap();
    assert_eq!(b.inflight.tensions.len(), 1);
    assert_eq!(b.inflight.tensions[0].kind, TensionKind::PremiseShift);
    assert_eq!(b.inflight.tensions[0].status, "open");
    assert_eq!(
        b.base.token, before.base.token,
        "tensions are not part of the token"
    );
    let t = one(&r, TensionKind::PremiseShift);
    let rev = Session::agent("agent:r".into(), Some("rr".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension_label(&t.id), "fine")
        .unwrap();
    let b = r.read().brief(&Scope::Canon, &cov, None).unwrap();
    assert_eq!(b.inflight.tensions[0].status, "dismissed");
}

#[test]
fn tbrf1_paging_has_a_tensions_section_before_the_base() {
    assert_eq!(PAGE_SECTIONS.len(), 10);
    assert_eq!(PAGE_SECTIONS[8], "inflight.tensions");
    assert_eq!(PAGE_SECTIONS[9], "base.blocks");
}

fn cov(b: &BlockId) -> Coverage {
    Coverage {
        ids: vec![b.to_string()],
        path: None,
    }
}

fn json_tokens(b: &Briefing) -> usize {
    serde_json::to_string(b).unwrap().len() / 4
}

/// Final review probe 8b (ruling on item 54): forty views edit one block; the 780 blocking edit
/// collisions lose their detail first and page, so the briefing at the default budget still
/// shows every view, as at M1.
#[test]
fn tbrf1_a_crowded_blocks_briefing_cuts_tension_detail_first_and_keeps_every_view() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    for i in 0..40 {
        put(
            &mut r,
            &format!("v{i:02}"),
            upd(&s.a, &format!("A in v{i}.")),
        );
    }
    let b = r.read().brief(&Scope::Canon, &cov(&s.a), None).unwrap();
    assert_eq!(b.inflight.views.len(), 40, "{:?}", b.omitted);
    assert!(json_tokens(&b) <= BRIEF_DEFAULT_TOKENS);
    assert!(b.inflight.tensions_detail_cut);
    assert!(!b.inflight.tensions.is_empty());
    assert!(b.inflight.tensions.iter().all(|t| t.sides.is_empty()));
    assert!(
        b.inflight
            .tensions
            .iter()
            .all(|t| t.severity == TensionSeverity::Blocking)
    );
    assert_eq!(
        b.omitted.get("inflight.tensions").copied(),
        Some(780 - b.inflight.tensions.len()),
        "the rest are on later pages"
    );
    assert!(b.next.is_some());
}

/// One advisory tension per live write, on its block (an area overlap).
struct OnWrite;
impl Detector for OnWrite {
    fn kind(&self) -> TensionKind {
        TensionKind::AreaOverlap
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() == DetectScope::Canon {
            return Ok(());
        }
        for w in cx.writes.live() {
            if seeds.is_some_and(|s| !s.contains(&w.block)) {
                continue;
            }
            out.push(Found {
                kind: TensionKind::AreaOverlap,
                involved: vec![cx.of_write(w)],
                summary: "written".into(),
                stable: true,
            });
        }
        Ok(())
    }
}
static ON_WRITE: OnWrite = OnWrite;
static MIXED: [&dyn Detector; 2] = [&collision::EditCollision, &ON_WRITE];

/// Every page of the briefing `first` starts, in order.
fn pages(r: &Rdy, first: Briefing) -> Vec<Briefing> {
    first.register(&r.briefs());
    let mut out = vec![];
    let mut next = first.next.clone();
    out.push(first);
    while let Some(cursor) = next {
        let p = r
            .read()
            .brief_page(&Scope::Canon, &r.briefs(), &cursor, None)
            .unwrap();
        next = p.next.clone();
        out.push(p);
        assert!(out.len() < 200, "paging ends");
    }
    out
}

/// Ruling on item 54 (the Q15 rule in a briefing): blocking tensions come first and are never
/// collapsed (a page that cannot hold them all pages them); advisory ones collapse to a count
/// per kind.
#[test]
fn tbrf1_advisory_tensions_collapse_to_a_count_per_kind_and_blocking_ones_page() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&MIXED);
    for i in 0..20 {
        put(
            &mut r,
            &format!("v{i:02}"),
            upd(&s.a, &format!("A in v{i}.")),
        );
    }
    let all = r.read().tensions().unwrap();
    let blocking: BTreeSet<String> = all
        .iter()
        .filter(|t| t.severity == TensionSeverity::Blocking)
        .map(|t| tension_label(&t.id))
        .collect();
    assert_eq!(blocking.len(), 190);
    let first = r.read().brief(&Scope::Canon, &cov(&s.a), None).unwrap();
    assert_eq!(first.inflight.views.len(), 20);
    assert_eq!(
        first.inflight.tensions_collapsed,
        BTreeMap::from([("area_overlap".to_string(), 20)])
    );
    assert!(
        first
            .inflight
            .tensions
            .iter()
            .all(|t| t.severity == TensionSeverity::Blocking)
    );
    let mut seen: Vec<String> = vec![];
    for p in pages(&r, first) {
        assert!(json_tokens(&p) <= BRIEF_DEFAULT_TOKENS);
        let kinds: Vec<TensionSeverity> = p.inflight.tensions.iter().map(|t| t.severity).collect();
        let mut sorted = kinds.clone();
        sorted.sort();
        assert_eq!(kinds, sorted, "blocking first");
        seen.extend(
            p.inflight
                .tensions
                .iter()
                .filter(|t| t.severity == TensionSeverity::Blocking)
                .map(|t| t.id.clone()),
        );
    }
    let distinct: BTreeSet<String> = seen.iter().cloned().collect();
    assert_eq!(seen.len(), distinct.len(), "each blocking tension once");
    assert_eq!(distinct, blocking, "no blocking tension is dropped");
}

/// Checklist line 33: an all-canon tension on a covered block is listed with its canon sides.
#[test]
fn tbrf1_an_all_canon_tension_is_listed_with_its_canon_sides() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let w = put(
        &mut r,
        "w",
        serde_json::json!([{"op": "create", "type": "claim", "id": "$y",
            "parent": s.section.to_string(), "body": "Y.",
            "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
    );
    let y = created(&w, "$y");
    land(&mut r, "w").unwrap();
    r.supersede(
        &ag("n"),
        "n",
        &s.decision.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "A new rule".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "n").unwrap();
    let b = r.read().brief(&Scope::Canon, &cov(&y), None).unwrap();
    let t = b
        .inflight
        .tensions
        .iter()
        .find(|t| t.kind == TensionKind::DeprecatedReference)
        .unwrap_or_else(|| panic!("{:#?}", b.inflight.tensions));
    assert!(!t.sides.is_empty());
    assert!(
        t.sides.iter().all(|x| x.side.starts_with("canon")),
        "{t:#?}"
    );
}

/// Checklist line 33: a read-time overlap on a covered block is listed, marked read-time.
#[test]
fn tbrf1_a_read_time_overlap_is_listed() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.intent_post(
        &ag("i"),
        Some("i"),
        None,
        &[s.a.to_string()],
        "rework A",
        Some(3_600_000),
    )
    .unwrap();
    put(&mut r, "b", upd(&s.a, "A by b."));
    let b = r.read().brief(&Scope::Canon, &cov(&s.a), None).unwrap();
    let t: Vec<_> = b.inflight.tensions.iter().filter(|t| t.read_time).collect();
    assert_eq!(t.len(), 1, "{:#?}", b.inflight.tensions);
    assert_eq!(t[0].kind, TensionKind::AreaOverlap);
    assert!(
        t[0].sides.iter().any(|x| x.side.starts_with("intent:in-")),
        "{:#?}",
        t[0]
    );
}

/// Perf fix (mcp_write_9mib): a briefing over blocks with thousands of tensions builds each
/// row once and measures it a bounded number of times, whatever it cuts.
#[test]
fn tbrf1_a_briefing_over_thousands_of_tensions_builds_each_row_once() {
    use rdy_engine::tensions::info::info_counts;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // 2,000 pairs of terms with one name each: 2,000 duplicate groups (FW-TN2)
    let mut ops = vec![];
    for i in 0..4000 {
        ops.push(
            serde_json::json!({"op": "create", "type": "term", "id": format!("$f{i}"),
            "parent": s.section.to_string(), "fields": {"name": format!("fee {}", i / 2)},
            "body": format!("Fee number {i} is a term.")}),
        );
    }
    let w = put(&mut r, "b", serde_json::Value::Array(ops));
    let ids: Vec<String> = (0..4000)
        .map(|i| created(&w, &format!("$f{i}")).to_string())
        .collect();
    let cov = Coverage { ids, path: None };
    let st = r.read();
    st.brief(&Scope::View("b".into()), &cov, None).unwrap();
    let (f0, (s0, b0)) = (st.reader().frames_read(), info_counts());
    let b = st.brief(&Scope::View("b".into()), &cov, None).unwrap();
    let (s1, b1) = info_counts();
    let frames = st.reader().frames_read() - f0;
    assert!(
        frames <= 4,
        "{frames} frames: each change's provenance is read once"
    );
    assert!(json_tokens(&b) <= BRIEF_DEFAULT_TOKENS);
    let all = b.inflight.tensions.len() + b.omitted.get("inflight.tensions").copied().unwrap_or(0);
    assert_eq!(all, 2000);
    // perf fix (pre-merge): the detail cut drops every side, so no side is read (each row was
    // built with its sides once before)
    assert_eq!(b1 - b0, 0, "the detail cut drops every side: none is read");
    assert!(s1 - s0 <= 6 * 2000, "{} serializations", s1 - s0);
}

/// Perf fix (pre-merge, brief_doc): a briefing after commits on a read set carries it and never
/// builds the whole set; a briefing that lists its tensions in full still shows their sides.
#[test]
fn tbrf1_a_briefing_after_commits_builds_no_whole_tension_set() {
    use rdy_engine::tensions::set::full_builds;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A one."));
    put(&mut r, "b", upd(&s.a, "A two."));
    r.read().brief(&Scope::Canon, &cov(&s.a), None).unwrap();
    put(&mut r, "b", upd(&s.b, "B two."));
    put(&mut r, "c", upd(&s.c, "C two."));
    let before = full_builds();
    let b = r.read().brief(&Scope::Canon, &cov(&s.a), None).unwrap();
    assert_eq!(full_builds(), before, "no whole set was built");
    assert_eq!(b.inflight.tensions.len(), 1, "{:?}", b.inflight.tensions);
    assert_eq!(b.inflight.tensions[0].kind, TensionKind::EditCollision);
    assert_eq!(
        b.inflight.tensions[0].sides.len(),
        2,
        "uncut: the sides are shown"
    );
    assert!(!b.inflight.tensions_detail_cut);
}

/// Every page at `max`, from the first, as JSON: rows built on demand, or every row built
/// before the page measures (`every_row`, the 1d0c1da behaviour).
fn pages_at(r: &Rdy, c: &Coverage, max: usize, every_row: bool) -> Vec<serde_json::Value> {
    build_every_row_for_tests(every_row);
    let st = r.read();
    let first = st.brief(&Scope::Canon, c, Some(max)).unwrap();
    first.register(&r.briefs());
    let mut next = first.next.clone();
    let mut out = vec![serde_json::to_value(&first).unwrap()];
    while let Some(cursor) = next {
        let p = st
            .brief_page(&Scope::Canon, &r.briefs(), &cursor, Some(max))
            .unwrap();
        next = p.next.clone();
        out.push(serde_json::to_value(&p).unwrap());
        assert!(out.len() < 500, "paging ends");
    }
    build_every_row_for_tests(false);
    out
}

/// Perf (brief_doc_50k): building rows on demand changes no page. 190 blocking edit collisions
/// and 20 advisory overlaps, paged at budgets from tiny to uncut.
#[test]
fn hd1_rows_built_on_demand_give_the_same_briefing_at_every_budget() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&MIXED);
    for i in 0..20 {
        put(
            &mut r,
            &format!("v{i:02}"),
            upd(&s.a, &format!("A in v{i}.")),
        );
    }
    for max in [60, 300, 1_000, 2_500, BRIEF_DEFAULT_TOKENS, 20_000, 400_000] {
        let lazy = pages_at(&r, &cov(&s.a), max, false);
        let eager = pages_at(&r, &cov(&s.a), max, true);
        assert_eq!(lazy.len(), eager.len(), "max {max}: page count");
        for (i, (l, e)) in lazy.iter().zip(&eager).enumerate() {
            assert_eq!(l, e, "max {max}, page {i}");
        }
    }
}

/// `n` terms under `s`'s section in view `b`, in name pairs (`fee 0`, `fee 0`, `fee 1`, ...):
/// each pair is one advisory duplicate row. Their ids, in creation order.
fn fee_pairs(r: &mut Rdy, s: &rdy_qa::edit_loop::Spec, n: usize) -> Vec<String> {
    let mut ops = vec![];
    for i in 0..n {
        ops.push(
            serde_json::json!({"op": "create", "type": "term", "id": format!("$f{i}"),
            "parent": s.section.to_string(), "fields": {"name": format!("fee {}", i / 2)},
            "body": format!("Fee number {i} is a term.")}),
        );
    }
    let w = put(r, "b", serde_json::Value::Array(ops));
    (0..n)
        .map(|i| created(&w, &format!("$f{i}")).to_string())
        .collect()
}

/// What one steady-state brief of `c` in view `b` costs at the default budget: (rows shown,
/// rows in all, rows built, row serializations). The first brief builds the tension set.
fn cut_cost(r: &Rdy, c: &Coverage) -> (u64, u64, u64, u64) {
    use rdy_engine::tensions::info::info_counts;
    use rdy_engine::tensions::set::full_builds;
    let scope = Scope::View("b".into());
    let st = r.read();
    st.brief(&scope, c, None).unwrap(); // builds the tension set once
    let (rows0, (s0, b0), f0) = (bare_rows_built(), info_counts(), full_builds());
    let b = st.brief(&scope, c, None).unwrap();
    let shown = b.inflight.tensions.len() as u64;
    let all = shown + b.omitted.get("inflight.tensions").copied().unwrap_or(0) as u64;
    let built = bare_rows_built() - rows0;
    let (s1, b1) = info_counts();
    assert_eq!(b1 - b0, 0, "the detail cut reads no side");
    assert_eq!(full_builds(), f0, "no whole tension set is built");
    (shown, all, built, s1 - s0)
}

/// Perf (brief_doc_50k): a cut briefing builds and measures the rows it can show, not every
/// row that involves its blocks. 400 terms in 200 name pairs give 200 advisory duplicate rows.
/// The 400 covered blocks alone put the briefing over the budget, so no row is shown.
#[test]
fn hd1_a_cut_briefing_builds_only_the_rows_it_can_show() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ids = fee_pairs(&mut r, &s, 400);
    let (shown, all, built, serialized) = cut_cost(&r, &Coverage { ids, path: None });
    assert_eq!(all, 200, "every duplicate pair is counted");
    assert!(shown < all, "the page is cut: {shown} of {all}");
    assert!(built <= shown + 1, "{built} rows built for {shown} shown");
    assert!(
        serialized <= 6 * (shown + 1),
        "{serialized} row serializations for {shown} shown"
    );
}

/// Perf (brief_doc_50k, the bench's setup): the briefing without its rows fits the budget, so
/// the page shows some rows and collapses the rest. 100 name pairs, one term of each pair
/// covered: 100 advisory duplicate rows, of which about a fifth fit. The rows built follow the
/// rows shown, with a small margin: the first measure comes before the per-doc counts, the
/// cursor and the cut's own counts join the briefing, and the last row built is the one that
/// crosses the budget, so the cut collapses a few built rows (2 of 23 built at this commit).
#[test]
fn hd1_a_cut_briefing_that_shows_rows_builds_about_the_rows_it_shows() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ids = fee_pairs(&mut r, &s, 200);
    let covered = ids.into_iter().step_by(2).collect();
    let (shown, all, built, serialized) = cut_cost(
        &r,
        &Coverage {
            ids: covered,
            path: None,
        },
    );
    assert_eq!(all, 100, "every duplicate pair is counted");
    assert!(shown > 0, "the page shows rows: {shown} of {all}");
    assert!(shown < all, "the page is cut: {shown} of {all}");
    assert!(built <= shown + 3, "{built} rows built for {shown} shown");
    assert!(
        serialized <= shown + 3,
        "{serialized} row serializations for {shown} shown"
    );
}

/// A budget so large that `max * 4` wraps (an engine caller may pass any `usize`) is no cut:
/// every row is shown, and the briefing is the uncut one.
#[test]
fn hd1_a_budget_past_the_byte_range_shows_every_row() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ids = fee_pairs(&mut r, &s, 20);
    let c = Coverage { ids, path: None };
    let scope = Scope::View("b".into());
    let st = r.read();
    let uncut = serde_json::to_value(st.brief(&scope, &c, Some(1_000_000)).unwrap()).unwrap();
    for max in [1usize << 62, usize::MAX / 2, usize::MAX] {
        let b = st.brief(&scope, &c, Some(max)).unwrap();
        assert_eq!(b.inflight.tensions.len(), 10, "max {max}: every row");
        assert!(!b.truncated, "max {max}: not cut");
        assert!(b.omitted.is_empty(), "max {max}: {:?}", b.omitted);
        assert_eq!(serde_json::to_value(&b).unwrap(), uncut, "max {max}");
    }
}
