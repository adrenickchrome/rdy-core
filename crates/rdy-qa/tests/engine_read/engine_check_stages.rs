//! ck5 (D-JQD5X "Stages"; D-DSPMW, D-AJ0XQ): one registry of the deterministic checks, each
//! tagged with its stages, and no agent check in any stage. Each act runs its built-in checks
//! once. The reach the pack rules add is bounded by the assessment's existing bound.
use rdy_engine::check::rules::rule_scans_for_tests;
use rdy_engine::check::{AGENT_CHECKS, AgentCheck, DeterministicCheck, STAGE_CHECKS, Stage};
use rdy_engine::pending::{Fact, Item, ItemKind};
use rdy_engine::plan::lifecycle::PlanSummary;
use rdy_engine::readiness::assessment::{GateConfig, Inputs, build};
use rdy_engine::readiness::readiness_calls_for_tests;
use rdy_engine::{CoverageGate, E_VALIDATION, ProposalRef, Rdy};
use rdy_model::assessment::{
    AssessmentConfidence, AssessmentCoverage, AssessmentLevel, AssessmentScoreChange,
    AssessmentScores, MAX_ROWS,
};
use rdy_model::{BlockId, CROCKFORD, Hash};
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_schema::{PackSet, Severity};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn ck5_stages_have_only_deterministic_entries() {
    use Stage::{Change, Land, Propose};
    // the exact registry, in its order
    let rows: Vec<(&str, Vec<Stage>)> = STAGE_CHECKS
        .iter()
        .map(|c| (c.name, c.stages.to_vec()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("tension_delta", vec![Change]),
            ("pack_rules", vec![Change, Propose]),
            ("readiness", vec![Propose]),
            ("suspects", vec![Land]),
            ("events", vec![Land]),
            ("rebases", vec![Land]),
        ]
    );
    assert_eq!(
        DeterministicCheck::ALL
            .iter()
            .map(|c| c.row().name)
            .collect::<Vec<_>>(),
        STAGE_CHECKS.iter().map(|c| c.name).collect::<Vec<_>>()
    );
    assert_eq!(
        DeterministicCheck::at(Change).collect::<Vec<_>>(),
        vec![
            DeterministicCheck::TensionDelta,
            DeterministicCheck::PackRules
        ]
    );
    assert_eq!(
        DeterministicCheck::at(Propose).collect::<Vec<_>>(),
        vec![DeterministicCheck::PackRules, DeterministicCheck::Readiness]
    );
    // no agent check is a registry row: the registry type has no variant for one, and no name
    let registered: BTreeSet<&str> = STAGE_CHECKS.iter().map(|c| c.name).collect();
    for check in AgentCheck::ALL {
        assert!(
            !registered.contains(check.spec().name),
            "{} is in a stage",
            check.spec().name
        );
    }
    assert_eq!(AGENT_CHECKS.len(), 8);
    // rules never run at land: a caller cannot dispatch anything through that stage
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let (_d, r) = project();
    let st = r.read();
    let m = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    let g = st.graph(&rdy_engine::Scope::Canon).unwrap();
    let empty = BTreeSet::new();
    let none = BTreeMap::new();
    let inputs = rdy_engine::filter::FilterInputs {
        pack: &pack,
        m: &m,
        graph: &g,
        areas: st.area_matcher(),
        suspects: &empty,
        tensions: &empty,
        coverage: &none,
    };
    let e = rdy_engine::check::evaluate_rules(&pack, inputs, std::iter::empty(), Land).unwrap_err();
    assert_eq!(e.code, rdy_engine::E_INTERNAL);
    drop(st);
    // one built-in run per act: a write scans the rules once, a propose runs readiness once and
    // its rules once
    let (d, r) = project();
    drop(r);
    rdy_qa::packs::use_pack(
        d.path(),
        "one",
        "name: one\nversion: \"1\"\nextends: rdy-standard\nrules:\n  - name: any\n    select: type = claim\n    require: type = claim\n    message: m\n    fix: f\n",
        "",
    );
    let mut r = Rdy::open(d.path()).unwrap();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let e = std::sync::Arc::new(rdy_engine::engine::Local::new(r));
    let scans = rule_scans_for_tests();
    rdy_engine::consequence::write_and_report(&*e, "v", None, move |r| {
        write(
            r,
            "v",
            json!([{"op": "create", "type": "claim", "parent": section, "body": "two"}]),
        )
    })
    .unwrap();
    assert_eq!(rule_scans_for_tests() - scans, 1, "a write scans once");
    let (scans, calls) = (rule_scans_for_tests(), readiness_calls_for_tests());
    rdy_qa::inbox::on(&e, |r| {
        r.propose(&writer(), "v", "v", Some(TEST_SUMMARY)).unwrap()
    });
    assert_eq!(
        readiness_calls_for_tests() - calls,
        1,
        "a propose runs readiness once"
    );
    assert_eq!(rule_scans_for_tests() - scans, 1, "and its rules once");
}

