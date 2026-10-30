//! dq1: what `rdy_diff` adds to a diff entry (D-HZYK0): its summary, the per-kind link rows, the
//! repinned ids, the frozen key sets, the cut to a root's subtree, and the status pass that only
//! the diff request runs.
use std::collections::{BTreeMap, BTreeSet};

use rdy_engine::diff::{
    Diff, DiffChange, DiffEntry, FieldChange, LinkChange, StatusChange, add_decision_status,
    diff_states, diff_states_under,
};
use rdy_engine::{DecisionInput, Materialized, Rdy, Scope, WriteRequest, compact_summary};
use rdy_model::{BlockId, EditorialClass, Hash, Justification};
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

type Ids = BTreeMap<String, String>;

fn id(ids: &Ids, key: &str) -> BlockId {
    BlockId::parse(&ids[key]).unwrap()
}

fn view(name: &str) -> Scope {
    Scope::View(name.into())
}

fn entry<'a>(d: &'a Diff, id: &BlockId) -> &'a DiffEntry {
    d.entries
        .iter()
        .find(|e| &e.id == id)
        .unwrap_or_else(|| panic!("no entry for {id}: {d:#?}"))
}

fn entry_ids(d: &Diff) -> Vec<BlockId> {
    d.entries.iter().map(|e| e.id.clone()).collect()
}

fn sorted(mut ids: Vec<BlockId>) -> Vec<BlockId> {
    ids.sort();
    ids
}

fn keys(v: &Value) -> BTreeSet<&str> {
    v.as_object()
        .unwrap_or_else(|| panic!("not an object: {v}"))
        .keys()
        .map(String::as_str)
        .collect()
}

