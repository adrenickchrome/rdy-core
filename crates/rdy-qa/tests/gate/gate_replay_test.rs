//! crb4 (D-2QX9W Replay test and Gate result, D-N0NVF, D-QCE8M): the replay test of a chain of
//! landings stops where a pull replay of it stops, for the same cause, and gives each replayed
//! landing the gate result the stored replay gets. It runs on the local state after a pull's
//! fetch, before its second batch, and writes nothing.
use crate::gate_sync_world::{Synced, copy_dir, land_apart, land_claim, switch_pack, synced};
use rdy_engine::landing_check::{CheckInputs, check_landing};
use rdy_engine::redaction::RedactInput;
use rdy_engine::replay_test::{ReplayStopCause, ReplayTest, replay_test};
use rdy_engine::sync_replay::{PlanKind, PullOutcome, StopReason, plan};
use rdy_engine::version_tests::{RebaseTest, rebase_test};
use rdy_engine::{Rdy, Scope, pin_clock};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::tensions::{j_typo, put_j};
use rdy_store::sync_state::{SyncCanon, SyncStep};
use rdy_store::{ConflictKind, MergeConflict};
use serde_json::json;

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// The end of `r`'s log and every ref it holds.
fn log_and_refs(r: &Rdy) -> (rdy_store::segment::Location, Vec<(String, Vec<u8>)>) {
    (r.store().log_end(), r.store().refs_with_prefix("").unwrap())
}

/// After the fetch of the hub's tip `tip`: the unpushed landings the pull would replay, and the
/// replay test of them onto `tip` on the local state, with `inputs` (the local state's when
/// none). The local store's log end and refs do not move.
fn fetched_test(s: &Synced, tip: Hash, inputs: Option<CheckInputs>) -> (Vec<Hash>, ReplayTest) {
    let st = s.local.read();
    let plan = plan(&st, &s.request(tip, false)).unwrap();
    let PlanKind::Replay { unpushed, .. } = plan.kind else {
        panic!("a replay: {plan:?}");
    };
    let inputs = inputs.unwrap_or_else(|| CheckInputs::of_state(&st));
    let before = log_and_refs(&s.local);
    let test = replay_test(&st, &unpushed, Some(tip), &inputs).unwrap();
    assert_eq!(
        log_and_refs(&s.local),
        before,
        "the replay test writes nothing"
    );
    (unpushed, test)
}

/// The sources of the landings `out` replayed.
fn pulled(out: &PullOutcome) -> Vec<Hash> {
    out.replayed.iter().map(|r| h(&r.source)).collect()
}

fn tested(test: &ReplayTest) -> Vec<Hash> {
    test.replayed.iter().map(|r| r.source).collect()
}

