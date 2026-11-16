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

const URL: &str = "https://127.0.0.1:9/r/demo";

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// The hub's copy and the local project after one shared landing that holds a section `s`, on
/// pack `pack(severity)`, in a stable doc; the local project last synced at that landing.
struct Synced {
    dir: tempfile::TempDir,
    hub: Rdy,
    local: Rdy,
    base: Hash,
    section: String,
    /// The log end after the last pull's fetch: where its second batch starts.
    fetched: Option<Location>,
}

fn synced(severity: &str) -> Synced {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hub");
    drop(Rdy::init(&a, "rdy-standard").unwrap());
    use_pack(&a, "scc3", &pack(severity), "");
    let mut hub = Rdy::open(&a).unwrap();
    let w = put_j(
        &mut hub,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Rules"}}
        ]),
        j_typo(),
    );
    land(&mut hub, "seed").unwrap();
    let base = hub.read().canon_head().unwrap();
    drop(hub);
    let b = dir.path().join("local");
    copy_dir(&a, &b);
    let mut local = Rdy::open(&b).unwrap();
    let repo = local.store().reader().repo_id().unwrap().unwrap();
    local
        .store_mut()
        .write_sync(
            "origin",
            vec![rdy_store::sync_state::SyncStep::Canon(
                rdy_store::sync_state::SyncCanon {
                    repo_id: repo,
                    url: URL.into(),
                    fetched: Some(base),
                    applied: Some(base),
                    held: None,
                },
            )],
        )
        .unwrap();
    Synced {
        hub: Rdy::open(&a).unwrap(),
        local,
        base,
        section: w.created["$s"].clone(),
        dir,
        fetched: None,
    }
}

/// Writes a claim (`ok` when `ok`) under the section in view `view` of `r` and lands it.
fn land_claim(r: &mut Rdy, section: &str, view: &str, ok: bool) -> Hash {
    let fields = json!({ "ok": ok });
    put_j(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": section, "body": format!("{view} holds."), "fields": fields}]),
        j_typo(),
    );
    land(r, view).unwrap();
    r.read().canon_head().unwrap()
}

/// Lands a new draft doc named `name` in view `name` of `r`: a hub landing apart from the
/// stable section, so it opens no change-plan item for a replayed claim.
fn land_apart(r: &mut Rdy, name: &str) -> Hash {
    put_j(
        r,
        name,
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "overview"}}]),
        j_typo(),
    );
    land(r, name).unwrap();
    r.read().canon_head().unwrap()
}

impl Synced {
    /// Fetches the hub's canon and runs the pull's second batch.
    fn pull(&mut self, strict: bool) -> rdy_engine::sync_replay::PullOutcome {
        use rdy_engine::sync_fetch::HubState;
        let tip = self.hub.read().canon_head().unwrap();
        let mut out = Vec::new();
        rdy_engine::stream_export::write_history(
            &self.hub.read(),
            rdy_store::sync_stream::StreamPurpose::CanonHistory,
            tip,
            Some(self.base),
            &std::collections::BTreeSet::new(),
            &mut out,
        )
        .unwrap();
        let path = self.dir.path().join("batch");
        std::fs::write(&path, out).unwrap();
        let target = rdy_store::take_batch::FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: rdy_store::sync_stream::StreamPurpose::CanonHistory,
            root: tip,
            view: None,
        };
        rdy_engine::sync_fetch::fetch(
            &mut self.local,
            &[std::fs::File::open(&path).unwrap()],
            &target,
        )
        .unwrap();
        self.fetched = Some(self.local.store().log_end());
        let repo = self.hub.store().reader().repo_id().unwrap();
        rdy_engine::sync_replay::pull(
            &mut self.local,
            &approver(),
            &rdy_engine::sync_replay::PullRequest {
                remote: "origin".into(),
                hub: HubState {
                    repo_id: repo,
                    canon: Some(tip),
                },
                take: false,
                strict,
            },
        )
        .unwrap()
    }
}

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