fn key_set(want: &[&'static str]) -> BTreeSet<&'static str> {
    want.iter().copied().collect()
}

/// Folder `area` holding doc `one` (section `$s1` with `$p1`, `$kept`), folder `other`, and doc
/// `two` at the top level (`$p2`, `$gone`). Landed.
fn tree() -> (tempfile::TempDir, Rdy, Ids) {
    let (d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area", "title": "Area"}},
        {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "other", "title": "Other"}},
        {"op": "create", "type": "doc", "id": "$one", "parent": "$f", "fields": {"name": "one", "title": "One", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s1", "parent": "$one", "fields": {"title": "S1"}},
        {"op": "create", "type": "prose", "id": "$p1", "parent": "$s1", "body": "Old first. Old second."},
        {"op": "create", "type": "prose", "id": "$kept", "parent": "$s1", "body": "Kept as it was."},
        {"op": "create", "type": "doc", "id": "$two", "fields": {"name": "two", "title": "Two", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$p2", "parent": "$two", "body": "Second doc. It stays."},
        {"op": "create", "type": "prose", "id": "$gone", "parent": "$two", "body": "Gone soon. Really."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    (d, r, w.created)
}

/// A landed doc with a claim `$c` that links `$t1` and `$t2` under `depends_on` and `$e1` under
/// `describes` and `$e0` under `affects`, in a section with those targets and two more entities.
fn linked() -> (tempfile::TempDir, Rdy, Ids) {
    let (d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "linked", "title": "Linked", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$t1", "parent": "$s", "body": "Target one."},
        {"op": "create", "type": "claim", "id": "$t2", "parent": "$s", "body": "Target two."},
        {"op": "create", "type": "entity", "id": "$e0", "parent": "$s", "fields": {"name": "E0"}},
        {"op": "create", "type": "entity", "id": "$e1", "parent": "$s", "fields": {"name": "E1"}},
        {"op": "create", "type": "entity", "id": "$e2", "parent": "$s", "fields": {"name": "E2"}},
        {"op": "create", "type": "entity", "id": "$e3", "parent": "$s", "fields": {"name": "E3"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "A linked claim.",
         "links": {"depends_on": [{"target": "$t1"}, {"target": "$t2"}],
                   "describes": [{"target": "$e1"}],
                   "affects": [{"target": "$e0"}]}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    (d, r, w.created)
}

/// A landed doc whose section holds the prose `$before`, the decision `$old`, then the proses
/// `$gone` and `$p`. Returns the first landing's hash and the section's id too.
fn decided() -> (tempfile::TempDir, Rdy, Ids, Hash, String) {
    let (d, r) = project();
    decided_in(d, r)
}

fn decided_in(d: tempfile::TempDir, mut r: Rdy) -> (tempfile::TempDir, Rdy, Ids, Hash, String) {
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}},
        {"op": "create", "type": "prose", "id": "$before", "parent": "$s", "body": "Before it."}
    ]))
    .unwrap();
    let s = w.created["$s"].clone();
    let old = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Wallets move".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let after = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "id": "$gone", "parent": s, "body": "Gone soon."},
            {"op": "create", "type": "prose", "id": "$p", "parent": s, "body": "After it."}
        ]),
    )
    .unwrap();
    let l1: Hash = land(&mut r, "work").unwrap().parse().unwrap();
    let mut ids = w.created;
    ids.insert("$old".into(), old);
    ids.extend(after.created);
    (d, r, ids, l1, s)
}

fn supersede(r: &mut Rdy, view: &str, old: &str, s: &str) -> BlockId {
    let new = r
        .supersede(
            &writer(),
            view,
            old,
            None,
            DecisionInput {
                parent: s.to_string(),
                statement: "Wallets stay".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    BlockId::parse(&new).unwrap()
}

/// The diff from `from` to `to` with the status pass, as the diff request runs it.
fn with_status(r: &Rdy, from: &Scope, to: &Scope, root: Option<&BlockId>) -> Diff {
    let st = r.read();
    let (mf, mt) = (st.materialized(from).unwrap(), st.materialized(to).unwrap());
    let (gf, gt) = (st.graph(from).unwrap(), st.graph(to).unwrap());
    let mut d = diff_states_under(&mf, &mt, root);
    add_decision_status(&mut d, &mf, &mt, &gf, &gt, root);
    d
}

fn states(r: &Rdy, from: &Scope, to: &Scope) -> (Materialized, Materialized) {
    let st = r.read();
    let (mf, mt) = (st.materialized(from).unwrap(), st.materialized(to).unwrap());
    ((*mf).clone(), (*mt).clone())
}

#[test]
fn dq1_every_entry_carries_its_summary() {
    let (_d, mut r, ids) = tree();
    let (p1, gone, s1) = (id(&ids, "$p1"), id(&ids, "$gone"), id(&ids, "$s1"));
    let w = write(
        &mut r,
        "edit",
        json!([
            {"op": "update", "id": p1.to_string(), "body": "New first. New second."},
            {"op": "delete", "id": gone.to_string()},
            {"op": "create", "type": "prose", "id": "$new", "parent": s1.to_string(), "body": "Fresh one. Fresh two."}
        ]),
    )
    .unwrap();
    let new = id(&w.created, "$new");
    let d = r.read().diff(&Scope::Canon, &view("edit")).unwrap();
    let (from, to) = states(&r, &Scope::Canon, &view("edit"));

    let updated = entry(&d, &p1);
    assert_eq!(updated.change, DiffChange::Updated);
    assert_eq!(updated.summary, "New first.");
    assert_eq!(updated.summary, compact_summary(to.get(&p1).unwrap()));
    let added = entry(&d, &new);
    assert_eq!(added.change, DiffChange::Added);
    assert_eq!(added.summary, "Fresh one.");
    assert_eq!(added.summary, compact_summary(to.get(&new).unwrap()));
    let removed = entry(&d, &gone);
    assert_eq!(removed.change, DiffChange::Removed);
    assert_eq!(removed.summary, "Gone soon.");
    assert_eq!(removed.summary, compact_summary(from.get(&gone).unwrap()));
}

#[test]
fn dq1_links_list_each_changed_kind() {
    let (_d, mut r, ids) = linked();
    let c = id(&ids, "$c");
    let (t1, t2) = (id(&ids, "$t1"), id(&ids, "$t2"));
    let (e0, e1, e2, e3) = (
        id(&ids, "$e0"),
        id(&ids, "$e1"),
        id(&ids, "$e2"),
        id(&ids, "$e3"),
    );
    // given in the order that sorts last first, so the row's own sort shows
    let mut new_targets = [e2.clone(), e3.clone()];
    new_targets.sort();
    new_targets.reverse();
    write(
        &mut r,
        "relink",
        json!([{"op": "update", "id": c.to_string(), "links": {
            "depends_on": [{"target": t2.to_string()}],
            "describes": [{"target": e1.to_string()},
                          {"target": new_targets[0].to_string()},
                          {"target": new_targets[1].to_string()}],
            "affects": [{"target": e0.to_string()}]
        }}]),
    )
    .unwrap();
    let d = r.read().diff(&Scope::Canon, &view("relink")).unwrap();
    assert_eq!(
        entry(&d, &c).links,
        vec![
            LinkChange {
                rel: "depends_on".into(),
                added: vec![],
                removed: vec![t1],
            },
            LinkChange {
                rel: "describes".into(),
                added: sorted(vec![e2, e3]),
                removed: vec![],
            },
        ],
        "two rows in kind order; `affects` did not change"
    );
}

#[test]
fn dq1_a_part_change_lists_the_target_as_removed_and_added() {
    let (_d, mut r, ids) = linked();
    let (s, e1) = (id(&ids, "$s"), id(&ids, "$e1"));
    let w = write(
        &mut r,
        "start",
        json!([{"op": "create", "type": "claim", "id": "$p", "parent": s.to_string(), "body": "Parted.",
                "links": {"describes": [{"target": e1.to_string(), "part": "the old part"}]}}]),
    )
    .unwrap();
    land(&mut r, "start").unwrap();
    let p = id(&w.created, "$p");
    write(
        &mut r,
        "part",
        json!([{"op": "update", "id": p.to_string(), "links": {
            "describes": [{"target": e1.to_string(), "part": "the new part"}]
        }}]),
    )
    .unwrap();
    let d = r.read().diff(&Scope::Canon, &view("part")).unwrap();
    assert_eq!(
        entry(&d, &p).links,
        vec![LinkChange {
            rel: "describes".into(),
            added: vec![e1.clone()],
            removed: vec![e1],
        }]
    );
}

#[test]
fn dq1_added_and_removed_blocks_have_empty_links_and_repinned() {
    let (_d, mut r, ids) = linked();
    let (s, c, e1) = (id(&ids, "$s"), id(&ids, "$c"), id(&ids, "$e1"));
    let w = write(
        &mut r,
        "both",
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": s.to_string(), "body": "New.",
             "links": {"describes": [{"target": e1.to_string()}]}},
            {"op": "delete", "id": c.to_string()}
        ]),
    )
    .unwrap();
    let n = id(&w.created, "$n");
    let d = r.read().diff(&Scope::Canon, &view("both")).unwrap();
    for (block, change) in [(&n, DiffChange::Added), (&c, DiffChange::Removed)] {
        let e = entry(&d, block);
        assert_eq!(e.change, change);
        assert_eq!(e.links, vec![], "{block}");
        assert_eq!(e.repinned, Vec::<BlockId>::new(), "{block}");
    }
}

#[test]
fn dq1_a_pins_only_change_lists_repinned() {
    let (_d, mut r, ids) = linked();
    let (c, t1, t2) = (id(&ids, "$c"), id(&ids, "$t1"), id(&ids, "$t2"));
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": t2.to_string(), "body": "Target two, edited."},
               {"op": "update", "id": t1.to_string(), "body": "Target one, edited."}]),
    )
    .unwrap();
    land(&mut r, "edit").unwrap();
    write(
        &mut r,
        "again",
        json!([{"op": "repin", "id": c.to_string()}]),
    )
    .unwrap();
    let d = r.read().diff(&Scope::Canon, &view("again")).unwrap();
    assert_eq!(entry_ids(&d), vec![c.clone()]);
    let e = entry(&d, &c);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.repinned, sorted(vec![t1, t2]));
    assert!(e.fields.is_empty(), "{:?}", e.fields);
    assert!(e.links.is_empty(), "{:?}", e.links);
    assert_eq!(e.body, None);
    assert_eq!(e.moved, None);
    assert_eq!(e.type_change, None);
    assert_ne!(e.from_version, e.to_version, "a pin is part of the version");
}

