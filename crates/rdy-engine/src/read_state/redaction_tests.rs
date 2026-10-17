//! rdx10: published read states and their caches belong to one redaction epoch (plan Redaction,
//! Task 10; D-CC90E, D-600CW). After a removal the writer rebuilds canon, the views and every
//! shared cache from a fresh reader; same-epoch publishes keep their reuse; the masked preview a
//! removal is checked against is private to the writer and never published.
use super::ReadState;
use crate::redaction::{RedactInput, fail_next_redaction_previews_for_tests};
use crate::{ProposalInfo, Rdy, Scope, Session, WriteRequest};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{BlockId, Hash, Role};
use serde_json::json;
use std::sync::Arc;

const SECRET: &str = "The launch code is ZEBRA-7731.";

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer)
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver)
}

fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> crate::WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

/// A project with landed content (tagged v1.0.0), an open view `edit` with its own change, and
/// a recorded request for the prose block, proposed from view `redact`.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    prose: BlockId,
    term: String,
    landing: Hash,
    request: Hash,
    prop: ProposalInfo,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}
        ]),
    );
    let base = r.propose(&writer(), "work", "Base", None).unwrap();
    let landing: Hash = r.land(&approver(), &base.id).unwrap().parse().unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let term = w.created["$t"].clone();
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": term.clone(), "body": "A ledger of records."}]),
    );
    let prose = BlockId::parse(&w.created["$p"]).unwrap();
    let res = r
        .redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![prose.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let prop = r
        .propose(&writer(), "redact", "Remove the code", None)
        .unwrap();
    World {
        _dir: dir,
        r,
        prose,
        term,
        landing,
        request: res.redaction.unwrap(),
        prop,
    }
}

fn scopes(w: &World) -> Vec<Scope> {
    vec![
        Scope::Canon,
        Scope::View("edit".into()),
        Scope::At(w.landing),
        Scope::Proposal(w.prop.id.parse().unwrap()),
        Scope::Inflight,
    ]
}

/// Fills every cache a state holds or shares: each scope's build, graph, tree and lexicon; the
/// as-of and proposal entries; the version positions; the proposal touched sets; canon's steps;
/// the in-flight set and build; the tension set; the coordination index; the coverage memo.
fn seed(st: &ReadState, w: &World) {
    for s in scopes(w) {
        assert_eq!(st.block(&s, &w.prose).unwrap().body, SECRET, "{s:?}");
        st.graph(&s).unwrap();
        st.lexicon(&s).unwrap();
        st.version_position(&s).unwrap();
    }
    st.tree_full(&Scope::Canon).unwrap();
    st.proposal_touched
        .lock()
        .unwrap()
        .insert(w.prop.id.parse().unwrap(), Arc::default());
    drop(st.canon_steps().unwrap());
    st.inflight_set().unwrap();
    st.tensions().unwrap();
    st.coverage_issues(&Scope::Canon).unwrap();
    assert!(st.version_position_cache_len() > 0);
    assert!(st.proposal_touched_cache_len() > 0);
}

/// The builds a state holds for its canon and views.
fn builds(st: &ReadState) -> Vec<Arc<super::Built>> {
    let mut out: Vec<Arc<super::Built>> = st.canon.built_ok().cloned().into_iter().collect();
    out.extend(st.views.values().filter_map(|e| e.built_ok().cloned()));
    out.extend(st.inflight_cell.built_ok().cloned());
    out
}

/// `new` shares no cache with `old`.
fn shares_none(old: &ReadState, new: &ReadState, what: &str) {
    assert!(!Arc::ptr_eq(&old.canon, &new.canon), "{what}: canon entry");
    for (name, e) in &new.views {
        if let Some(o) = old.views.get(name) {
            assert!(!Arc::ptr_eq(o, e), "{what}: view {name}");
        }
    }
    for b in builds(new) {
        for o in builds(old) {
            assert!(!Arc::ptr_eq(&b, &o), "{what}: a build");
            assert!(!Arc::ptr_eq(&b.m, &o.m), "{what}: a materialization");
        }
    }
    assert!(!Arc::ptr_eq(&old.as_of, &new.as_of), "{what}: as-of cache");
    assert!(
        !Arc::ptr_eq(&old.vpos, &new.vpos),
        "{what}: version positions"
    );
    assert!(
        !Arc::ptr_eq(&old.proposal_touched, &new.proposal_touched),
        "{what}: proposal touched sets"
    );
    assert!(
        !Arc::ptr_eq(&old.canon_step_cache, &new.canon_step_cache),
        "{what}: canon steps"
    );
    assert!(
        !Arc::ptr_eq(&old.inflight_cell, &new.inflight_cell),
        "{what}: in-flight cell"
    );
    assert!(
        !Arc::ptr_eq(&old.tensions, &new.tensions),
        "{what}: tension cell"
    );
    assert!(
        !Arc::ptr_eq(&old.coord_heads, &new.coord_heads),
        "{what}: coordination heads"
    );
    assert!(
        !Arc::ptr_eq(&old.semantic, &new.semantic),
        "{what}: semantic state"
    );
    // nothing carried in: each shared cache starts empty, the coverage memo carries nothing
    assert!(new.as_of.lru.lock().unwrap().is_empty(), "{what}");
    assert!(new.as_of.proposals.lock().unwrap().is_empty(), "{what}");
    assert_eq!(new.version_position_cache_len(), 0, "{what}");
    assert_eq!(new.proposal_touched_cache_len(), 0, "{what}");
    assert!(
        new.tensions.set.get().is_none(),
        "{what}: no tension set carried"
    );
    assert!(
        new.canon.built_ok().is_none(),
        "{what}: canon starts unbuilt"
    );
    assert!(
        new.views.values().all(|e| e.built_ok().is_none()),
        "{what}: views start unbuilt"
    );
    let (o, n) = (
        old.coverage_issues(&Scope::Canon).unwrap(),
        new.coverage_issues(&Scope::Canon).unwrap(),
    );
    assert!(!Arc::ptr_eq(&o, &n), "{what}: coverage memo");
}

