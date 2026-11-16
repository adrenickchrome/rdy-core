//! scc3 (D-0WWYR; D-ME2HD as refined by D-JQD5X): the explicit candidate evaluator. A landing is
//! checked as a fresh propose of its approved version on the landing's `prev` would check it,
//! with the pack and gate settings the caller passes and every other input from records: the
//! same rows, in the same order, and the same policy verdict as the project's one readiness. It
//! reads no view and judges no freshness. Normal approval keeps its stored-assessment freshness
//! path and never runs the evaluator.
use rdy_engine::landing_check::{
    CheckInputs, LandingCheck, check_landing, landing_checks_for_tests,
};
use rdy_engine::readiness::{
    Candidate, assess, candidate_builds_for_tests, readiness, readiness_calls_for_tests,
};
use rdy_engine::{E_RULE_CHECK, LandInfo, ProposalInfo, Rdy, RdyError, ReloadOutcome};
use rdy_model::{BlockId, Hash, LandingRecord};
use rdy_qa::fixtures::{approver, land};
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items};
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{ag, j_typo, put_j, upd};
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::json;

/// A pack with one rule on claims, `ok-claims`, of `severity`.
fn pack(severity: &str) -> String {
    format!(
        "\
name: scc3
version: \"1\"
extends: rdy-standard
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      ok: {{ kind: bool }}
rules:
  - name: ok-claims
    select: type = claim
    require: fields.ok = true
    severity: {severity}
    message: ok-claims message
    fix: ok-claims fix
"
    )
}