#[test]
fn dq1_repinned_skips_a_pin_that_came_with_a_new_link() {
    let (_d, mut r, ids) = linked();
    let (c, e0, e1, e2, t1, t2) = (
        id(&ids, "$c"),
        id(&ids, "$e0"),
        id(&ids, "$e1"),
        id(&ids, "$e2"),
        id(&ids, "$t1"),
        id(&ids, "$t2"),
    );
    write(
        &mut r,
        "more",
        json!([{"op": "update", "id": c.to_string(), "links": {
            "depends_on": [{"target": t1.to_string()}, {"target": t2.to_string()}],
            "describes": [{"target": e1.to_string()}, {"target": e2.to_string()}],
            "affects": [{"target": e0.to_string()}]
        }}]),
    )
    .unwrap();
    let (from, to) = states(&r, &Scope::Canon, &view("more"));
    assert!(
        to.get(&c).unwrap().pins.contains_key(&e2) && !from.get(&c).unwrap().pins.contains_key(&e2),
        "the new link brought a pin on one side only"
    );
    let d = r.read().diff(&Scope::Canon, &view("more")).unwrap();
    let e = entry(&d, &c);
    assert_eq!(e.repinned, Vec::<BlockId>::new());
    assert_eq!(e.links.len(), 1, "{:?}", e.links);
}

