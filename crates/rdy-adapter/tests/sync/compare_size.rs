//! cs1: the size of a compare, counted up to a limit. `compare_size` counts the rows the matching
//! `compare` lists in `added`, `deleted`, `changed`, `moved` and `conflicted`, and stops at the
//! first row past the limit. It refuses what `compare` refuses, with the same code, message and
//! fix.

use crate::hub_world::{
    AT, World, check, doc, export, land, moved, project, pushed_draft, repo_of, seed, session, tag,
    view_head_of, world, write_in,
};
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Compare, CompareSize, Hash, RdyError, Snapshot, StoreHandle};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

fn landing(h: &Hash) -> String {
    format!("landing:{h}")
}

fn view_at(name: &str, op: &Hash) -> String {
    format!("view:{name}@{op}")
}

/// The rows of a compare, each counted as the size counts it.
fn total(c: &Compare) -> usize {
    c.added.len() + c.deleted.len() + c.changed.len() + c.moved.len() + c.conflicted.len()
}

fn size(s: &Snapshot, from: Option<&str>, to: &str, limit: usize) -> CompareSize {
    s.compare_size(from, to, limit)
        .unwrap_or_else(|e| panic!("{from:?} to {to}: {e:#?}"))
}

/// Every pair of a holder snapshot: canon, three landings (one held), the empty side and a
/// pushed view at two ops.
fn hub_pairs(w: &World) -> Vec<(Option<String>, String)> {
    let some = |s: String| Some(s);
    vec![
        (some("canon".into()), "canon".into()),
        (some(landing(&w.p1)), landing(&w.p2)),
        (some(landing(&w.p2)), landing(&w.p1)),
        (some(landing(&w.p2)), landing(&w.q2)),
        (some(landing(&w.q2)), landing(&w.p1)),
        (None, landing(&w.p1)),
        (None, landing(&w.p2)),
        (some(view_at("draft", &w.d1)), view_at("draft", &w.d2)),
        (some(view_at("draft", &w.d2)), view_at("draft", &w.d1)),
        (some(landing(&w.p2)), view_at("draft", &w.d2)),
        (None, view_at("draft", &w.d2)),
    ]
}

/// The hub of `w` with `p1`, `p2`, a held `q2` and view `draft` pushed at `d1` and `d2`.
fn hub(w: &World) -> StoreHandle {
    let h = pushed_draft(w);
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.eq]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Hold, Some(w.p2)).unwrap(),
        moved(w.q2)
    );
    drop((s, c));
    h
}

/// The hub with canon replaced by `q2`: `p2` is a replaced landing.
fn replaced(w: &World) -> StoreHandle {
    let h = w.holder("hub-replaced");
    seed(&h, w);
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.e2]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
    drop((s, c));
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.eq]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Force, Some(w.p2)).unwrap(),
        moved(w.q2)
    );
    drop((s, c));
    h
}

/// The hub of a project with moved, changed and conflicted blocks. Project P lands `l1`, a
/// folder `area` with docs `one` (prose X, Y, Z, M) and `two` (prose U, V), and then `l2`: a new
/// doc, V deleted, X and Y edited, Z moved to `two` and M moved to `two` with an edit. Two views of P
/// hold a merge conflict each, pushed to the hub: `left` on X and `right` on U.
struct Conflicts {
    _dir: tempfile::TempDir,
    hub: StoreHandle,
    l1: Hash,
    l2: Hash,
    left: Hash,
    right: Hash,
}

