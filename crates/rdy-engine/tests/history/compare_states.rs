//! spv2 (D-CWC91): the block-level compare of two supplied materialized states. A review of a
//! profile compares the selected state of each side, so the state loop works without a scope:
//! [`compare_states`] gives the rows the scope compare gives, with empty labels and no
//! conflicts. The scope compare keeps its labels and appends its scopes' conflicts unchanged.
use rdy_engine::view_query::compare_states;
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};

fn writer() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn approver() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, "spv2", Some("test summary"))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
}

/// A landed canon: folder `area` holding docs `one` (prose X, Y, Z) and `two` (prose W).
struct Base {
    _dir: tempfile::TempDir,
    r: Rdy,
    area: String,
    one: String,
    two: String,
    x: String,
    y: String,
    z: String,
    w: String,
}

fn base() -> Base {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area"}},
            {"op": "create", "type": "doc", "id": "$one", "parent": "$f", "fields": {"name": "one", "title": "One", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$one", "body": "Text x."},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$one", "body": "Text y."},
            {"op": "create", "type": "prose", "id": "$z", "parent": "$one", "body": "Text z."},
            {"op": "create", "type": "doc", "id": "$two", "parent": "$f", "fields": {"name": "two", "title": "Two", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$w", "parent": "$two", "body": "Text w."}
        ]),
    );
    land(&mut r, "seed");
    let id = |k: &str| w.created[k].clone();
    Base {
        area: id("$f"),
        one: id("$one"),
        two: id("$two"),
        x: id("$x"),
        y: id("$y"),
        z: id("$z"),
        w: id("$w"),
        _dir: dir,
        r,
    }
}

/// The scope compare with its labels cleared and its conflicts dropped: the state rows only.
fn rows(mut c: Compare) -> Compare {
    c.a = String::new();
    c.b = String::new();
    c.conflicted = vec![];
    c
}

fn ids(entries: &[CompareEntry]) -> Vec<String> {
    entries.iter().map(|e| e.id.clone()).collect()
}

#[test]
fn spv2_compare_states_matches_scope_compare() {
    let mut b = base();
    // the view adds a doc and a block, deletes one, edits one, moves one to the other doc and
    // retitles a doc
    write(
        &mut b.r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$new", "parent": b.area, "fields": {"name": "three", "title": "Three", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$new", "body": "Text n."},
            {"op": "delete", "id": b.y},
            {"op": "update", "id": b.x, "body": "Text x, edited."},
            {"op": "move", "id": b.z, "parent": b.two},
            {"op": "update", "id": b.one, "fields": {"title": "One, renamed"}}
        ]),
    );
    let st = b.r.read();
    let work = Scope::View("work".into());
    let (mw, mc) = (
        st.materialized(&work).unwrap(),
        st.materialized(&Scope::Canon).unwrap(),
    );
    for (a, sa, bb, sb) in [
        (&work, &mw, &Scope::Canon, &mc),
        (&Scope::Canon, &mc, &work, &mw),
    ] {
        let scoped = st.compare(a, bb).unwrap();
        let state = compare_states(sa, sb);
        assert_eq!((state.a.as_str(), state.b.as_str()), ("", ""));
        assert!(state.conflicted.is_empty());
        assert_eq!(state, rows(scoped.clone()), "{a:?} against {bb:?}");
        assert_ne!(scoped.a, "", "the scope compare keeps its labels");
    }
    // every row kind is exercised, sorted by id, with fields and parents as the scope gives them
    let c = compare_states(&mw, &mc);
    assert_eq!(c.added.len(), 2);
    assert_eq!(ids(&c.deleted), vec![b.y.clone()]);
    let mut changed = vec![b.one.clone(), b.x.clone()];
    changed.sort();
    assert_eq!(ids(&c.changed), changed);
    let mut sorted = ids(&c.added);
    sorted.sort();
    assert_eq!(ids(&c.added), sorted, "sorted by id");
    let x = c.changed.iter().find(|e| e.id == b.x).unwrap();
    assert_eq!(x.fields, vec!["body".to_string()]);
    let one = c.changed.iter().find(|e| e.id == b.one).unwrap();
    assert_eq!(one.fields, vec!["title".to_string()]);
    assert_eq!(ids(&c.moved), vec![b.z.clone()]);
    assert_eq!(
        (
            c.moved[0].from_parent.as_deref(),
            c.moved[0].to_parent.as_deref()
        ),
        (Some(b.one.as_str()), Some(b.two.as_str()))
    );
    // equal states compare empty
    let same = compare_states(&mc, &mc);
    assert!(
        same.added.is_empty()
            && same.deleted.is_empty()
            && same.changed.is_empty()
            && same.moved.is_empty()
    );
    // against an empty side every block is added, with its path
    let empty = Materialized::default();
    let all = compare_states(&mc, &empty);
    assert_eq!(all.added.len(), mc.len());
    let area = all.added.iter().find(|e| e.id == b.area).unwrap();
    assert_eq!(area.path.as_deref(), Some("area"));
    let gone = compare_states(&empty, &mc);
    assert_eq!(gone.deleted.len(), mc.len());
}