#[test]
fn dq1_the_entry_json_has_the_frozen_keys() {
    let (_d, mut r, ids) = tree();
    let (kept, two, s1, one) = (
        id(&ids, "$kept"),
        id(&ids, "$two"),
        id(&ids, "$s1"),
        id(&ids, "$one"),
    );
    let w = write(
        &mut r,
        "shape",
        json!([
            {"op": "update", "id": kept.to_string(), "type": "note",
             "move_to": {"parent": two.to_string()}},
            {"op": "create", "type": "prose", "id": "$np", "parent": s1.to_string(), "body": "New here."},
            {"op": "create", "type": "section", "id": "$ns", "parent": s1.to_string(), "fields": {"title": "Empty"}}
        ]),
    )
    .unwrap();
    let d = r.read().diff(&Scope::Canon, &view("shape")).unwrap();
    let (_, to) = states(&r, &Scope::Canon, &view("shape"));

    // an updated entry with a move and a type change, with no body and no status
    let e = entry(&d, &kept);
    assert!(e.moved.is_some() && e.type_change.is_some());
    let e = serde_json::to_value(e).unwrap();
    assert_eq!(
        keys(&e),
        key_set(&[
            "id",
            "block_type",
            "doc",
            "path",
            "change",
            "summary",
            "from_version",
            "to_version",
            "moved",
            "type",
            "fields",
            "links",
            "repinned",
        ])
    );
    assert_eq!(
        keys(&e["moved"]),
        key_set(&[
            "from_parent",
            "to_parent",
            "from_path",
            "to_path",
            "reordered_only"
        ])
    );
    let (from, _) = states(&r, &Scope::Canon, &view("shape"));
    assert_eq!(
        e["from_version"],
        json!(from.get(&kept).unwrap().version_hash().to_string()),
        "a version is 64 hex characters, as O2's labels are"
    );
    assert_eq!(e["type"], json!({"from": "prose", "to": "note"}));
    assert_eq!(e["change"], json!("updated"));
    assert_eq!(e["fields"], json!([]));
    assert_eq!(e["links"], json!([]));
    assert_eq!(e["repinned"], json!([]));

    // a plain added paragraph: `doc`, `to_version`, `summary`, and the two nulls
    let np = id(&w.created, "$np");
    let e = serde_json::to_value(entry(&d, &np)).unwrap();
    assert_eq!(
        keys(&e),
        key_set(&[
            "id",
            "block_type",
            "doc",
            "path",
            "change",
            "summary",
            "from_version",
            "to_version",
            "fields",
            "links",
            "repinned",
            "body",
        ])
    );
    assert_eq!(e["doc"], json!(one.to_string()));
    assert_eq!(e["path"], Value::Null);
    assert_eq!(e["from_version"], Value::Null);
    assert_eq!(
        e["to_version"],
        json!(to.get(&np).unwrap().version_hash().to_string())
    );
    assert_eq!(e["body"], json!([{"op": "add", "text": "New here."}]));

    // no body: the key is left out, where `doc`, `path` and `from_version` stay
    let ns = id(&w.created, "$ns");
    let e = serde_json::to_value(entry(&d, &ns)).unwrap();
    assert!(e.get("body").is_none(), "{e}");
    assert!(e.get("moved").is_none() && e.get("type").is_none(), "{e}");
    assert!(e.get("decision_status").is_none(), "{e}");
    assert!(e.get("path").is_some() && e.get("from_version").is_some());
}

