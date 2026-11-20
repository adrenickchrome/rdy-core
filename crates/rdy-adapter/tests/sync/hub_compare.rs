//! crb1: the compare and the diff of two scopes a holder's snapshot reads (D-2QX9W Compare): a
//! held landing, a replaced landing, a pushed view at an op, each from the captured refs only.
//! crb5: the compare page of a holder (D-V2X3H) and its empty side (D-AJW0S).

use crate::hub_world::{AT, World, check, moved, seed, world};
use rdy_adapter::compare::DiffQuery;
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Compare, CompareEntry, DiffChange, Hash, SearchRequest, Snapshot, StoreHandle};

fn landing(h: &Hash) -> String {
    format!("landing:{h}")
}

fn view_at(name: &str, op: &Hash) -> String {
    format!("view:{name}@{op}")
}

/// Extends the hub's canon from `p1` to `p2`.
fn extend_p2(h: &StoreHandle, w: &World) {
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.e2]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
}

/// Moves `q2` onto the hub beside or over canon at `p2`.
fn move_q2(h: &StoreHandle, w: &World, how: CanonMove) {
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.eq]).unwrap();
    assert_eq!(h.move_canon(&c, how, Some(w.p2)).unwrap(), moved(w.q2));
}

/// The block id of the first match of `query` in `scope`.
fn found(snap: &Snapshot, query: &str, scope: &str) -> String {
    let reply = snap
        .search(SearchRequest {
            query: query.into(),
            ignore_case: true,
            scope: Some(scope.into()),
            ..Default::default()
        })
        .unwrap();
    reply["matches"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{reply:#}"))
        .to_string()
}

/// The rows of one doc written by `hub_world::doc`: the doc at `path`, its section, and the
/// prose block `prose`.
fn is_doc_rows(rows: &[CompareEntry], path: &str, prose: &str) {
    let mut types: Vec<&str> = rows.iter().map(|e| e.block_type.as_str()).collect();
    types.sort();
    assert_eq!(types, ["doc", "prose", "section"], "{rows:#?}");
    let doc = rows.iter().find(|e| e.block_type == "doc").unwrap();
    assert_eq!(doc.path.as_deref(), Some(path), "{rows:#?}");
    assert!(rows.iter().any(|e| e.id == prose), "{rows:#?}");
}

fn no_change(c: &Compare) {
    assert!(c.changed.is_empty(), "{c:#?}");
    assert!(c.moved.is_empty(), "{c:#?}");
    assert!(c.conflicted.is_empty(), "{c:#?}");
}

#[test]
fn crb1_compare_a_held_landing_with_canon() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    extend_p2(&h, &w);
    move_q2(&h, &w, CanonMove::Hold);
    h.close().unwrap();
    let before = log_end(&w);
    let h = StoreHandle::open_holder(&w.dir.path().join("hub")).unwrap();
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.p2));
    let held = found(&snap, "beside canon", &landing(&w.q2));
    let extend = found(&snap, "canon moves forward", &landing(&w.p2));
    let c = snap
        .compare(Some(landing(&w.p2).as_str()), &landing(&w.q2))
        .unwrap();
    is_doc_rows(&c.added, "held", &held);
    is_doc_rows(&c.deleted, "extend", &extend);
    no_change(&c);
    snap.diff(Some(landing(&w.p2).as_str()), &landing(&w.q2))
        .unwrap();
    drop(snap);
    h.close().unwrap();
    // neither call wrote: the log ends where it ended
    assert_eq!(log_end(&w), before);
}

/// The end of the hub's log, read with the hub closed.
fn log_end(w: &World) -> rdy_store::segment::Location {
    let r = rdy_engine::Rdy::open_holder(&w.dir.path().join("hub")).unwrap();
    let end = r.store().log_end();
    r.close().unwrap();
    end
}

#[test]
fn crb1_compare_a_replaced_landing() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    extend_p2(&h, &w);
    move_q2(&h, &w, CanonMove::Force);
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.q2));
    let extend = found(&snap, "canon moves forward", &landing(&w.p2));
    let c = snap
        .compare(Some(landing(&w.p2).as_str()), "canon")
        .unwrap();
    is_doc_rows(&c.deleted, "extend", &extend);
    drop(snap);
    h.close().unwrap();
}

/// The hub seeded with `p1`, view `draft` pushed at `d1`, canon extended to `p2` and `draft`
/// moved on to `d2`.
fn pushed_draft(w: &World) -> StoreHandle {
    let h = w.holder("hub");
    seed(&h, w);
    let set = |path: &std::path::Path, from: Option<Hash>, to: Hash| {
        let s = h.snapshot().unwrap();
        let c = check(&s, &[path]).unwrap();
        let how = CanonMove::SetViewHead {
            name: "draft".into(),
            root: w.draft_root,
        };
        assert_eq!(h.move_canon(&c, how, from).unwrap(), moved(to));
    };
    set(&w.v1, None, w.d1);
    extend_p2(&h, w);
    set(&w.v2, Some(w.d1), w.d2);
    h
}