/// Two views that each hold a merge conflict: `left` on X, `right` on W. Each view edits its
/// block, then includes another view's edit of the same block from the same base.
fn conflicted() -> (Base, BlockId, BlockId) {
    let mut b = base();
    let ca = write(
        &mut b.r,
        "side-a",
        json!([{"op": "update", "id": b.x, "body": "Text x, a."}]),
    )
    .change;
    write(
        &mut b.r,
        "left",
        json!([{"op": "update", "id": b.x, "body": "Text x, left."}]),
    );
    b.r.view_include(&writer(), "left", &[ca]).unwrap();
    let cb = write(
        &mut b.r,
        "side-b",
        json!([{"op": "update", "id": b.w, "body": "Text w, b."}]),
    )
    .change;
    write(
        &mut b.r,
        "right",
        json!([{"op": "update", "id": b.w, "body": "Text w, right."}]),
    );
    b.r.view_include(&writer(), "right", &[cb]).unwrap();
    let (x, w) = (BlockId::parse(&b.x).unwrap(), BlockId::parse(&b.w).unwrap());
    (b, x, w)
}

#[test]
fn spv2_scope_compare_keeps_its_conflict_supplement() {
    let (b, x, w) = conflicted();
    let st = b.r.read();
    let (left, right) = (Scope::View("left".into()), Scope::View("right".into()));
    let lc = st.conflicts(&left).unwrap();
    let rc = st.conflicts(&right).unwrap();
    assert_eq!(lc.len(), 1, "left holds one conflict");
    assert_eq!(rc.len(), 1, "right holds one conflict");
    let want = |first: (&BlockId, String), second: (&BlockId, String)| {
        let mut v = vec![
            (first.0.to_string(), Some(first.1)),
            (second.0.to_string(), Some(second.1)),
        ];
        v.sort();
        v
    };
    let got = |c: &Compare| -> Vec<(String, Option<String>)> {
        c.conflicted
            .iter()
            .map(|e| (e.id.clone(), e.conflict.clone()))
            .collect()
    };
    // both sides' conflicts, by block id, each with its own conflict id
    for (a, bb) in [(&left, &right), (&right, &left)] {
        let scoped = st.compare(a, bb).unwrap();
        assert_eq!(
            got(&scoped),
            want((&x, lc[0].id.to_string()), (&w, rc[0].id.to_string())),
            "{a:?} against {bb:?}"
        );
        assert_eq!(
            (scoped.a.as_str(), scoped.b.as_str()),
            (
                if a == &left { "left" } else { "right" },
                if bb == &left { "left" } else { "right" }
            )
        );
        // the state rows are the extracted loop's, and it adds no conflict
        let (ma, mb) = (st.materialized(a).unwrap(), st.materialized(bb).unwrap());
        let state = compare_states(&ma, &mb);
        assert!(state.conflicted.is_empty());
        assert_eq!(state, rows(scoped));
    }
    // canon has no conflicts: only the view's own are listed
    let scoped = st.compare(&left, &Scope::Canon).unwrap();
    assert_eq!(
        got(&scoped),
        vec![(x.to_string(), Some(lc[0].id.to_string()))]
    );
    let scoped = st.compare(&Scope::Canon, &right).unwrap();
    assert_eq!(
        got(&scoped),
        vec![(w.to_string(), Some(rc[0].id.to_string()))]
    );
}
