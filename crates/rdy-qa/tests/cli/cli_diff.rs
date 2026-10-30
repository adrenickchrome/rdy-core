//! dq7: `rdy diff` through the daemon: the text of O16, its scope defaults, the root and the JSON.
use rdy_engine::{DiffRequest, Rdy};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

/// A served project with a landed doc `guide` (sections `Alpha` and `Beta`; claims `old` and
/// `mover` under Alpha), the ids, and the landing.
pub(crate) struct World {
    pub s: Served,
    pub doc: String,
    pub alpha: String,
    pub beta: String,
    pub old: String,
    pub mover: String,
    pub l1: String,
}

pub(crate) fn world() -> World {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "Alpha"}},
        {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "Beta"}},
        {"op": "create", "type": "claim", "id": "$old", "parent": "$a", "body": "Line one.\nLine two old."},
        {"op": "create", "type": "claim", "id": "$m", "parent": "$a", "body": "Mover."}
    ]))
    .unwrap();
    let l1 = land(&mut r, "seed").unwrap();
    r.close().unwrap();
    World {
        s,
        doc: w.created["$d"].clone(),
        alpha: w.created["$a"].clone(),
        beta: w.created["$b"].clone(),
        old: w.created["$old"].clone(),
        mover: w.created["$m"].clone(),
        l1,
    }
}

/// View `work`: one claim added, one body updated, one claim moved to the other section.
pub(crate) fn work(w: &World) -> String {
    let mut r = Rdy::open(w.s.dir.path()).unwrap();
    let res = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": w.alpha, "body": "Fresh."},
            {"op": "update", "id": w.old, "body": "Line one.\nLine two new."},
            {"op": "move", "id": w.mover, "parent": w.beta}
        ]),
    )
    .unwrap();
    r.close().unwrap();
    res.created["$n"].clone()
}

fn short(h: &str) -> &str {
    &h[..12]
}

fn stdout(w: &World, args: &[&str]) -> String {
    let out = w.s.rdy(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn stderr(w: &World, args: &[&str]) -> String {
    let out = w.s.rdy(args);
    assert!(!out.status.success(), "{args:?} should be refused");
    String::from_utf8_lossy(&out.stderr).trim_end().to_string()
}

fn entry<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap_or_else(|| panic!("no entry {id} in {v}"))
}

#[test]
fn dq7_rdy_diff_prints_a_view_s_work() {
    let w = world();
    let n = work(&w);
    let (ok, v) = w.s.json(&["--json", "diff", "work"]);
    assert!(ok, "{v}");
    let text = stdout(&w, &["diff", "work"]);
    let line = |id: &str, change: &str| {
        let e = entry(&v, id);
        format!(
            "{change} {id} (claim) {}: {}",
            e["doc"].as_str().unwrap(),
            e["summary"].as_str().unwrap()
        )
    };
    let path = entry(&v, &w.mover)["moved"]["from_path"].as_str().unwrap();
    let want = [
        format!(
            "landing:{} to view:work: 1 added, 0 removed, 1 updated, 1 moved",
            short(&w.l1)
        ),
        line(&w.old, "updated"),
        line(&n, "added"),
        line(&w.mover, "moved"),
        format!("    moved from {path} to {path}"),
    ]
    .join("\n");
    assert_eq!(text, want);
    // the order of the lines is the reading order of the newer side
    let ids: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [w.old.as_str(), n.as_str(), w.mover.as_str()]);
    w.s.stop();
}

#[test]
fn dq7_rdy_diff_detail_prints_values_and_the_word_diff() {
    let w = world();
    work(&w);
    let mut r = Rdy::open(w.s.dir.path()).unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": w.alpha, "fields": {"title": "Alpha again"}}
        ]),
    )
    .unwrap();
    r.close().unwrap();
    let compact = stdout(&w, &["diff", "work"]);
    assert!(compact.contains("\n    fields title"), "{compact}");
    assert!(!compact.contains("field title:"), "{compact}");
    assert!(!compact.contains("[-"), "{compact}");
    let text = stdout(&w, &["diff", "work", "--detail"]);
    assert!(
        text.contains("\n    field title: \"Alpha\" to \"Alpha again\"\n"),
        "{text}"
    );
    // a body with a line break continues on a line indented 8 spaces
    assert!(
        text.contains("\n    body Line one.\n        Line two [-old-]{+new+}.\n"),
        "{text}"
    );
    w.s.stop();
}