#[test]
fn rdx10_no_cache_crosses_a_redaction_epoch() {
    let _clock = crate::pin_clock(1_000_000);
    let mut w = world();
    let old = w.r.read();
    seed(&old, &w);
    // the full builder behind the preview copies no cache
    let preview = w.r.redaction_preview(&[w.request]).unwrap();
    shares_none(&old, &preview, "the preview");
    assert_eq!(
        preview.block(&Scope::Canon, &w.prose).unwrap().body,
        CONTENT_REMOVED,
        "the preview reads the request as applied"
    );
    drop(preview);
    // a real apply: the published state, and the writer's own shared caches, are all new
    w.r.land(&approver(), &w.prop.id).unwrap();
    let new = w.r.read();
    new.ensure_current_epoch().unwrap();
    assert!(old.ensure_current_epoch().is_err());
    shares_none(&old, &new, "after the apply");
    assert!(Arc::ptr_eq(&w.r.as_of, &new.as_of));
    assert!(Arc::ptr_eq(&w.r.vpos, &new.vpos));
    assert!(Arc::ptr_eq(&w.r.proposal_touched, &new.proposal_touched));
    assert!(Arc::ptr_eq(&w.r.canon_step_cache, &new.canon_step_cache));
    let logged = w.r.logged.clone().expect("the batch logged its moves");
    assert!(
        Arc::ptr_eq(&logged, &new),
        "the logged state is the new epoch's"
    );
    for s in scopes(&w) {
        let body = new.block(&s, &w.prose).unwrap().body;
        assert_eq!(body, CONTENT_REMOVED, "{s:?}");
    }
}

#[test]
fn rdx10_ordinary_publishes_keep_cache_reuse() {
    let _clock = crate::pin_clock(2_000_000);
    let mut w = world();
    let st = w.r.read();
    seed(&st, &w);
    // a commit carries canon's entry and every shared cache
    write(
        &mut w.r,
        "edit",
        json!([{"op": "update", "id": w.term.clone(), "body": "A ledger of all records."}]),
    );
    let next = w.r.read();
    assert!(Arc::ptr_eq(&st.canon, &next.canon), "commit: canon entry");
    assert!(Arc::ptr_eq(&st.as_of, &next.as_of), "commit: as-of");
    assert!(
        Arc::ptr_eq(&st.vpos, &next.vpos),
        "commit: version positions"
    );
    assert!(Arc::ptr_eq(&st.proposal_touched, &next.proposal_touched));
    assert!(Arc::ptr_eq(&st.canon_step_cache, &next.canon_step_cache));
    assert!(
        Arc::ptr_eq(&st.semantic, &next.semantic),
        "commit: semantic"
    );
    assert!(
        Arc::ptr_eq(&st.coord_heads, &next.coord_heads),
        "commit: coord"
    );
    assert_eq!(st.redaction_epoch(), next.redaction_epoch());
    // an ordinary landing seeds canon's build from the one before and keeps the shared caches
    let side = write(
        &mut w.r,
        "side",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "other", "title": "Other"}}
        ]),
    );
    assert!(!side.created.is_empty());
    let before = w.r.read();
    before.materialized(&Scope::Canon).unwrap();
    let p = w.r.propose(&writer(), "side", "Other", None).unwrap();
    w.r.land(&approver(), &p.id).unwrap();
    let landed = w.r.read();
    assert!(Arc::ptr_eq(&before.as_of, &landed.as_of), "landing: as-of");
    assert!(Arc::ptr_eq(&before.vpos, &landed.vpos));
    assert!(Arc::ptr_eq(
        &before.proposal_touched,
        &landed.proposal_touched
    ));
    assert!(Arc::ptr_eq(
        &before.canon_step_cache,
        &landed.canon_step_cache
    ));
    assert!(landed.canon.built_ok().is_some(), "landing: canon seeded");
    assert_eq!(before.redaction_epoch(), landed.redaction_epoch());
    // a full publish in the same epoch keeps the writer's shared caches
    w.r.invalidate();
    let full = w.r.read();
    assert!(Arc::ptr_eq(&landed.as_of, &full.as_of), "full: as-of");
    assert!(Arc::ptr_eq(&landed.vpos, &full.vpos));
    assert!(Arc::ptr_eq(
        &landed.proposal_touched,
        &full.proposal_touched
    ));
    assert!(Arc::ptr_eq(
        &landed.canon_step_cache,
        &full.canon_step_cache
    ));
    landed.ensure_current_epoch().unwrap();
    full.ensure_current_epoch().unwrap();
}