#[test]
fn dq1_a_field_row_leaves_out_the_side_it_lacks() {
    let row = |from, to| {
        serde_json::to_value(FieldChange {
            name: "n".into(),
            from,
            to,
        })
        .unwrap()
    };
    assert_eq!(row(None, Some(json!(1))), json!({"name": "n", "to": 1}));
    assert_eq!(row(Some(json!(1)), None), json!({"name": "n", "from": 1}));
    assert_eq!(
        row(Some(json!(1)), Some(json!(2))),
        json!({"name": "n", "from": 1, "to": 2})
    );
}

#[test]
fn dq1_an_added_block_outside_any_doc_has_a_null_doc() {
    let (_d, mut r, _ids) = tree();
    let w = write(
        &mut r,
        "out",
        json!([{"op": "create", "type": "question", "id": "$q", "fields": {"prompt": "Root?"}}]),
    )
    .unwrap();
    let q = id(&w.created, "$q");
    let d = r.read().diff(&Scope::Canon, &view("out")).unwrap();
    let e = serde_json::to_value(entry(&d, &q)).unwrap();
    assert_eq!(e["doc"], Value::Null);
    assert_eq!(e["summary"], json!("Root?"));
}

#[test]
fn dq1_a_subtree_diff_keeps_only_blocks_under_the_root() {
    let (_d, mut r, ids) = tree();
    let (one, p1, p2, gone, s1) = (
        id(&ids, "$one"),
        id(&ids, "$p1"),
        id(&ids, "$p2"),
        id(&ids, "$gone"),
        id(&ids, "$s1"),
    );
    let w = write(
        &mut r,
        "wide",
        json!([
            {"op": "update", "id": p1.to_string(), "body": "Changed in one."},
            {"op": "create", "type": "prose", "id": "$n", "parent": s1.to_string(), "body": "New in one."},
            {"op": "update", "id": p2.to_string(), "body": "Changed in two."},
            {"op": "delete", "id": gone.to_string()}
        ]),
    )
    .unwrap();
    let n = id(&w.created, "$n");
    let (from, to) = states(&r, &Scope::Canon, &view("wide"));
    let whole = diff_states_under(&from, &to, None);
    assert_eq!(whole, diff_states(&from, &to), "no root is the whole diff");
    assert_eq!(whole.entries.len(), 4);

    let cut = diff_states_under(&from, &to, Some(&one));
    assert_eq!(entry_ids(&cut), vec![p1.clone(), n.clone()]);
    assert_eq!(cut.counts.updated, 1);
    assert_eq!(cut.counts.added, 1);
    assert_eq!(cut.counts.removed, 0);
    assert_eq!(cut.counts.moved, 0);
    // the entries are the whole diff's entries
    assert_eq!(entry(&cut, &p1), entry(&whole, &p1));
    assert_eq!(entry(&cut, &n), entry(&whole, &n));
}

