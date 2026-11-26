//! scc3 (D-0WWYR; D-ME2HD as refined by D-JQD5X): the explicit candidate evaluator. A landing is
//! checked as a fresh propose of its approved version on the landing's `prev` would check it,
//! with the pack and gate settings the caller passes and every other input from records: the
//! same rows, in the same order, and the same policy verdict as the project's one readiness. It
//! reads no view and judges no freshness. Normal approval keeps its stored-assessment freshness
//! path and never runs the evaluator.
use crate::gate_sync_world::{land_apart, land_claim, pack, switch_pack, synced};
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

/// The sync frames of the store `s` from its start, in log order: what an initial upload brings.
fn sync_frames(s: &rdy_store::Store) -> Vec<(u8, Vec<u8>)> {
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    let mut out = vec![];
    s.reader()
        .for_each_frame_from(origin, s.log_end(), |_, raw, _, payload, _| {
            if rdy_store::sync_stream::is_sync_kind(rdy_store::codec::logical_kind(raw)) {
                out.push((raw, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

/// A first landing (no `prev`) checks in a `hub_canon` holder as it checks in its project: the
/// evaluator's empty base is the candidate's own input, not a canon read, so the holder's
/// refusal of canon reads (`holder reads need a tag or landing`) does not apply to it. An
/// ordinary canon read of a holder with no canon still refuses.
#[test]
fn scc3_first_landing_checks_in_a_holder() {
    let (_dir, mut r) = rdy_qa::fixtures::project();
    put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "parent": "$t", "body": "X holds."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let (head, landing) = head_record(&r);
    assert_eq!(landing.prev, None, "the project's first landing");
    let inputs = CheckInputs::of_state(&r.read());
    let want = check_landing(&r.read(), &landing, &inputs).unwrap();
    assert!(want.pass, "{want:#?}");
    let repo = r.store().reader().repo_id().unwrap().unwrap();
    let records = sync_frames(r.store());
    // a holder of the same repo, seeded with the project's records, then reopened
    let hd = tempfile::tempdir().unwrap();
    let mut hr = Rdy::create_holder(hd.path(), repo).unwrap();
    // before the seed the holder has no canon, and an ordinary canon read still refuses
    let e = hr.read().resolve_scope("canon").unwrap_err();
    assert_eq!(e.message, "holder reads need a tag or landing", "{e:#?}");
    let s = hr.store_mut();
    let b = s.reader().check_move_batch(records, head).unwrap();
    s.move_canon(
        &b,
        &rdy_store::hub_canon::CanonMove::Seed,
        None,
        1_763_164_800_000,
    )
    .unwrap();
    hr.close().unwrap();
    let hr = Rdy::open_holder(hd.path()).unwrap();
    // the holder's check of the first landing equals the project's
    assert_eq!(check_landing(&hr.read(), &landing, &inputs).unwrap(), want);
}

// ---- Task 3b: the pull replay ----

/// D-QCE8M, D-NME32: each replayed landing passes the gate again at its new base, with the
/// pack and gate settings the project has now: the shared evaluator runs once per replay and
/// one fresh ASSESSMENT is written per replayed landing. A landing that fails there stops the
/// replay. `--strict` and `reviews.require_up_to_date` stop at the first replay.
#[test]
fn scc3_replay_assesses_again_and_obeys_strict() {
    use rdy_engine::sync_replay::{PULLED_WITH_CONFLICTS, StopReason};
    // the gate at the new base: the rule blocks now, so the claim without `ok` stops
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let l1 = land_claim(&mut s.local, &sec, "a1", true);
    let l2 = land_claim(&mut s.local, &sec, "a2", false);
    let t = land_apart(&mut s.hub, "h1");
    let local_dir = s.dir.path().join("local");
    let reload = {
        use_pack(&local_dir, "scc3", &pack("blocking"), "");
        s.local.reload_pack().unwrap()
    };
    assert!(matches!(reload, ReloadOutcome::Reloaded), "{reload:?}");
    let (out, (_, _, checks)) = counted(|| s.pull(false));
    let from = s.fetched.unwrap();
    assert_eq!(out.status, PULLED_WITH_CONFLICTS);
    assert_eq!(out.replayed.len(), 1, "{:#?}", out.not_replayed);
    assert_eq!(out.replayed[0].source, l1.to_string());
    assert_eq!(checks, 2, "the evaluator once per landing it tried");
    assert_eq!(assessments_from(&s.local, from), 1, "one fresh assessment");
    let fresh = s
        .local
        .store()
        .reader()
        .assessment(&h(&out.replayed[0].assessment))
        .unwrap();
    assert_eq!(fresh.base, Some(t));
    let stop = &out.not_replayed[0];
    assert_eq!(
        (stop.landing.clone(), stop.reason),
        (l2.to_string(), StopReason::Gate)
    );
    assert!(
        stop.detail.as_deref().is_some_and(|d| !d.is_empty()),
        "{stop:?}"
    );

    // --strict: the first replay stops; nothing replays, everything waits in pull-1
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let l1 = land_claim(&mut s.local, &sec, "a1", true);
    let t = land_apart(&mut s.hub, "h1");
    let (out, (_, _, checks)) = counted(|| s.pull(true));
    assert_eq!(checks, 0);
    assert!(out.replayed.is_empty());
    assert_eq!(out.not_replayed[0].landing, l1.to_string());
    assert_eq!(out.not_replayed[0].reason, StopReason::Strict);
    assert_eq!(s.local.read().canon_head(), Some(t));
    assert_eq!(out.view.as_deref(), Some("pull-1"));

    // reviews.require_up_to_date: the same, with no flag
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let l1 = land_claim(&mut s.local, &sec, "a1", true);
    land_apart(&mut s.hub, "h1");
    let local_dir = s.dir.path().join("local");
    use_pack(
        &local_dir,
        "scc3",
        &pack("advisory"),
        "reviews:\n  require_up_to_date: true\n",
    );
    s.local.reload_pack().unwrap();
    let path = local_dir.clone();
    let spare = s.dir.path().join("spare");
    drop(Rdy::init(&spare, "rdy-standard").unwrap());
    drop(std::mem::replace(&mut s.local, Rdy::open(&spare).unwrap()));
    s.local = Rdy::open(&path).unwrap();
    assert!(s.local.read().require_up_to_date());
    let out = s.pull(false);
    assert!(out.replayed.is_empty());
    assert_eq!(out.not_replayed[0].landing, l1.to_string());
    assert_eq!(out.not_replayed[0].reason, StopReason::Strict);
}

/// How a `landing_of` reader of b8b16064 reads landed membership after Task 3b.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reads {
    /// Current canon's membership (`landing_of`): a carrier that left canon lands nothing, and a
    /// replay's landing lands its source.
    Canon,
    /// The carrier as the log stood at the reader's bound (`first_carrier`): history keeps its
    /// output after a canon replacement.
    History,
    /// Task 1a's fsck check of the landed rows (`check_landed`) took the call's place.
    Landed,
}

/// The 37 production `landing_of` calls of b8b16064: (file, enclosing fn, how it reads). A call
/// is named by the fn it sits in, so an edit elsewhere in the file does not move it.
const READERS: [(&str, &str, Reads); 37] = [
    ("crates/rdy-engine/src/gate.rs", "landed_as", Reads::Canon),
    ("crates/rdy-engine/src/gate.rs", "unresolved", Reads::Canon),
    (
        "crates/rdy-engine/src/gate.rs",
        "propose_relayed",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/history.rs",
        "landing_id",
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/pending/reaffirm.rs",
        "landing_of_version",
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/pending/reaffirm.rs",
        "retirement_landing",
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/pending/reaffirm.rs",
        "raised_by",
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/plan/api.rs",
        "landed_in",
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/read_state.rs",
        "inflight_set_of",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/rules/approval.rs",
        "rule_proposal",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/rules/approval.rs",
        "run_answer",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/rules/lifecycle.rs",
        "find",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/tensions/dismiss.rs",
        "canon_item_writers",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/tensions/index.rs",
        "canon_side",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        "conflict_infos",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        "landed_through",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        "include_closure",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        "view_include",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views/undo.rs",
        "landed_refusal",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views/undo.rs",
        "landed_refusal",
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views/undo.rs",
        "landed_refusal",
        Reads::Canon,
    ),
    ("crates/rdy-engine/src/views/undo.rs", "undo", Reads::Canon),
    (
        "crates/rdy-engine/src/write.rs",
        "frozen_in_conflict_hinted",
        Reads::Canon,
    ),
    (
        "crates/rdy-qa/src/invariants.rs",
        "check_views_in",
        Reads::Canon,
    ),
    (
        "crates/rdy-store/src/check.rs",
        "check_objects",
        Reads::Landed,
    ),
    ("crates/rdy-store/src/land.rs", "land_frames", Reads::Canon),
    ("crates/rdy-store/src/land.rs", "land_frames", Reads::Canon),
    ("crates/rdy-store/src/plan.rs", "landed", Reads::History),
    ("crates/rdy-store/src/plan.rs", "landed", Reads::History),
    (
        "crates/rdy-store/src/plan.rs",
        "landing_here",
        Reads::History,
    ),
    ("crates/rdy-store/src/store.rs", "producer", Reads::Canon),
    (
        "crates/rdy-store/src/store.rs",
        "producer_at",
        Reads::History,
    ),
    (
        "crates/rdy-store/src/store.rs",
        "producer_before",
        Reads::History,
    ),
    (
        "crates/rdy-store/src/store.rs",
        "producer_before",
        Reads::History,
    ),
    (
        "crates/rdy-store/src/stream_source.rs",
        "landing_here",
        Reads::History,
    ),
    ("crates/rdy-store/src/view.rs", "landing_of", Reads::Canon),
    ("crates/rdy-store/src/view.rs", "rebase_onto", Reads::Canon),
];

/// The production `landing_of` calls added after b8b16064, which read current canon too: the
/// gate fix after Task 1b (D-TVC38, D-08SPN: `through_replay` in `landed_as`), and Task 4's
/// re-chained view onto a current base, which drops its landed changes (`view_rebase_rechained`),
/// and the partial-history writer lookup, which keeps a producer only when a canon landing carries
/// it while history is incomplete (`writer_of`).
const ADDED: [(&str, &str); 3] = [
    ("crates/rdy-engine/src/gate.rs", "propose_relayed"),
    ("crates/rdy-store/src/view.rs", "view_rebase_rechained"),
    ("crates/rdy-engine/src/history.rs", "writer_of"),
];

fn workspace() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

/// The name of the fn that a `fn` line declares, if it is one.
fn fn_name(line: &str) -> Option<String> {
    let mut t = line.trim_start();
    if let Some(r) = t.strip_prefix("pub") {
        t = match r.strip_prefix('(') {
            Some(r) => r.split_once(')')?.1.trim_start(),
            None => r.trim_start(),
        };
    }
    for kw in ["async ", "const ", "unsafe "] {
        t = t.strip_prefix(kw).unwrap_or(t);
    }
    let r = t.strip_prefix("fn ")?;
    let n: String = r
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!n.is_empty()).then_some(n)
}

/// The enclosing fn of each production line of `file` that holds `call`: no definition and no
/// comment.
fn calls_in(file: &str, call: &str) -> Vec<String> {
    let text = std::fs::read_to_string(workspace().join(file)).unwrap();
    let mut cur = String::new();
    let mut out = vec![];
    for l in text.lines() {
        if let Some(n) = fn_name(l) {
            cur = n;
        }
        let t = l.trim_start();
        if t.contains(call) && !t.starts_with("//") && fn_name(l).is_none() {
            out.push(cur.clone());
        }
    }
    out
}

/// D-QCE8M reader audit (Task 3b): every b8b16064 `landing_of` reader is accounted for. The
/// history readers read the carrier as the log stood at their bound and keep their output after
/// a canon replacement; the current-canon readers read canon's membership: a carrier that left
/// canon lands nothing, and a replay's landing lands its source. No other production reader
/// exists.
#[test]
fn scc3_landing_dependencies_use_current_canon_membership() {
    // the audit table against the source: each reader sits in the fn the table names, of its kind
    for (file, name, reads) in READERS {
        let want = match reads {
            Reads::Canon => "landing_of(",
            Reads::History => "first_carrier(",
            Reads::Landed => "check_landed(",
        };
        let listed = READERS
            .iter()
            .filter(|r| r.0 == file && r.1 == name && r.2 == reads)
            .count();
        let there = calls_in(file, want).iter().filter(|n| *n == name).count();
        assert!(
            there >= listed,
            "{file}: fn {name} reads by {reads:?} and holds {there} of {listed} `{want}` calls"
        );
    }
    // no unlisted production reader of current canon
    let mut found: Vec<(String, String)> = vec![];
    for dir in [
        "crates/rdy-engine/src",
        "crates/rdy-store/src",
        "crates/rdy-qa/src",
    ] {
        let mut stack = vec![workspace().join(dir)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    // '/' on every OS, as the audit table writes it
                    let rel = p
                        .strip_prefix(workspace())
                        .unwrap()
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    for n in calls_in(&rel, "landing_of(") {
                        found.push((rel.clone(), n));
                    }
                }
            }
        }
    }
    found.sort();
    let mut want: Vec<(String, String)> = READERS
        .iter()
        .filter(|r| r.2 == Reads::Canon)
        .map(|r| (r.0.to_string(), r.1.to_string()))
        .chain(ADDED.iter().map(|(f, n)| (f.to_string(), n.to_string())))
        .collect();
    want.sort();
    assert_eq!(
        found, want,
        "a production landing_of reader the audit does not name"
    );

    // the two readings after a stopped pull: L1 replays, L2 stops and leaves canon
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let l1 = land_claim(&mut s.local, &sec, "a1", true);
    put_j(
        &mut s.local,
        "a2",
        json!([{"op": "update", "id": sec, "fields": {"title": "Local rules"}}]),
        j_typo(),
    );
    land(&mut s.local, "a2").unwrap();
    let l2 = s.local.read().canon_head().unwrap();
    put_j(
        &mut s.hub,
        "h1",
        json!([{"op": "update", "id": sec, "fields": {"title": "Hub rules"}}]),
        j_typo(),
    );
    land(&mut s.hub, "h1").unwrap();
    let out = s.pull(false);
    assert_eq!(out.replayed.len(), 1, "{out:#?}");
    let l1r = h(&out.replayed[0].landing);
    let reader = s.local.store().reader();
    let c1 = reader.landing(&l1).unwrap().changes[0];
    let c1r = reader.landing(&l1r).unwrap().changes[0];
    let c2 = reader.landing(&l2).unwrap().changes[0];
    // current canon: the replay lands its source; the stopped landing lands nothing
    assert_eq!(reader.landing_of(&c1).unwrap(), Some(l1r));
    assert_eq!(reader.landing_of(&c1r).unwrap(), Some(l1r));
    assert_eq!(reader.landing_of(&c2).unwrap(), None);
    // history: the carrier as the log stood
    assert_eq!(reader.first_carrier(&c1, None).unwrap(), Some(l1));
    assert_eq!(reader.first_carrier(&c1r, None).unwrap(), Some(l1r));
    assert_eq!(reader.first_carrier(&c2, None).unwrap(), Some(l2));
    let before_l2 = reader.kind_location(kinds::LANDING, &l2).unwrap();
    assert_eq!(reader.first_carrier(&c2, before_l2).unwrap(), None);
    // the plan and history checks read the log as it stood: the store is whole
    s.local.store().check_objects().unwrap();
    s.local.fsck().unwrap();
}