fn conflicts() -> Conflicts {
    use rdy_adapter::stream::StreamPurpose;
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let mut r = project(&pd);
    let repo = repo_of(&r);
    let w = write_in(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area"}},
            {"op": "create", "type": "doc", "id": "$one", "parent": "$f", "fields": {"name": "one", "title": "One", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$one", "body": "Text x."},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$one", "body": "Text y."},
            {"op": "create", "type": "prose", "id": "$z", "parent": "$one", "body": "Text z."},
            {"op": "create", "type": "prose", "id": "$m", "parent": "$one", "body": "Text m."},
            {"op": "create", "type": "doc", "id": "$two", "parent": "$f", "fields": {"name": "two", "title": "Two", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$u", "parent": "$two", "body": "Text u."},
            {"op": "create", "type": "prose", "id": "$v", "parent": "$two", "body": "Text v."}
        ]),
    );
    let l1 = land(&mut r, "seed");
    let id = |k: &str| w.created[k].clone();
    let (area, two) = (id("$f"), id("$two"));
    let (x, y, z, m) = (id("$x"), id("$y"), id("$z"), id("$m"));
    let (u, v) = (id("$u"), id("$v"));
    write_in(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$new", "parent": area, "fields": {"name": "three", "title": "Three", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$new", "body": "Text n."},
            {"op": "delete", "id": v},
            {"op": "update", "id": y, "body": "Text y, edited."},
            {"op": "update", "id": x, "body": "Text x, edited."},
            {"op": "move", "id": z, "parent": two},
            {"op": "move", "id": m, "parent": two}
        ]),
    );
    write_in(
        &mut r,
        "work",
        json!([{"op": "update", "id": m, "body": "Text m, edited."}]),
    );
    let l2 = land(&mut r, "work");
    let writer = session(rdy_model::Role::Writer);
    let mut conflicted = |name: &str, block: &str, side: &str| {
        let c = write_in(
            &mut r,
            side,
            json!([{"op": "update", "id": block, "body": format!("Text, {side}.")}]),
        )
        .change;
        write_in(
            &mut r,
            name,
            json!([{"op": "update", "id": block, "body": format!("Text, {name}.")}]),
        );
        r.view_include(&writer, name, &[c]).unwrap();
        let head = view_head_of(&r, name);
        let mut root = head;
        while let Some(prev) = r.store().reader().view_op(&root).unwrap().prev {
            root = prev;
        }
        (head, root)
    };
    let (left, left_root) = conflicted("left", &x, "side-a");
    let (right, right_root) = conflicted("right", &u, "side-b");
    r.close().unwrap();
    let at = |n: &str| dir.path().join(format!("{n}.rdysync"));
    export(&pd, StreamPurpose::CanonHistory, l1, None, &at("e1"));
    export(&pd, StreamPurpose::CanonHistory, l2, Some(l1), &at("e2"));
    export(&pd, StreamPurpose::ViewHistory, left, None, &at("left"));
    export(&pd, StreamPurpose::ViewHistory, right, None, &at("right"));
    let hub = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let step = |path: &Path, how: CanonMove, from: Option<Hash>, to: Hash| {
        let s = hub.snapshot().unwrap();
        let c = check(&s, &[path]).unwrap();
        assert_eq!(hub.move_canon(&c, how, from).unwrap(), moved(to));
    };
    step(&at("e1"), CanonMove::Seed, None, l1);
    step(&at("e2"), CanonMove::Extend, Some(l1), l2);
    for (name, root, head) in [("left", left_root, left), ("right", right_root, right)] {
        let set = CanonMove::SetViewHead {
            name: name.into(),
            root,
        };
        step(&at(name), set, None, head);
    }
    Conflicts {
        _dir: dir,
        hub,
        l1,
        l2,
        left,
        right,
    }
}

fn conflict_pairs(w: &Conflicts) -> Vec<(Option<String>, String)> {
    let some = |s: String| Some(s);
    let (left, right) = (view_at("left", &w.left), view_at("right", &w.right));
    vec![
        (some("canon".into()), "canon".into()),
        (some(landing(&w.l1)), landing(&w.l2)),
        (some(landing(&w.l2)), landing(&w.l1)),
        (None, landing(&w.l2)),
        (some("canon".into()), left.clone()),
        (some(left.clone()), "canon".into()),
        (some(left.clone()), right.clone()),
        (some(right.clone()), left.clone()),
        (some(left.clone()), left.clone()),
        (some(landing(&w.l1)), left.clone()),
        (None, right.clone()),
    ]
}