#[test]
fn dq1_a_doc_moved_into_the_root_brings_its_blocks_as_added() {
    let (_d, mut r, ids) = tree();
    let (f, two, p2, gone) = (
        id(&ids, "$f"),
        id(&ids, "$two"),
        id(&ids, "$p2"),
        id(&ids, "$gone"),
    );
    write(
        &mut r,
        "in",
        json!([{"op": "move", "id": two.to_string(), "parent": f.to_string()}]),
    )
    .unwrap();
    let (from, to) = states(&r, &Scope::Canon, &view("in"));
    let d = diff_states_under(&from, &to, Some(&f));
    assert_eq!(entry_ids(&d), vec![two.clone(), p2.clone(), gone.clone()]);
    assert_eq!(d.counts.added, 3);
    assert_eq!(d.counts.moved, 0);
    let e = entry(&d, &two);
    assert_eq!(e.change, DiffChange::Added);
    assert_eq!(e.from_version, None);
    let moved = e
        .moved
        .as_ref()
        .expect("the doc that moved names the other side");
    assert_eq!(moved.from_parent, None);
    assert_eq!(moved.to_parent.as_ref(), Some(&f));
    assert_eq!(moved.from_path, from.path_of(&two));
    assert_eq!(moved.to_path, to.path_of(&two));
    assert_ne!(moved.from_path, moved.to_path);
    assert!(!moved.reordered_only);
    let e = entry(&d, &p2);
    assert_eq!(e.change, DiffChange::Added);
    assert_eq!(e.moved, None, "its parent is the same on both sides");
}

#[test]
fn dq1_a_doc_moved_out_of_the_root_takes_its_blocks_as_removed() {
    let (_d, mut r, ids) = tree();
    let (f, g, one, s1, p1, kept) = (
        id(&ids, "$f"),
        id(&ids, "$g"),
        id(&ids, "$one"),
        id(&ids, "$s1"),
        id(&ids, "$p1"),
        id(&ids, "$kept"),
    );
    write(
        &mut r,
        "out",
        json!([{"op": "move", "id": one.to_string(), "parent": g.to_string()}]),
    )
    .unwrap();
    let (from, to) = states(&r, &Scope::Canon, &view("out"));
    let d = diff_states_under(&from, &to, Some(&f));
    // a removed subtree keeps `from`'s order, whatever `to` holds outside the root (rule 5)
    assert_eq!(
        entry_ids(&d),
        vec![one.clone(), s1.clone(), p1.clone(), kept.clone()]
    );
    assert_eq!(d.counts.removed, 4);
    assert_eq!(d.counts.moved, 0);
    let e = entry(&d, &one);
    assert_eq!(e.change, DiffChange::Removed);
    assert_eq!(e.to_version, None);
    let moved = e
        .moved
        .as_ref()
        .expect("the doc that moved names the other side");
    assert_eq!(moved.from_parent.as_ref(), Some(&f));
    assert_eq!(moved.to_parent.as_ref(), Some(&g));
    assert_eq!(moved.from_path, from.path_of(&one));
    assert_eq!(moved.to_path, to.path_of(&one));
    assert!(!moved.reordered_only);
    for child in [&s1, &p1, &kept] {
        let e = entry(&d, child);
        assert_eq!(e.change, DiffChange::Removed, "{child}");
        assert_eq!(e.moved, None, "{child}");
    }
    // the side it moved to sees it arrive
    let d = diff_states_under(&from, &to, Some(&g));
    assert_eq!(entry(&d, &one).change, DiffChange::Added);
    assert!(entry(&d, &one).moved.is_some());
}