/// `draft` at `d1` sits on `p1`; at `d2` it sits on `p2` (the view's base moved with canon
/// before its second write), so the diff from `d1` to `d2` adds the blocks of "Extend" and of
/// "Draft two", and nothing else.
#[test]
fn crb1_compare_a_view_at_an_op() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = pushed_draft(&w);
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.view_head("draft").unwrap().unwrap().base, Some(w.p2));
    let (d1, d2) = (view_at("draft", &w.d1), view_at("draft", &w.d2));
    let prose = found(&snap, "More work in flight", &d2);
    let extend = found(&snap, "canon moves forward", &landing(&w.p2));
    let diff = snap.diff(Some(d1.as_str()), &d2).unwrap();
    assert_eq!(diff.entries.len(), 6, "{diff:#?}");
    assert!(
        diff.entries
            .iter()
            .all(|e| matches!(e.change, DiffChange::Added)),
        "{diff:#?}"
    );
    for id in [&prose, &extend] {
        assert!(
            diff.entries.iter().any(|e| e.id.to_string() == *id),
            "{diff:#?}"
        );
    }
    assert_eq!(diff.counts.added, 6, "{diff:#?}");
    // the compare reads the same two states
    let c = snap.compare(Some(d1.as_str()), &d2).unwrap();
    // each row of the compare split by the doc that holds it, as the diff names it
    let doc_of = |id: &str| {
        let e = diff
            .entries
            .iter()
            .find(|d| d.id.to_string() == id)
            .unwrap();
        e.doc.as_ref().unwrap_or(&e.id).to_string()
    };
    let two_doc = doc_of(&prose);
    let (two, ext): (Vec<CompareEntry>, Vec<CompareEntry>) = c
        .added
        .iter()
        .cloned()
        .partition(|e| doc_of(&e.id) == two_doc);
    is_doc_rows(&two, "draft-two", &prose);
    is_doc_rows(&ext, "extend", &extend);
    assert!(c.deleted.is_empty(), "{c:#?}");
    no_change(&c);
    drop(snap);
    h.close().unwrap();
}

#[test]
fn crb1_compare_is_the_captured_refs() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    extend_p2(&h, &w);
    let s1 = h.snapshot().unwrap();
    move_q2(&h, &w, CanonMove::Hold);
    let e = s1.compare(Some("canon"), &landing(&w.q2)).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    let e = s1.diff(Some("canon"), &landing(&w.q2)).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    let s2 = h.snapshot().unwrap();
    assert!(s2.compare(Some("canon"), &landing(&w.q2)).is_ok());
    assert!(s2.diff(Some("canon"), &landing(&w.q2)).is_ok());
    drop((s1, s2));
    h.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// crb5: the compare page of a holder: a cursor of another pair is stale, and no `from` is the
// empty state (D-V2X3H, D-AJW0S)

fn crb5_query(from: Option<String>, to: String) -> DiffQuery {
    DiffQuery {
        from,
        to,
        doc: None,
        detail: true,
        max_tokens: None,
        page: None,
    }
}

#[test]
fn crb5_a_page_of_another_pair_is_stale() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    extend_p2(&h, &w);
    move_q2(&h, &w, CanonMove::Hold);
    let snap = h.snapshot().unwrap();
    let pair = |to: &Hash| crb5_query(Some(landing(&w.p1)), landing(to));
    // the first budget whose page of (p1, p2) is cut with a cursor
    let next = (20..2_000)
        .step_by(10)
        .find_map(|max| {
            let q = DiffQuery {
                max_tokens: Some(max),
                ..pair(&w.p2)
            };
            let reply = snap.diff_reply(&q).ok()?;
            reply["next"].as_str().map(str::to_string)
        })
        .expect("a page of (p1, p2) with a cursor");
    // the cursor continues its own pair
    let q = DiffQuery {
        page: Some(next.clone()),
        ..pair(&w.p2)
    };
    snap.diff_reply(&q).unwrap();
    let q = DiffQuery {
        page: Some(next),
        ..pair(&w.q2)
    };
    let e = snap.diff_reply(&q).unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e:#?}");
    assert_eq!(
        e.message,
        "the diff behind this page changed since the first page, or the request differs"
    );
    assert_eq!(e.fix, rdy_engine::PAGE_STALE_FIX);
    drop(snap);
    h.close().unwrap();
}

#[test]
fn crb5_empty_from_lists_every_block_added() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    h.close().unwrap();
    // the block count of p1's state, read by the engine with the hub closed
    let blocks = {
        let r = rdy_engine::Rdy::open_holder(&w.dir.path().join("hub")).unwrap();
        let n = r
            .read()
            .materialized(&rdy_engine::Scope::At(w.p1))
            .unwrap()
            .len();
        r.close().unwrap();
        n
    };
    let before = log_end(&w);
    let h = StoreHandle::open_holder(&w.dir.path().join("hub")).unwrap();
    let snap = h.snapshot().unwrap();
    let reply = snap.diff_reply(&crb5_query(None, landing(&w.p1))).unwrap();
    assert_eq!(
        reply["from"],
        serde_json::json!({"kind": "empty"}),
        "{reply:#}"
    );
    assert_eq!(reply["to"]["kind"], "landing", "{reply:#}");
    let entries = reply["entries"].as_array().unwrap();
    assert_eq!(entries.len(), blocks, "{reply:#}");
    assert!(entries.iter().all(|e| e["change"] == "added"), "{reply:#}");
    assert_eq!(
        reply["counts"],
        serde_json::json!({"added": blocks, "removed": 0, "updated": 0, "moved": 0})
    );
    // the compare and the diff of the empty state take no `from` alike
    let c = snap.compare(None, &landing(&w.p1)).unwrap();
    assert_eq!(c.b, "empty");
    assert_eq!(c.added.len(), blocks, "{c:#?}");
    assert!(c.deleted.is_empty(), "{c:#?}");
    no_change(&c);
    let d = snap.diff(None, &landing(&w.p1)).unwrap();
    assert_eq!(d.counts.added, blocks, "{d:#?}");
    drop(snap);
    h.close().unwrap();
    // no call wrote: the log ends where it ended
    assert_eq!(log_end(&w), before);
}
