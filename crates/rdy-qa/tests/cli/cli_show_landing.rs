//! dq7: `rdy show` of a landing: the row, then its change list (O13, O14).
use crate::cli_diff::world;
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;

fn short(h: &str) -> &str {
    &h[..12]
}

/// The world with a second landing, tagged `v1.0.0`: returns the claim it adds and its landing.
fn two_landings(w: &crate::cli_diff::World) -> (String, String) {
    let mut r = Rdy::open(w.s.dir.path()).unwrap();
    let c = write(&mut r, "two", json!([
        {"op": "create", "type": "claim", "id": "$c", "parent": w.beta, "body": "Second landing."}
    ]))
    .unwrap()
    .created["$c"]
        .clone();
    let l2 = land(&mut r, "two").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
    (c, l2)
}

fn stdout(s: &Served, args: &[&str]) -> String {
    let out = s.rdy(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn stderr(s: &Served, args: &[&str]) -> String {
    let out = s.rdy(args);
    assert!(!out.status.success(), "{args:?} should be refused");
    String::from_utf8_lossy(&out.stderr).trim_end().to_string()
}

#[test]
fn dq7_a_full_landing_hash_shows_the_landing() {
    let w = world();
    let (c, l2) = two_landings(&w);
    let (ok, v) = w.s.json(&["--json", "show", &l2]);
    assert!(ok, "{v}");
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["diff", "landing"]);
    assert_eq!(v["landing"]["landing"], l2.as_str());
    assert_eq!(v["diff"]["entries"][0]["id"], c.as_str());
    // the text: the `rdy log` row's lines, then the change list
    let log = stdout(&w.s, &["log", "--limit", "1"]);
    let text = stdout(&w.s, &["show", &l2]);
    let (head, rest) = text.split_once("\nlanding:").unwrap();
    assert_eq!(head, log);
    assert!(
        rest.starts_with(&format!(
            "{} to landing:{}: 1 added, 0 removed, 0 updated, 0 moved\nadded {c} (claim) ",
            short(&w.l1),
            short(&l2)
        )),
        "{text}"
    );
    // the row is the log's row
    let (_, rows) = w.s.json(&["--json", "log"]);
    assert_eq!(rows[0], v["landing"]);
    w.s.stop();
}

#[test]
fn dq7_a_landing_prefix_landing_form_and_tag_show_the_landing() {
    let w = world();
    let (_, l2) = two_landings(&w);
    let (_, whole) = w.s.json(&["--json", "show", &l2]);
    for id in [
        l2[..8].to_string(),
        format!("landing:{}", &l2[..10]),
        format!("landing:{l2}"),
        "tag:v1.0.0".to_string(),
    ] {
        let (ok, v) = w.s.json(&["--json", "show", &id]);
        assert!(ok, "{id}: {v}");
        assert_eq!(v, whole, "{id}");
    }
    w.s.stop();
}

#[test]
fn dq7_the_first_landing_diffs_from_the_empty_project() {
    let w = world();
    let text = stdout(&w.s, &["show", &w.l1]);
    assert!(
        text.contains(&format!(
            "\nthe empty project to landing:{}: ",
            short(&w.l1)
        )),
        "{text}"
    );
    let (_, v) = w.s.json(&["--json", "show", &w.l1]);
    assert_eq!(v["diff"]["from"], json!({"kind": "empty"}));
    w.s.stop();
}

#[test]
fn dq7_blame_with_a_landing_is_refused() {
    let w = world();
    assert_eq!(
        stderr(&w.s, &["show", &w.l1, "--blame"]),
        "error E_BAD_INPUT: blame applies to blocks, not landings\nfix: drop --blame to read the landing, or pass a block id"
    );
    w.s.stop();
}

#[test]
fn dq7_a_hash_that_is_neither_is_refused_in_show() {
    let w = world();
    let hash = "ab".repeat(32);
    assert_eq!(
        stderr(&w.s, &["show", &hash]),
        format!(
            "error E_BAD_INPUT: {hash} is not a proposal version or a landing\nfix: list landings with rdy log, or pass P-<id>@<n>"
        )
    );
    w.s.stop();
}

#[test]
fn dq7_a_proposal_version_still_shows_as_before() {
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
    let summary = rdy_qa::intake::summary_for(&r, "prop");
    let p = r.propose(&writer(), "prop", "Prop", summary).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap();
    let (name, version) = (pref.to_string(), pref.version.to_string());
    r.close().unwrap();
    let (ok, by_ref) = w.s.json(&["--json", "show", &name]);
    assert!(ok, "{by_ref}");
    assert!(by_ref["proposal"].is_object() && by_ref.get("landing").is_none());
    let text = stdout(&w.s, &["show", &name]);
    assert_eq!(
        text,
        rdy_engine::proposal_text(&by_ref["proposal"]).trim_end_matches('\n')
    );
    // the version's hash and a prefix of it show the same proposal
    let (_, by_hash) = w.s.json(&["--json", "show", &version]);
    assert_eq!(
        by_hash["proposal"]["version"],
        by_ref["proposal"]["version"]
    );
    let (_, by_prefix) = w.s.json(&["--json", "show", &version[..10]]);
    assert_eq!(
        by_prefix["proposal"]["version"],
        by_ref["proposal"]["version"]
    );
    w.s.stop();
}

/// `rdy approve` reads the version through `show`, which also reads a landing since dq7: a landing
/// hash is refused by the approval in its own words, not as an internal error.
#[test]
fn dq7_approving_a_landing_hash_is_not_found() {
    let w = world();
    let err = stderr(&w.s, &["approve", &w.l1]);
    assert!(err.starts_with("error E_NOT_FOUND"), "{err}");
    w.s.stop();
}