/// Points the project at `pack(severity)` and reloads it.
fn switch_pack(dir: &std::path::Path, r: &mut Rdy, severity: &str) {
    use_pack(dir, "scc3", &pack(severity), "");
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    answer_open_items(r, &ag(view), view);
    r.propose(&ag(view), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"))
}

/// The landing record of canon's head.
fn head_record(r: &Rdy) -> (Hash, LandingRecord) {
    let st = r.read();
    let head = st.canon_head().expect("a landing");
    (head, st.reader().landing(&head).unwrap())
}

/// The project's one readiness of version `id` on the published state, as a propose runs it:
/// the rows and the policy verdict.
fn project_readiness(r: &Rdy, id: &str) -> (Vec<rdy_schema::Issue>, Option<LandingPolicy>) {
    let st = r.read();
    let pref = r.resolve_proposal(id).unwrap();
    let p = st.reader().proposal(&h(id)).unwrap();
    let c = Candidate::stored(&st, pref, p).unwrap();
    let a = assess(&st, &c, &st.gate_config()).unwrap();
    // the one readiness gives the same rows
    assert_eq!(readiness(&st, &c).unwrap().issues, a.readiness.issues);
    (a.readiness.issues, a.readiness.policy)
}

type LandingPolicy = rdy_model::assessment::AssessedPolicy;

#[test]
fn scc3_explicit_candidate_matches_project_readiness() {
    let (dir, r) = rdy_qa::fixtures::project();
    drop(r);
    use_pack(dir.path(), "scc3", &pack("advisory"), "");
    let mut r = Rdy::open(dir.path()).unwrap();
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$st", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$st", "fields": {"title": "Rules"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let ss = seeded.created["$ss"].clone();
    // view a puts a claim that fails the rule into stable content
    let a = put_j(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": ss, "body": "C holds.", "fields": {"ok": false}}]),
        j_typo(),
    );
    let c = BlockId::parse(&a.created["$c"]).unwrap();
    let p = propose(&mut r, "a");
    assert!(p.ready, "{:#?}", p.issues);
    // the project's readiness under the advisory rule: ready, with a policy verdict
    let (adv_rows, adv_policy) = project_readiness(&r, &p.id);
    assert!(adv_rows.is_empty(), "{adv_rows:#?}");
    assert!(adv_policy.is_some());
    let adv = CheckInputs::of_state(&r.read());
    // the project's readiness under the blocking rule: one E_RULE_CHECK row on the claim
    switch_pack(dir.path(), &mut r, "blocking");
    let (blk_rows, blk_policy) = project_readiness(&r, &p.id);
    assert_eq!(
        blk_rows
            .iter()
            .map(|i| (i.code, i.block.clone()))
            .collect::<Vec<_>>(),
        vec![(E_RULE_CHECK, Some(c.clone()))],
        "{blk_rows:#?}"
    );
    assert!(blk_policy.is_none());
    let blk = CheckInputs::of_state(&r.read());
    // back to the advisory pack: the stored assessment is fresh, so the version lands
    switch_pack(dir.path(), &mut r, "advisory");
    r.land_report(&approver(), &p.id).expect("p lands");
    let (_, landing) = head_record(&r);
    assert_eq!(landing.approved_version, Some(h(&p.id)));
    let expect =
        |rows: &[rdy_schema::Issue], policy: &Option<LandingPolicy>, matches: bool| LandingCheck {
            pass: rows.is_empty(),
            rows: rows.to_vec(),
            policy: policy.clone(),
            assessment_matches: matches,
        };
    let want_adv = expect(&adv_rows, &adv_policy, true);
    let want_blk = expect(&blk_rows, &blk_policy, false);
    let check = |r: &Rdy, inputs: &CheckInputs| check_landing(&r.read(), &landing, inputs).unwrap();
    // the landing in the store: the explicit candidate gives the project's rows and verdict
    assert_eq!(check(&r, &adv), want_adv);
    assert_eq!(check(&r, &blk), want_blk);
    // a captured copy of the record gives the same
    let captured = landing.clone();
    assert_eq!(check_landing(&r.read(), &captured, &blk).unwrap(), want_blk);
    // work in flight on the claim and a later landing over it change nothing: the check reads
    // no view and judges on the landing's own prev, never freshness
    put_j(&mut r, "later", upd(&c, "C holds now."), j_typo());
    assert_eq!(check(&r, &adv), want_adv);
    assert_eq!(check(&r, &blk), want_blk);
    land(&mut r, "later").unwrap();
    assert_eq!(check(&r, &adv), want_adv);
    assert_eq!(check(&r, &blk), want_blk);
}

/// Canon: a draft doc whose claim `y` references claim `x`.
fn draft_world() -> (tempfile::TempDir, Rdy, BlockId) {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$t", "body": "Y builds on {{$x}}."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    (dir, r, BlockId::parse(&w.created["$x"]).unwrap())
}

/// The ASSESSMENT frames written since `from`.
fn assessments_from(r: &Rdy, from: Location) -> usize {
    let mut n = 0;
    r.store()
        .reader()
        .for_each_frame_from(from, r.store().log_end(), |_, kind, _, _, _| {
            if rdy_store::codec::logical_kind(kind) == kinds::ASSESSMENT {
                n += 1;
            }
            Ok(())
        })
        .unwrap();
    n
}

/// `f`, with the readiness runs, candidate builds and landing checks it made.
fn counted<T>(f: impl FnOnce() -> T) -> (T, (u64, u64, u64)) {
    let before = (
        readiness_calls_for_tests(),
        candidate_builds_for_tests(),
        landing_checks_for_tests(),
    );
    let out = f();
    (
        out,
        (
            readiness_calls_for_tests() - before.0,
            candidate_builds_for_tests() - before.1,
            landing_checks_for_tests() - before.2,
        ),
    )
}

#[test]
fn scc3_normal_approval_keeps_freshness_only() {
    let (_dir, mut r, x) = draft_world();
    put_j(&mut r, "a", upd(&x, "X holds twice."), j_typo());
    let p = propose(&mut r, "a");
    assert!(p.ready, "{:#?}", p.issues);
    let assessed = p.assessment.clone().expect("a stored assessment");
    // an unrelated landing moves canon past the version's base, outside its reach
    put_j(
        &mut r,
        "u",
        json!([{"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}}]),
        j_typo(),
    );
    let unrelated: Hash = land(&mut r, "u").unwrap().parse().unwrap();
    // the approval checks only freshness against the stored assessment: no readiness run, no
    // candidate build, no explicit check, and no new assessment
    let from = r.store().log_end();
    let (res, runs): (Result<LandInfo, RdyError>, _) =
        counted(|| r.land_report(&approver(), &p.id));
    res.expect("a landing outside reach lands");
    assert_eq!(runs, (0, 0, 0));
    assert_eq!(assessments_from(&r, from), 0);
    let (_, landing) = head_record(&r);
    assert_eq!(landing.prev, Some(unrelated));
    assert_eq!(landing.assessment.map(|a| a.to_string()), Some(assessed));
    // the explicit check of that landing runs the one readiness once, on the landing's prev,
    // and finds the stored assessment's pack and settings
    let inputs = CheckInputs::of_state(&r.read());
    let (check, runs) = counted(|| check_landing(&r.read(), &landing, &inputs).unwrap());
    assert_eq!((runs.0, runs.2), (1, 1));
    assert!(check.pass, "{check:#?}");
    assert!(check.rows.is_empty());
    assert!(check.policy.is_some());
    assert!(check.assessment_matches);
    // another gate setting: the same rows, and the stored assessment no longer matches
    let strict = CheckInputs {
        require_up_to_date: true,
        ..inputs
    };
    let other = check_landing(&r.read(), &landing, &strict).unwrap();
    assert_eq!(
        other,
        LandingCheck {
            assessment_matches: false,
            ..check
        }
    );
}
