//! tbrf1: a briefing's inflight lists the tensions on its blocks (amendment §6).
use rdy_engine::brief::{BRIEF_DEFAULT_TOKENS, Briefing, Coverage, PAGE_SECTIONS};
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