#[test]
fn crb4_replay_test_equals_the_real_pull() {
    // (a) two ok claims replay clean
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let l1 = land_claim(&mut s.local, &sec, "a1", true);
    let l2 = land_claim(&mut s.local, &sec, "a2", true);
    land_apart(&mut s.hub, "h1");
    let tip = s.fetch();
    let (unpushed, test) = fetched_test(&s, tip, None);
    assert_eq!(unpushed, vec![l1, l2]);
    let out = s.second_batch(tip, false);
    assert!(out.not_replayed.is_empty(), "{out:#?}");
    assert_eq!(tested(&test), pulled(&out));
    assert_eq!(tested(&test), vec![l1, l2]);
    assert!(test.replayed.iter().all(|r| r.check.pass), "{test:#?}");
    assert_eq!(test.stop, None);

    // (b) a landing that edits the section the hub's landing also edits: a conflict
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
    let tip = s.fetch();
    let (unpushed, test) = fetched_test(&s, tip, None);
    assert_eq!(unpushed, vec![l1, l2]);
    let out = s.second_batch(tip, false);
    assert_eq!(tested(&test), pulled(&out));
    assert_eq!(tested(&test), vec![l1]);
    let stop = test.stop.clone().expect("a stop");
    let first = &out.not_replayed[0];
    assert_eq!(
        (h(&first.landing), first.reason),
        (l2, StopReason::Conflict)
    );
    assert_eq!((stop.landing, stop.cause), (l2, ReplayStopCause::Conflict));
    assert_eq!(
        stop.conflicts,
        vec![MergeConflict {
            block: BlockId::parse(&sec).unwrap(),
            kind: ConflictKind::StaleBase,
        }]
    );
    assert_eq!((stop.check, stop.removals), (None, vec![]));

    // (c) a claim that fails the rule, which blocks now: the gate
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let l1 = land_claim(&mut s.local, &sec, "a1", true);
    let l2 = land_claim(&mut s.local, &sec, "a2", false);
    land_apart(&mut s.hub, "h1");
    let local_dir = s.dir.path().join("local");
    switch_pack(&local_dir, &mut s.local, "blocking");
    let tip = s.fetch();
    let (unpushed, test) = fetched_test(&s, tip, None);
    assert_eq!(unpushed, vec![l1, l2]);
    let out = s.second_batch(tip, false);
    assert_eq!(tested(&test), pulled(&out));
    assert_eq!(tested(&test), vec![l1]);
    let stop = test.stop.clone().expect("a stop");
    let first = &out.not_replayed[0];
    assert_eq!((h(&first.landing), first.reason), (l2, StopReason::Gate));
    assert_eq!((stop.landing, stop.cause), (l2, ReplayStopCause::Gate));
    let check = stop.check.expect("the gate result");
    assert!(!check.pass);
    let codes: Vec<&str> = check.rows.iter().map(|i| i.code).collect();
    assert_eq!(Some(codes.join(", ")), first.detail, "{check:#?}");
    assert!(stop.conflicts.is_empty() && stop.removals.is_empty());

    // (d) a later landing updates the claim an earlier one wrote: it is checked on the state
    // the earlier one merged to, built in memory, and both replay
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let w = put_j(
        &mut s.local,
        "a1",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": sec, "body": "C holds.", "fields": {"ok": true}}]),
        j_typo(),
    );
    let l1 = h(&land(&mut s.local, "a1").unwrap());
    let c = w.created["$c"].clone();
    put_j(
        &mut s.local,
        "a2",
        json!([{"op": "update", "id": c, "body": "C holds now."}]),
        j_typo(),
    );
    let l2 = h(&land(&mut s.local, "a2").unwrap());
    land_apart(&mut s.hub, "h1");
    let tip = s.fetch();
    let (_, test) = fetched_test(&s, tip, None);
    let out = s.second_batch(tip, false);
    assert!(out.not_replayed.is_empty(), "{out:#?}");
    assert_eq!(tested(&test), pulled(&out));
    assert_eq!(tested(&test), vec![l1, l2]);
    assert!(test.replayed.iter().all(|r| r.check.pass), "{test:#?}");
    assert_eq!(test.stop, None);

    // (e) a landing creates a claim and a later one deletes it; the hub lands apart, so its
    // tip is logged after both: the deletion is no delete of the hub's state, and both replay
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let w = put_j(
        &mut s.local,
        "a1",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": sec, "body": "C holds.", "fields": {"ok": true}}]),
        j_typo(),
    );
    let l1 = h(&land(&mut s.local, "a1").unwrap());
    let c = w.created["$c"].clone();
    put_j(
        &mut s.local,
        "a2",
        json!([{"op": "delete", "id": c}]),
        j_typo(),
    );
    let l2 = h(&land(&mut s.local, "a2").unwrap());
    land_apart(&mut s.hub, "h1");
    let tip = s.fetch();
    let (_, test) = fetched_test(&s, tip, None);
    let rebase = rebase_test(&s.local.read(), &[l1, l2], Some(tip)).unwrap();
    let out = s.second_batch(tip, false);
    assert!(out.not_replayed.is_empty(), "{out:#?}");
    assert_eq!(tested(&test), pulled(&out));
    assert_eq!(tested(&test), vec![l1, l2]);
    assert!(test.replayed.iter().all(|r| r.check.pass), "{test:#?}");
    assert_eq!(test.stop, None);
    // the rebase test of the same chain onto the same tip merges too
    assert_eq!(rebase, RebaseTest::Clean);
}

#[test]
fn crb4_each_replayed_gate_result_equals_the_stored_check() {
    let mut s = synced("advisory");
    let sec = s.section.clone();
    land_claim(&mut s.local, &sec, "a1", true);
    land_claim(&mut s.local, &sec, "a2", true);
    land_apart(&mut s.hub, "h1");
    let tip = s.fetch();
    let inputs = CheckInputs::of_state(&s.local.read());
    let (_, test) = fetched_test(&s, tip, Some(inputs.clone()));
    let out = s.second_batch(tip, false);
    assert_eq!(test.replayed.len(), 2, "{test:#?}");
    assert_eq!(out.replayed.len(), 2, "{out:#?}");
    let st = s.local.read();
    for (t, r) in test.replayed.iter().zip(&out.replayed) {
        assert_eq!(t.source, h(&r.source));
        let stored = st.reader().landing(&h(&r.landing)).unwrap();
        assert_eq!(stored.rebased_from, vec![t.source]);
        assert_eq!(
            t.check,
            check_landing(&st, &stored, &inputs).unwrap(),
            "{}",
            r.landing
        );
    }
}