/// Every snapshot pair of the tests, with the compare of each.
fn each_pair(mut visit: impl FnMut(&Snapshot, Option<&str>, &str, Compare)) {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    for h in [hub(&w), replaced(&w)] {
        let snap = h.snapshot().unwrap();
        let mut pairs = hub_pairs(&w);
        pairs.push((Some(landing(&w.p2)), "canon".into()));
        for (from, to) in pairs {
            // the pairs that name a landing the hub does not hold are refused by both calls
            let Ok(c) = snap.compare(from.as_deref(), &to) else {
                continue;
            };
            visit(&snap, from.as_deref(), &to, c);
        }
        drop(snap);
        h.close().unwrap();
    }
    let w = conflicts();
    let snap = w.hub.snapshot().unwrap();
    for (from, to) in conflict_pairs(&w) {
        let c = snap
            .compare(from.as_deref(), &to)
            .unwrap_or_else(|e| panic!("{from:?} to {to}: {e:#?}"));
        visit(&snap, from.as_deref(), &to, c);
    }
    drop(snap);
    w.hub.close().unwrap();
}

#[test]
fn cs1_size_matches_compare_below_the_limit() {
    let (mut pairs, mut rows, mut conflicted, mut empty) = (0, 0, 0, 0);
    each_pair(|snap, from, to, c| {
        let n = total(&c);
        pairs += 1;
        rows += n;
        conflicted += c.conflicted.len();
        empty += usize::from(n == 0);
        for limit in [n + 1, n + 1_000, usize::MAX] {
            assert_eq!(
                size(snap, from, to, limit),
                CompareSize {
                    count: n,
                    stopped: false
                },
                "{from:?} to {to} at {limit}"
            );
        }
    });
    // the pairs are not vacuous: they list rows, conflicts and some empty compares
    assert!(pairs >= 20, "{pairs} pairs");
    assert!(rows >= 30, "{rows} rows");
    assert!(conflicted >= 3, "{conflicted} conflicted rows");
    assert!(empty >= 2, "{empty} empty compares");
}

#[test]
fn cs1_size_stops_at_the_limit() {
    let mut counted = 0;
    each_pair(|snap, from, to, c| {
        let n = total(&c);
        let at = |limit| size(snap, from, to, limit);
        let pair = |s: CompareSize| (s.count, s.stopped);
        assert_eq!(pair(at(n + 1)), (n, false), "{from:?} to {to}");
        assert_eq!(pair(at(n)), (n, false), "{from:?} to {to}");
        assert_eq!(pair(at(0)), (0, n > 0), "{from:?} to {to}");
        if n > 0 {
            assert_eq!(pair(at(n - 1)), (n - 1, true), "{from:?} to {to}");
            counted += 1;
        }
        // every limit below the total stops at the limit
        for limit in 0..n {
            assert_eq!(pair(at(limit)), (limit, true), "{from:?} to {to}");
        }
    });
    assert!(counted >= 15, "{counted} pairs with rows");
}