fn h(n: u32) -> Hash {
    Hash::of(&n.to_le_bytes())
}

/// The `n`th valid block id with prefix `p`, in order.
fn nth(p: &str, mut n: usize) -> BlockId {
    let mut body = [b'0'; 5];
    for c in body.iter_mut().rev() {
        *c = CROCKFORD[n % 32];
        n /= 32;
    }
    BlockId::parse(&format!("{p}-{}", std::str::from_utf8(&body).unwrap())).unwrap()
}

fn scores() -> AssessmentScores {
    AssessmentScores {
        overall: 100,
        grounded: 100,
        consistent: 100,
        current: 100,
        connected: 100,
        complete: 100,
        governed: 100,
    }
}

fn card() -> Item {
    Item {
        r#ref: "P-7f3a@2".to_string(),
        kind: ItemKind::Approve,
        title: "Bound".to_string(),
        why: "ready to land: 1 changes".to_string(),
        blocking: false,
        summary: Some("A bound".to_string()),
        summary_by: None,
        facts: vec![Fact {
            name: "Changes".into(),
            value: "1 doc, 1 block".to_string(),
        }],
        members: vec![],
        options: vec![],
        raised_by: "agent:a".to_string(),
        raised_at: "2026-10-30T00:00:00Z".to_string(),
        age: "today".to_string(),
        batch: None,
        exception: None,
        card: String::new(),
        card_hash: String::new(),
        removal: None,
        removal_reports: BTreeMap::new(),
        plan_binding: None,
        check_verdicts: vec![],
    }
}

#[test]
fn ck5_rule_reach_limit_is_the_existing_assessment_bound() {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let plan = PlanSummary::default();
    let item = card();
    let pref = ProposalRef {
        version: h(2),
        stable: h(1),
        n: 1,
    };
    // the existing reach: 10 written blocks and 5 plan blocks; the rules add the rest
    let inputs = |rule_reach: BTreeSet<BlockId>, written: BTreeSet<BlockId>| Inputs {
        pref,
        base: None,
        pack: &pack,
        gate: GateConfig {
            require_up_to_date: false,
            coverage_gate: CoverageGate::Off,
        },
        changes: 1,
        consequences: 0,
        plan: &plan,
        plan_hash: None,
        supersedes: vec![],
        reaffirms: vec![],
        reversibility: vec![],
        exceptions: vec![],
        tensions: &[],
        tensions_omitted: 0,
        answers: &[],
        carried: &[],
        suspects: vec![],
        written,
        plan_blocks: (0..5).map(|n| nth("p", n)).collect(),
        rule_reach,
        scope_blocks: BTreeSet::new(),
        coverage: AssessmentCoverage {
            pages: vec![],
            corpus: AssessmentScoreChange {
                before: scores(),
                after: scores(),
            },
        },
        confidence: AssessmentConfidence {
            level: AssessmentLevel::High,
            reasons: vec![],
        },
        issues: vec![],
        card: &item,
        policy: None,
    };
    let written: BTreeSet<BlockId> = (0..10).map(|n| nth("w", n)).collect();
    // the rules repeat the existing reach (counted once) and add blocks up to the bound
    let extension = |extra: usize| -> BTreeSet<BlockId> {
        written
            .iter()
            .cloned()
            .chain((0..extra).map(|n| nth("r", n)))
            .collect()
    };
    let a = build(inputs(extension(MAX_ROWS - 15), written.clone())).unwrap();
    assert_eq!(a.reach.len(), MAX_ROWS, "100,000 blocks pass");
    let e = build(inputs(extension(MAX_ROWS - 14), written.clone())).unwrap_err();
    assert_eq!(e.severity, Severity::Error);
    assert_eq!(e.code, E_VALIDATION);
    assert_eq!(e.block, None);
    assert_eq!(e.message, format!("{pref} is too large to assess"));
    assert_eq!(e.fix, "split the work into smaller proposals");
    // a reach over the bound before the rules keeps its own issue
    let over: BTreeSet<BlockId> = (0..MAX_ROWS - 4).map(|n| nth("w", n)).collect();
    let e = build(inputs(BTreeSet::new(), over)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{pref} reaches more than {MAX_ROWS} blocks")
    );
    // with no rules the reach is the existing one, unchanged
    let a = build(inputs(BTreeSet::new(), written.clone())).unwrap();
    let mut want: BTreeSet<BlockId> = written.clone();
    want.extend((0..5).map(|n| nth("p", n)));
    assert_eq!(a.reach, want.into_iter().collect::<Vec<_>>());
}
