//! Working-copy helpers for the `wc` area (M2f-a): a served project seeded through the engine,
//! file access relative to its root, and `rdy` runs that must succeed.
use crate::daemon::Served;
use crate::fixtures::*;
use rdy_engine::{DecisionInput, Rdy, Scope, WriteResult};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The seeded tree's ids.
pub struct Seed {
    pub folder: String,
    pub intro: String,
    pub section: String,
    pub prose: String,
    pub claim: String,
    pub decision: String,
    pub notes: String,
    pub note: String,
}

/// A served project whose canon (and view `work`) holds folder `guides/` with doc `intro`
/// (doc_type page): section `Start` holding prose `Hello.`, then claim `Claims hold.` (describes
/// the prose, decided_by the decision), then decision `Keep it`; and top-level doc `notes`
/// (doc_type page) holding prose `Note.`. Seeded in-process, landed and closed: no daemon runs yet.
pub fn seeded() -> (Served, Seed) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).expect("open the served project");
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "guides", "title": "Guides"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Start"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Hello."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "after": "$p", "body": "Claims hold.", "links": {"describes": [{"target": "$p"}]}},
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$np", "parent": "$n", "body": "Note."}
    ]))
    .expect("seed the tree");
    let id = |k: &str| w.created[k].clone();
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: id("$s"),
                statement: "Keep it".into(),
                ..Default::default()
            },
        )
        .expect("decide");
    let decision = d.created["$decision"].clone();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$c"), "links": {"decided_by": [{"target": decision}]}}]),
    )
    .expect("decided_by");
    land(&mut r, "work").expect("land the seed");
    r.close().expect("close");
    let seed = Seed {
        folder: id("$f"),
        intro: id("$d"),
        section: id("$s"),
        prose: id("$p"),
        claim: id("$c"),
        decision,
        notes: id("$n"),
        note: id("$np"),
    };
    (s, seed)
}

/// `rdy <args>` as the human; panics with its output unless it succeeds; returns stdout.
pub fn ok(s: &Served, args: &[&str]) -> String {
    let o = s.rdy(args);
    assert!(
        o.status.success(),
        "rdy {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// `rdy --json <args>` as the human: whether it succeeded, and its JSON.
pub fn json(s: &Served, args: &[&str]) -> (bool, Value) {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    s.json(&a)
}

/// A path in the project root.
pub fn at(s: &Served, rel: &str) -> PathBuf {
    s.dir.path().join(rel)
}

pub fn read(s: &Served, rel: &str) -> String {
    std::fs::read_to_string(at(s, rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Writes a file in the project root, creating its directories.
pub fn put(s: &Served, rel: &str, text: &str) {
    let p = at(s, rel);
    std::fs::create_dir_all(p.parent().expect("a parent")).expect("dirs");
    std::fs::write(p, text).expect("write");
}

/// The entries of a doc file.
pub fn entries(s: &Served, rel: &str) -> Vec<serde_yaml_ng::Value> {
    serde_yaml_ng::from_str(&read(s, rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The entry of block `id` in a doc file.
pub fn entry(s: &Served, rel: &str, id: &str) -> serde_yaml_ng::Value {
    entries(s, rel)
        .into_iter()
        .find(|e| e["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("{id} not in {rel}"))
}

/// Edits the entries of a doc file and writes them back.
pub fn edit(s: &Served, rel: &str, f: impl FnOnce(&mut Vec<serde_yaml_ng::Value>)) {
    let mut list = entries(s, rel);
    f(&mut list);
    std::fs::write(at(s, rel), serde_yaml_ng::to_string(&list).expect("yaml")).expect("write");
}

/// Sets the body of block `id` in a doc file.
pub fn set_body(s: &Served, rel: &str, id: &str, body: &str) {
    edit(s, rel, |list| {
        let e = list
            .iter_mut()
            .find(|e| e["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("{id} not in {rel}"));
        e.as_mapping_mut()
            .expect("an entry")
            .insert("body".into(), serde_yaml_ng::Value::String(body.into()));
    });
}

/// Writes `ops` into `view` with an in-process Rdy, after stopping the daemon (the next rdy
/// command starts it again).
pub fn store_write(s: &Served, view: &str, ops: Value) -> WriteResult {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).expect("open");
    let w = write(&mut r, view, ops).expect("store write");
    r.close().expect("close");
    w
}

/// Proposes and lands `view` in-process, after stopping the daemon. Returns the landing id.
pub fn store_land(s: &Served, view: &str) -> String {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).expect("open");
    let l = land(&mut r, view).expect("land");
    r.close().expect("close");
    l
}

/// Block `id` in `view`, read in-process after stopping the daemon.
pub fn store_block(s: &Served, view: &str, id: &str) -> rdy_model::Block {
    s.stop();
    let r = Rdy::open(s.dir.path()).expect("open");
    let b = {
        let st = r.read();
        st.block(
            &Scope::View(view.into()),
            &rdy_model::BlockId::parse(id).expect("an id"),
        )
        .expect("the block")
    };
    r.close().expect("close");
    b
}