#[test]
fn cs1_size_counts_move_and_change_as_compare_does() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = conflicts();
    let snap = w.hub.snapshot().unwrap();
    let ids = |rows: &[rdy_adapter::CompareEntry]| -> BTreeSet<String> {
        rows.iter().map(|e| e.id.clone()).collect()
    };
    let exact = |from: &str, to: &str, want: usize| {
        let c = snap.compare(Some(from), to).unwrap();
        assert_eq!(total(&c), want, "{c:#?}");
        assert_eq!(
            size(&snap, Some(from), to, 100),
            CompareSize {
                count: want,
                stopped: false
            }
        );
        let cut = size(&snap, Some(from), to, want - 1);
        assert_eq!((cut.count, cut.stopped), (want - 1, true));
    };
    let (l1, l2) = (landing(&w.l1), landing(&w.l2));
    let (left, right) = (view_at("left", &w.left), view_at("right", &w.right));

    // l1 to l2: two moved blocks, one of them also changed (M), so M is two rows
    let c = snap.compare(Some(&l1), &l2).unwrap();
    assert_eq!(c.moved.len(), 2, "{c:#?}");
    let both: Vec<String> = ids(&c.moved)
        .intersection(&ids(&c.changed))
        .cloned()
        .collect();
    assert_eq!(both.len(), 1, "one block moves and changes: {c:#?}");
    assert!(
        c.moved.iter().any(|e| e.id != both[0]),
        "one block only moves: {c:#?}"
    );
    assert_eq!((c.added.len(), c.deleted.len()), (2, 1), "{c:#?}");
    // added 2, deleted 1, changed X, Y and M, moved Z and M
    assert_eq!(total(&c), 8, "{c:#?}");
    exact(&l1, &l2, 8);

    // the pushed view `left`: X is conflicted, and a conflicted block is one row
    let c = snap.compare(Some("canon"), &left).unwrap();
    assert_eq!(c.conflicted.len(), 1, "{c:#?}");
    exact("canon", &left, total(&c));

    // a conflicted block that is also changed is two rows: one changed and one conflicted (the
    // view keeps X as `l2` has it, so X differs from `l1` and is conflicted)
    let c = snap.compare(Some(&l1), &left).unwrap();
    let x = &c.conflicted[0].id;
    assert!(ids(&c.changed).contains(x), "{c:#?}");
    exact(&l1, &left, total(&c));

    // both sides' conflicts, each block once: left on X, right on U
    let c = snap.compare(Some(&left), &right).unwrap();
    assert_eq!(c.conflicted.len(), 2, "{c:#?}");
    exact(&left, &right, total(&c));

    // a view against itself lists its one conflict once, not once per side
    let c = snap.compare(Some(&left), &left).unwrap();
    assert_eq!(total(&c), 1, "{c:#?}");
    assert_eq!(
        size(&snap, Some(&left), &left, 10),
        CompareSize {
            count: 1,
            stopped: false
        }
    );
    assert_eq!(
        size(&snap, Some(&left), &left, 0),
        CompareSize {
            count: 0,
            stopped: true
        }
    );
    drop(snap);
    w.hub.close().unwrap();
}

fn same_refusal(snap: &Snapshot, from: Option<&str>, to: &str) -> RdyError {
    let a = snap.compare(from, to).unwrap_err();
    let b = snap.compare_size(from, to, 10).unwrap_err();
    assert_eq!(
        (a.code, &a.message, &a.fix),
        (b.code, &b.message, &b.fix),
        "{from:?} to {to}"
    );
    b
}

#[test]
fn cs1_size_refuses_as_compare_does() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = hub(&w);
    let snap = h.snapshot().unwrap();
    let none = Hash::of(b"a landing the hub never held");
    let none_text = landing(&none);
    let bad = [
        // an unknown scope text, on either side
        (Some("nowhere"), "canon".to_string()),
        (Some("canon"), "nowhere".to_string()),
        // an unknown view
        (Some("canon"), "view:nope".to_string()),
        (Some("view:nope"), "canon".to_string()),
        // a bad `view:<name>@<op>`
        (Some("canon"), "view:draft@zzz".to_string()),
        (Some("view:draft@zzz"), "canon".to_string()),
        (Some("canon"), format!("view:draft@{}", w.p1)),
        // an unknown landing
        (Some("canon"), landing(&none)),
        (Some(none_text.as_str()), "canon".to_string()),
    ];
    let mut codes = BTreeSet::new();
    for (from, to) in &bad {
        codes.insert(same_refusal(&snap, *from, to).code);
    }
    assert!(codes.len() >= 2, "{codes:?}");
    // the refusals of the empty side
    same_refusal(&snap, None, "nowhere");
    drop(snap);
    h.close().unwrap();
}