#[test]
fn dq7_rdy_diff_doc_cuts_both_sides() {
    let w = world();
    work(&w);
    let mut r = Rdy::open(w.s.dir.path()).unwrap();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}}
    ]))
    .unwrap();
    r.close().unwrap();
    let all = stdout(&w, &["diff", "work"]);
    assert!(all.contains("(doc) other"), "{all}");
    let cut = stdout(&w, &["diff", "work", "--doc", "guide"]);
    assert!(
        cut.lines()
            .next()
            .unwrap()
            .contains(&format!(" in {}: ", w.doc)),
        "{cut}"
    );
    assert!(!cut.contains("(doc)"), "{cut}");
    assert!(cut.contains("updated"), "{cut}");
    w.s.stop();
}

#[test]
fn dq7_rdy_diff_one_scope_defaults() {
    let w = world();
    // a view reads from its base landing
    work(&w);
    let head = stdout(&w, &["diff", "work"]);
    assert!(
        head.starts_with(&format!("landing:{} to view:work: ", short(&w.l1))),
        "{head}"
    );
    // canon diffs from its previous landing; a tag diffs to canon
    w.s.stop();
    let mut r = Rdy::open(w.s.dir.path()).unwrap();
    let c2 = write(&mut r, "two", json!([
        {"op": "create", "type": "claim", "id": "$c", "parent": w.beta, "body": "Second landing."}
    ]))
    .unwrap()
    .created["$c"]
        .clone();
    let l2 = land(&mut r, "two").unwrap();
    r.tag(&approver(), "v0.1.0", Some(&w.l1), None).unwrap();
    r.close().unwrap();
    let canon = stdout(&w, &["diff", "canon"]);
    assert!(
        canon.starts_with(&format!(
            "landing:{} to canon: 1 added, 0 removed, 0 updated, 0 moved\nadded {c2} (claim) ",
            short(&w.l1)
        )),
        "{canon}"
    );
    let tag = stdout(&w, &["diff", "tag:v0.1.0"]);
    assert!(
        tag.starts_with("tag:v0.1.0 to canon: 1 added, 0 removed, 0 updated, 0 moved\n"),
        "{tag}"
    );
    let landing = stdout(&w, &["diff", &format!("landing:{l2}")]);
    assert!(
        landing.starts_with(&format!("landing:{} to canon: 0 added", short(&l2))),
        "{landing}"
    );
    w.s.stop();
}

#[test]
fn dq7_rdy_diff_labels_a_proposal_as_a_scope() {
    let w = world();
    let mut r = Rdy::open(w.s.dir.path()).unwrap();
    write(
        &mut r,
        "prop",
        json!([
            {"op": "create", "type": "claim", "id": "$c", "parent": w.beta, "body": "Proposed."}
        ]),
    )
    .unwrap();
    let summary = rdy_qa::intake::summary_for(&mut r, "prop");
    let p = r.propose(&writer(), "prop", "Prop", summary).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    r.close().unwrap();
    let text = stdout(&w, &["diff", &format!("proposal:{pref}")]);
    assert!(
        text.starts_with(&format!(
            "landing:{} to proposal:{pref}: 1 added, 0 removed, 0 updated, 0 moved",
            short(&w.l1)
        )),
        "{text}"
    );
    assert!(pref.starts_with("P-") && pref.contains('@'), "{pref}");
    w.s.stop();
}

#[test]
fn dq7_rdy_diff_inflight_alone_is_refused() {
    let w = world();
    let err = stderr(&w, &["diff", "inflight"]);
    assert_eq!(
        err,
        "error E_BAD_INPUT: inflight needs both sides\nfix: name both scopes: rdy_diff {from, to} / rdy diff <from> <to>"
    );
    w.s.stop();
}

#[test]
fn dq7_rdy_diff_json_is_the_whole_reply() {
    let w = world();
    work(&w);
    let (ok, v) = w.s.json(&["--json", "diff", "work"]);
    assert!(ok, "{v}");
    let (ok, detail) = w.s.json(&["--json", "diff", "work", "--detail"]);
    assert!(ok, "{detail}");
    w.s.stop();
    let r = Rdy::open(w.s.dir.path()).unwrap();
    let read = r.read();
    let req = |detail| DiffRequest {
        from: "work".into(),
        detail,
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(read.diff_reply(&req(false), None).unwrap()).unwrap(),
        v
    );
    assert_eq!(
        serde_json::to_value(read.diff_reply(&req(true), None).unwrap()).unwrap(),
        detail
    );
    assert!(
        v.get("next").is_none() && v.get("truncated").is_none(),
        "{v}"
    );
}

#[test]
fn dq7_no_changes_prints_a_line() {
    let w = world();
    assert_eq!(
        stdout(&w, &["diff", "canon", "canon"]),
        "canon to canon: 0 added, 0 removed, 0 updated, 0 moved\nno changes"
    );
    w.s.stop();
}