#[test]
fn crb4_up_to_date_stops_at_the_first_landing() {
    let mut s = synced("advisory");
    let sec = s.section.clone();
    let l1 = land_claim(&mut s.local, &sec, "a1", true);
    land_claim(&mut s.local, &sec, "a2", true);
    land_apart(&mut s.hub, "h1");
    let tip = s.fetch();
    let inputs = CheckInputs {
        require_up_to_date: true,
        ..CheckInputs::of_state(&s.local.read())
    };
    let (_, test) = fetched_test(&s, tip, Some(inputs));
    assert!(test.replayed.is_empty(), "{test:#?}");
    let stop = test.stop.expect("a stop");
    assert_eq!((stop.landing, stop.cause), (l1, ReplayStopCause::UpToDate));
    assert!(stop.check.is_none() && stop.conflicts.is_empty() && stop.removals.is_empty());
    // a strict pull stops at the same landing
    let out = s.second_batch(tip, true);
    assert!(out.replayed.is_empty());
    assert_eq!(
        (h(&out.not_replayed[0].landing), out.not_replayed[0].reason),
        (l1, StopReason::Strict)
    );
}

/// The prose the hub's removal removes.
const SECRET: &str = "crb4-secret-token";

/// A hub and a local copy after one shared landing that holds a prose block with [`SECRET`];
/// the local project last synced at that landing. Returns the world and the prose's id.
fn secret_world() -> (Synced, String) {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hub");
    let mut hub = Rdy::init(&a, "rdy-standard").unwrap();
    let w = write(
        &mut hub,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET}
        ]),
    )
    .unwrap();
    let base = h(&land(&mut hub, "seed").unwrap());
    drop(hub);
    let b = dir.path().join("local");
    copy_dir(&a, &b);
    let mut local = Rdy::open(&b).unwrap();
    let repo = local.store().reader().repo_id().unwrap().unwrap();
    local
        .store_mut()
        .write_sync(
            "origin",
            vec![SyncStep::Canon(SyncCanon {
                repo_id: repo,
                url: crate::gate_sync_world::URL.into(),
                fetched: Some(base),
                applied: Some(base),
                held: None,
            })],
        )
        .unwrap();
    let s = Synced {
        hub: Rdy::open(&a).unwrap(),
        local,
        base,
        section: w.created["$s"].clone(),
        dir,
        fetched: None,
    };
    (s, w.created["$p"].clone())
}

#[test]
fn crb4_removed_content_stops_with_no_text() {
    let _clock = pin_clock(1_000_000);
    let (mut s, prose) = secret_world();
    // a local landing that writes the prose away and back: its second change holds the
    // removed version
    write(
        &mut s.local,
        "keep",
        json!([{"op": "update", "id": prose, "body": "crb4-away-text"}]),
    )
    .unwrap();
    write(
        &mut s.local,
        "keep",
        json!([{"op": "update", "id": prose, "body": SECRET}]),
    )
    .unwrap();
    let lu = h(&land(&mut s.local, "keep").unwrap());
    // the hub removes the prose
    s.hub
        .redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![prose.clone()],
                reason_class: "secret".into(),
                reason: "a leaked token".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let l = h(&land(&mut s.hub, "redact").unwrap());
    let request = s.hub.store().reader().landing(&l).unwrap().checks[0];
    let tip = s.fetch();
    let (unpushed, test) = fetched_test(&s, tip, None);
    assert_eq!(unpushed, vec![lu]);
    assert!(test.replayed.is_empty(), "{test:#?}");
    let stop = test.stop.clone().expect("a stop");
    assert_eq!((stop.landing, stop.cause), (lu, ReplayStopCause::Removal));
    assert_eq!(stop.removals, vec![request]);
    assert!(stop.check.is_none() && stop.conflicts.is_empty());
    let text = serde_json::to_string(&test).unwrap();
    assert!(!text.contains(SECRET), "{text}");
    // the pull stops at the same landing, for the same cause and removal
    let out = s.second_batch(tip, false);
    let first = &out.not_replayed[0];
    assert_eq!((h(&first.landing), first.reason), (lu, StopReason::Removal));
    assert_eq!(first.removals, vec![request.to_string()]);
    assert_ne!(
        s.local
            .read()
            .block(&Scope::Canon, &BlockId::parse(&prose).unwrap())
            .unwrap()
            .body,
        SECRET
    );
}