/// A hub that holds the landing `l1` of a project whose secret a later tag removes, with a
/// snapshot taken before the removal.
struct Removal {
    _dir: tempfile::TempDir,
    hub: StoreHandle,
    before: Snapshot,
    l1: Hash,
    t2: std::path::PathBuf,
    l2: Hash,
}

impl Removal {
    fn up(&self, path: &Path, tag: &str, landing: Hash) {
        self.hub
            .take(
                &[std::fs::File::open(path).unwrap()],
                rdy_adapter::take::UploadTarget {
                    tag: tag.into(),
                    landing,
                },
            )
            .unwrap();
    }

    /// The hub takes the tag that removes the secret.
    fn remove(&self) {
        self.up(&self.t2, "v2.0.0", self.l2);
    }
}

fn removal() -> Removal {
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(&dir.path().join("p"));
    let repo = repo_of(&r);
    let w = doc(&mut r, "work", "Intro", "Public words.");
    let secret = write_in(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": w.created["$s"], "body": "The launch code is ZEBRA-7731."}]),
    );
    let l1 = land(&mut r, "work");
    let v1 = tag(&mut r, "v1.0.0");
    let export = |r: &rdy_engine::Rdy, v: Hash, name: &str| {
        let mut out = Vec::new();
        rdy_engine::stream_export::write(&r.read(), v, &BTreeSet::new(), &mut out).unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, out).unwrap();
        path
    };
    let t1 = export(&r, v1, "t1.rdysync");
    let hub = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let up = |p: &Path, tag: &str, landing: Hash| {
        hub.take(
            &[std::fs::File::open(p).unwrap()],
            rdy_adapter::take::UploadTarget {
                tag: tag.into(),
                landing,
            },
        )
        .unwrap();
    };
    up(&t1, "v1.0.0", l1);
    let before = hub.snapshot().unwrap();
    // the project removes the secret in the next tag
    redact(&mut r, &secret.created["$x"]);
    let v2 = tag(&mut r, "v2.0.0");
    let l2 = r.store().read_tag(&v2).unwrap().landing;
    let t2 = export(&r, v2, "t2.rdysync");
    r.close().unwrap();
    Removal {
        _dir: dir,
        hub,
        before,
        l1,
        t2,
        l2,
    }
}

fn redact(r: &mut rdy_engine::Rdy, target: &str) {
    use rdy_engine::redaction::RedactInput;
    use rdy_model::{Provenance, Role};
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r
        .propose(&s, "work", "Remove the code", Some("test summary"))
        .unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(rdy_store::LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

#[test]
fn cs1_size_refuses_a_removal_after_the_snapshot() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = removal();
    let to = landing(&w.l1);
    // before the removal both calls read the landing
    let c = w.before.compare(None, &to).unwrap();
    assert!(total(&c) > 0, "{c:#?}");
    assert_eq!(size(&w.before, None, &to, 1_000).count, total(&c));
    w.remove();
    let e = same_refusal(&w.before, None, &to);
    assert_eq!(e.code, "E_REDACTED_STATE", "{e:#?}");
    let e = same_refusal(&w.before, Some("canon"), &to);
    assert_eq!(e.code, "E_REDACTED_STATE", "{e:#?}");
    // a snapshot taken after the removal reads again
    let after = w.hub.snapshot().unwrap();
    assert!(after.compare_size(None, &landing(&w.l2), 1_000).is_ok());
    drop((after, w.before));
    w.hub.close().unwrap();
}

#[test]
fn cs1_size_checks_the_epoch_again_after_the_count() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = removal();
    let to = landing(&w.l1);
    // a removal that lands while the count runs is refused, as it is for `compare`
    let e = w
        .before
        .compare_size_between_for_tests(None, &to, 1_000, || w.remove())
        .unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e:#?}");
    drop(w.before);
    w.hub.close().unwrap();
}