#[test]
fn dq1_a_block_moved_out_of_the_root_is_removed_with_moved_set() {
    let (_d, mut r, ids) = tree();
    let (one, two, s1, p1) = (
        id(&ids, "$one"),
        id(&ids, "$two"),
        id(&ids, "$s1"),
        id(&ids, "$p1"),
    );
    write(
        &mut r,
        "away",
        json!([{"op": "move", "id": p1.to_string(), "parent": two.to_string()}]),
    )
    .unwrap();
    let (from, to) = states(&r, &Scope::Canon, &view("away"));
    let d = diff_states_under(&from, &to, Some(&one));
    assert_eq!(entry_ids(&d), vec![p1.clone()]);
    assert_eq!(d.counts.removed, 1);
    let e = entry(&d, &p1);
    assert_eq!(e.change, DiffChange::Removed);
    assert_eq!(e.doc.as_ref(), Some(&one), "the doc on the side it left");
    let moved = e.moved.as_ref().expect("the block that moved");
    assert_eq!(moved.from_parent.as_ref(), Some(&s1));
    assert_eq!(moved.to_parent.as_ref(), Some(&two));
    assert_eq!(moved.from_path, from.path_of(&p1));
    assert_eq!(moved.to_path, to.path_of(&p1));
    assert!(!moved.reordered_only);
}

#[test]
fn dq1_a_superseded_decision_gets_a_status_entry() {
    let (_d, mut r, ids, l1, s) = decided();
    let old = id(&ids, "$old");
    let new = supersede(&mut r, "next", &ids["$old"], &s);
    land(&mut r, "next").unwrap();
    let d = with_status(&r, &Scope::At(l1), &Scope::Canon, None);
    let (from, to) = states(&r, &Scope::At(l1), &Scope::Canon);

    let e = entry(&d, &old);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(
        e.decision_status,
        Some(StatusChange {
            from: "live".into(),
            to: "superseded".into()
        })
    );
    assert_eq!(e.summary, compact_summary(to.get(&old).unwrap()));
    assert_eq!(e.from_version, e.to_version, "its version did not change");
    assert_eq!(e.from_version, Some(from.get(&old).unwrap().version_hash()));
    assert_eq!(e.doc, entry(&d, &new).doc);
    assert!(e.fields.is_empty() && e.links.is_empty() && e.repinned.is_empty());
    assert_eq!(e.body, None);
    assert_eq!(e.moved, None);
    assert_eq!(e.type_change, None);
    assert_eq!(d.counts.updated, 1, "the counts count it");
    assert_eq!(d.counts.added, 1, "the new decision");
    assert_eq!(entry(&d, &new).decision_status, None, "one side only");
    // its place in `to`'s reading order
    let want: Vec<BlockId> = reading_order(&to)
        .into_iter()
        .filter(|x| *x == old || *x == new)
        .collect();
    assert_eq!(entry_ids(&d), want);
}

/// The depth-first reading order of `m`: each block, then its children in order.
fn reading_order(m: &Materialized) -> Vec<BlockId> {
    fn walk(m: &Materialized, parent: Option<&BlockId>, out: &mut Vec<BlockId>) {
        for b in m.children(parent) {
            out.push(b.id.clone());
            walk(m, Some(&b.id), out);
        }
    }
    let mut out = vec![];
    walk(m, None, &mut out);
    out
}

#[test]
fn dq1_a_status_entry_takes_its_place_among_the_entries() {
    let (_d, mut r, ids, l1, s) = decided();
    let (old, before, gone, p) = (
        id(&ids, "$old"),
        id(&ids, "$before"),
        id(&ids, "$gone"),
        id(&ids, "$p"),
    );
    let new = supersede(&mut r, "next", &ids["$old"], &s);
    // an entry on each side of the decision, and a removed block after it
    write(
        &mut r,
        "next",
        json!([
            {"op": "update", "id": before.to_string(), "body": "Before it, edited."},
            {"op": "delete", "id": gone.to_string()},
            {"op": "update", "id": p.to_string(), "body": "After it, edited."}
        ]),
    )
    .unwrap();
    land(&mut r, "next").unwrap();
    let d = with_status(&r, &Scope::At(l1), &Scope::Canon, None);
    let (_, to) = states(&r, &Scope::At(l1), &Scope::Canon);
    // the removed block goes right before the block that followed it
    let mut want = vec![];
    for x in reading_order(&to) {
        if x == p {
            want.push(gone.clone());
        }
        if [&before, &old, &p, &new].contains(&&x) {
            want.push(x);
        }
    }
    assert_eq!(entry_ids(&d), want);
    assert_eq!(d.counts.updated, 3);
    assert_eq!(d.counts.added, 1);
    assert_eq!(d.counts.removed, 1);
}