/// The 37 production `landing_of` calls of b8b16064: (file, b8b16064 line, S line, line now,
/// how it reads). S is the execution base b4fde86e.
const READERS: [(&str, u32, u32, u32, Reads); 37] = [
    ("crates/rdy-engine/src/gate.rs", 665, 665, 670, Reads::Canon),
    ("crates/rdy-engine/src/gate.rs", 906, 906, 921, Reads::Canon),
    (
        "crates/rdy-engine/src/gate.rs",
        2695,
        2695,
        2728,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/history.rs",
        266,
        266,
        270,
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/pending/reaffirm.rs",
        269,
        269,
        269,
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/pending/reaffirm.rs",
        392,
        392,
        392,
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/pending/reaffirm.rs",
        415,
        415,
        415,
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/plan/api.rs",
        560,
        560,
        560,
        Reads::History,
    ),
    (
        "crates/rdy-engine/src/read_state.rs",
        489,
        489,
        489,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/rules/approval.rs",
        501,
        501,
        501,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/rules/approval.rs",
        915,
        915,
        915,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/rules/lifecycle.rs",
        543,
        543,
        543,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/tensions/dismiss.rs",
        111,
        111,
        111,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/tensions/index.rs",
        1362,
        1362,
        1362,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        243,
        243,
        243,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        423,
        423,
        423,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        670,
        670,
        670,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views.rs",
        847,
        847,
        847,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views/undo.rs",
        122,
        122,
        122,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views/undo.rs",
        135,
        135,
        135,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views/undo.rs",
        158,
        158,
        158,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/views/undo.rs",
        440,
        440,
        440,
        Reads::Canon,
    ),
    (
        "crates/rdy-engine/src/write.rs",
        1345,
        1345,
        1345,
        Reads::Canon,
    ),
    (
        "crates/rdy-qa/src/invariants.rs",
        493,
        491,
        491,
        Reads::Canon,
    ),
    (
        "crates/rdy-store/src/check.rs",
        135,
        135,
        140,
        Reads::Landed,
    ),
    ("crates/rdy-store/src/land.rs", 438, 438, 438, Reads::Canon),
    ("crates/rdy-store/src/land.rs", 642, 664, 664, Reads::Canon),
    (
        "crates/rdy-store/src/plan.rs",
        925,
        925,
        926,
        Reads::History,
    ),
    (
        "crates/rdy-store/src/plan.rs",
        1229,
        1229,
        1234,
        Reads::History,
    ),
    (
        "crates/rdy-store/src/plan.rs",
        1264,
        1264,
        1263,
        Reads::History,
    ),
    (
        "crates/rdy-store/src/store.rs",
        4222,
        4426,
        4514,
        Reads::Canon,
    ),
    (
        "crates/rdy-store/src/store.rs",
        4372,
        4576,
        4664,
        Reads::History,
    ),
    (
        "crates/rdy-store/src/store.rs",
        4406,
        4610,
        4698,
        Reads::History,
    ),
    (
        "crates/rdy-store/src/store.rs",
        4412,
        4616,
        4704,
        Reads::History,
    ),
    (
        "crates/rdy-store/src/stream_source.rs",
        635,
        687,
        712,
        Reads::History,
    ),
    ("crates/rdy-store/src/view.rs", 445, 445, 445, Reads::Canon),
    ("crates/rdy-store/src/view.rs", 731, 731, 731, Reads::Canon),
];

/// The production `landing_of` call the gate fix after Task 1b added (D-TVC38, D-08SPN:
/// `through_replay` in `landed_as`), which reads current canon too.
const ADDED: (&str, u32) = ("crates/rdy-engine/src/gate.rs", 2702);

fn workspace() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

/// The production lines of `src` that call `landing_of`: no definition and no comment.
fn landing_of_calls(file: &str) -> Vec<u32> {
    let text = std::fs::read_to_string(workspace().join(file)).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            t.contains("landing_of(") && !t.starts_with("//") && !t.contains("fn landing_of")
        })
        .map(|(i, _)| i as u32 + 1)
        .collect()
}

/// D-QCE8M reader audit (Task 3b): every b8b16064 `landing_of` reader is accounted for. The
/// history readers read the carrier as the log stood at their bound and keep their output after
/// a canon replacement; the current-canon readers read canon's membership: a carrier that left
/// canon lands nothing, and a replay's landing lands its source. No other production reader
/// exists.
#[test]
fn scc3_landing_dependencies_use_current_canon_membership() {
    // the audit table against the source: each call is where the table says, of its kind
    for (file, b8, s, now, reads) in READERS {
        let text = std::fs::read_to_string(workspace().join(file)).unwrap();
        let line = text.lines().nth(now as usize - 1).unwrap_or("");
        let want = match reads {
            Reads::Canon => "landing_of(",
            Reads::History => "first_carrier(",
            Reads::Landed => "check_landed(",
        };
        assert!(
            line.contains(want),
            "{file}:{b8} (S {s}) reads by {reads:?}: line {now} is {line:?}"
        );
    }
    let text = std::fs::read_to_string(workspace().join(ADDED.0)).unwrap();
    assert!(
        text.lines()
            .nth(ADDED.1 as usize - 1)
            .unwrap()
            .contains("landing_of(")
    );
    // no unlisted production reader of current canon
    let mut files: Vec<&str> = READERS.iter().map(|r| r.0).collect();
    files.sort();
    files.dedup();
    let mut found: Vec<(String, u32)> = vec![];
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
                    let rel = p
                        .strip_prefix(workspace())
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    for n in landing_of_calls(&rel) {
                        found.push((rel.clone(), n));
                    }
                }
            }
        }
    }
    found.sort();
    let mut want: Vec<(String, u32)> = READERS
        .iter()
        .filter(|r| r.4 == Reads::Canon)
        .map(|r| (r.0.to_string(), r.3))
        .chain([(ADDED.0.to_string(), ADDED.1)])
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