#[test]
fn rdx10_a_preview_is_private_and_failure_publishes_nothing() {
    let _clock = crate::pin_clock(3_000_000);
    let mut w = world();
    let public = w.r.read();
    let end = w.r.store().log_end();
    // the preview reads the request as applied; the published state is untouched
    let preview = w.r.redaction_preview(&[w.request]).unwrap();
    assert_eq!(
        preview.block(&Scope::Canon, &w.prose).unwrap().body,
        CONTENT_REMOVED
    );
    assert!(Arc::ptr_eq(&public, &w.r.read()), "nothing was published");
    assert_eq!(public.block(&Scope::Canon, &w.prose).unwrap().body, SECRET);
    assert_eq!(preview.redaction_epoch(), public.redaction_epoch());
    public.ensure_current_epoch().unwrap();
    assert_eq!(w.r.store().log_end(), end, "a preview writes nothing");
    assert!(w.r.store().open_batch().is_none());
    drop(preview);
    // an unknown request has no preview
    assert!(w.r.redaction_preview(&[Hash([9; 32])]).is_err());
    assert!(Arc::ptr_eq(&public, &w.r.read()));
    // a preview failure during the approval publishes nothing and seals nothing
    fail_next_redaction_previews_for_tests(1);
    let e = w.r.land(&approver(), &w.prop.id).unwrap_err();
    assert_eq!(e.code, crate::E_INTERNAL, "{e}");
    assert!(
        Arc::ptr_eq(&public, &w.r.read()),
        "the failure publishes nothing"
    );
    assert!(
        w.r.store().open_batch().is_none(),
        "the batch was abandoned"
    );
    assert_eq!(
        w.r.store().log_end(),
        end,
        "nothing of the batch stays in the log"
    );
    assert_eq!(public.redaction_epoch(), 0, "the epoch never moved");
}

#[test]
fn rdx10_a_failed_finish_publishes_no_cache_of_the_old_epoch() {
    use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
    let _clock = crate::pin_clock(4_000_000);
    let mut w = world();
    let old = w.r.read();
    seed(&old, &w);
    arm_failpoint(Some((Failpoint::AfterZeroWrite, FailAction::Fail)));
    let e = w.r.land(&approver(), &w.prop.id).unwrap_err();
    arm_failpoint(None);
    assert_eq!(e.code, crate::E_REDACTION_PENDING, "{e}");
    // the published state holds the error and no entry, build or cache of the old epoch
    let new = w.r.read();
    assert_eq!(
        new.error.as_ref().map(|e| e.code),
        Some(crate::E_REDACTION_PENDING)
    );
    assert!(new.views.is_empty());
    assert!(new.canon.built_ok().is_none());
    assert!(!Arc::ptr_eq(&old.as_of, &new.as_of));
    assert!(!Arc::ptr_eq(&old.vpos, &new.vpos));
    assert!(!Arc::ptr_eq(&old.proposal_touched, &new.proposal_touched));
    assert!(!Arc::ptr_eq(&old.canon_step_cache, &new.canon_step_cache));
    assert!(!Arc::ptr_eq(&old.inflight_cell, &new.inflight_cell));
    assert!(!Arc::ptr_eq(&old.tensions, &new.tensions));
    assert!(!Arc::ptr_eq(&old.semantic, &new.semantic));
    assert!(
        Arc::ptr_eq(&w.r.as_of, &new.as_of),
        "the writer's caches start over"
    );
    assert!(w.r.logged.is_none(), "the old logged state is dropped");
    for s in scopes(&w) {
        assert_eq!(
            new.block(&s, &w.prose).unwrap_err().code,
            crate::E_REDACTION_PENDING,
            "{s:?}"
        );
    }
}