/// A pack whose decisions carry the tracking field `disposition`, the one edit a landed decision
/// takes (trk1).
const TRACKING_PACK: &str = "name: trk\nversion: '1'\nextends: rdy-standard\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      disposition: { kind: str }\n    tracking: [disposition]\n";

#[test]
fn dq1_an_edited_decision_carries_its_status_change() {
    let (dir, r) = project();
    // the store is locked while a handle is open
    drop(r);
    rdy_qa::packs::use_pack(dir.path(), "trk", TRACKING_PACK, "name: Tracking\n");
    let r = Rdy::open(dir.path()).unwrap();
    let (_d, mut r, ids, l1, s) = decided_in(dir, r);
    let old = id(&ids, "$old");
    supersede(&mut r, "next", &ids["$old"], &s);
    r.write(
        &writer(),
        WriteRequest {
            view: "next".into(),
            ops: parse_test_ops(
                json!([{"op": "update", "id": old.to_string(), "fields": {"disposition": "M3"}}]),
            )
            .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "re-sliced".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    land(&mut r, "next").unwrap();
    let d = with_status(&r, &Scope::At(l1), &Scope::Canon, None);
    let e = entry(&d, &old);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(
        e.fields,
        vec![FieldChange {
            name: "disposition".into(),
            from: None,
            to: Some(json!("M3")),
        }]
    );
    assert_eq!(
        e.decision_status,
        Some(StatusChange {
            from: "live".into(),
            to: "superseded".into()
        })
    );
    assert_ne!(e.from_version, e.to_version);
    assert_eq!(
        d.entries.iter().filter(|x| x.id == old).count(),
        1,
        "one entry for the block"
    );
    assert_eq!(d.counts.updated, 1);
}

#[test]
fn dq1_the_status_pass_keeps_inside_the_root() {
    let (_d, mut r, ids, l1, s) = decided();
    let old = id(&ids, "$old");
    let w = write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "doc", "id": "$else", "fields": {"name": "else", "title": "Else", "doc_type": "overview"}}]),
    )
    .unwrap();
    let elsewhere = id(&w.created, "$else");
    supersede(&mut r, "next", &ids["$old"], &s);
    land(&mut r, "next").unwrap();
    let inside = with_status(&r, &Scope::At(l1), &Scope::Canon, Some(&id(&ids, "$d")));
    assert!(entry(&inside, &old).decision_status.is_some());
    let outside = with_status(&r, &Scope::At(l1), &Scope::Canon, Some(&elsewhere));
    assert_eq!(entry_ids(&outside), vec![elsewhere]);
}

#[test]
fn dq1_the_renderer_paths_carry_no_status() {
    let (_d, mut r, ids, l1, s) = decided();
    let old = id(&ids, "$old");
    supersede(&mut r, "next", &ids["$old"], &s);
    land(&mut r, "next").unwrap();
    let (from_scope, to_scope) = (Scope::At(l1), Scope::Canon);
    let (from, to) = states(&r, &from_scope, &to_scope);
    let plain = diff_states(&from, &to);
    let read = r.read().diff(&from_scope, &to_scope).unwrap();
    let full = with_status(&r, &from_scope, &to_scope, None);
    for d in [&plain, &read] {
        assert!(d.entries.iter().all(|e| e.decision_status.is_none()));
        assert!(
            d.entries.iter().all(|e| e.id != old),
            "no status-only entry"
        );
        assert_eq!(d.counts.updated, full.counts.updated - 1);
        assert_eq!(d.counts.added, full.counts.added);
    }
    assert_eq!(entry_ids(&plain), entry_ids(&read));
    assert_eq!(plain.counts, read.counts);
    assert!(entry(&full, &old).decision_status.is_some());
}
