//! ab6 (D-WJ0SJ, FW-PB2): the publication check takes an optional `release`, the candidate's
//! product version. It lists the shown content that is `since` a later version and the retained
//! decisions `built` in one; a later one blocks the result for either audience. The report adds
//! the aggregate `status`; `references.status` stays what it was. The check reads one snapshot
//! and writes nothing.
mod common;

use rdy_engine::publication;
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    common::answer_open_items(r, &session(Role::Writer), view);
    r.propose(
        &session(Role::Writer),
        view,
        "release check",
        Some(common::TEST_SUMMARY),
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) {
    let p = propose(r, view);
    common::confirm_exceptions(r, &session(Role::Approver), &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
}

fn id(w: &WriteResult, name: &str) -> String {
    w.created[name].clone()
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

const VERSION_FIX: &str = "Use a version such as 1.2.0 or 2.0.0-rc.1; one leading v is allowed, and +build metadata is not.";

fn check(r: &Rdy, profile: &str, scope: Option<&str>, release: Option<&str>) -> Value {
    publication::check(&r.read(), profile, scope, None, release).unwrap()
}

/// The `(id, field)` of every release row, in report order.
fn rows(v: &Value) -> Vec<(String, String)> {
    v["release"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["field"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

struct Fixture {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: std::collections::BTreeMap<String, String>,
}

impl Fixture {
    fn id(&self, name: &str) -> &str {
        &self.ids[name]
    }
    /// The expected rows for the named blocks, sorted by full id then field.
    fn expect(&self, items: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = items
            .iter()
            .map(|(n, f)| (self.id(n).to_string(), f.to_string()))
            .collect();
        out.sort();
        out
    }
}

/// A canon with guide content of every version shape and four profiles: `pub` (public, planned
/// hidden), `orgm` (org, planned marked, since off), and a tag `v1.0.0` before the view `edit`
/// adds content since 5.0.0.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ids = std::collections::BTreeMap::new();
    fn take(ids: &mut std::collections::BTreeMap<String, String>, w: &WriteResult, names: &[&str]) {
        for n in names {
            ids.insert(n.trim_start_matches('$').to_string(), id(w, n));
        }
    }
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$d_old", "fields": {"statement": "Old.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$d_late", "fields": {"statement": "Late.", "status": "proposed", "built": "2.0.0"}},
            {"op": "create", "type": "decision", "id": "$d_plan", "fields": {"statement": "Plan.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$d_none", "fields": {"statement": "None.", "status": "proposed", "built": "none"}},
            {"op": "create", "type": "decision", "id": "$d_hid", "fields": {"statement": "Hidden.", "status": "proposed", "built": "3.0.0"}}
        ]),
    );
    take(
        &mut ids,
        &w,
        &[
            "$guide", "$admin", "$d_old", "$d_late", "$d_plan", "$d_none", "$d_hid",
        ],
    );
    let (guide, admin) = (ids["guide"].clone(), ids["admin"].clone());
    let dec = |ids: &std::collections::BTreeMap<String, String>, n: &str| ids[n].clone();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "section", "id": "$sec", "parent": guide, "fields": {"title": "Later", "planned": true}},
            {"op": "create", "type": "claim", "id": "$c_lower", "parent": guide, "body": "Lower.", "fields": {"since": "1.0.0"}},
            {"op": "create", "type": "claim", "id": "$c_equal", "parent": guide, "body": "Equal.", "fields": {"since": "1.2.0"}},
            {"op": "create", "type": "claim", "id": "$c_high", "parent": guide, "body": "High.", "fields": {"since": "1.10.0"}},
            {"op": "create", "type": "claim", "id": "$c_v", "parent": guide, "body": "V.", "fields": {"since": "v1.3.0"}},
            {"op": "create", "type": "claim", "id": "$c_pre", "parent": guide, "body": "Pre.", "fields": {"since": "1.2.0-rc.1"}},
            {"op": "create", "type": "claim", "id": "$c_late", "parent": guide, "body": "Late one.", "links": {"decided_by": [dec(&ids, "d_late")]}},
            {"op": "create", "type": "claim", "id": "$c_late2", "parent": guide, "body": "Late two.", "links": {"decided_by": [dec(&ids, "d_late")]}},
            {"op": "create", "type": "claim", "id": "$c_old", "parent": guide, "body": "Old.", "links": {"decided_by": [dec(&ids, "d_old")]}},
            {"op": "create", "type": "claim", "id": "$c_none", "parent": guide, "body": "None.", "links": {"decided_by": [dec(&ids, "d_none")]}},
            {"op": "create", "type": "claim", "id": "$c_plan", "parent": guide, "body": "Plan.", "links": {"decided_by": [dec(&ids, "d_plan")]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "pub", "title": "Pub", "audience": "public", "select": [guide]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "orgm", "title": "Org", "audience": "org", "planned": "mark", "since": "off", "select": [guide]}}
        ]),
    );
    take(
        &mut ids,
        &w,
        &[
            "$sec", "$c_lower", "$c_equal", "$c_high", "$c_v", "$c_pre", "$c_late", "$c_late2",
            "$c_old", "$c_none", "$c_plan",
        ],
    );
    let sec = ids["sec"].clone();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "claim", "id": "$c_inner", "parent": sec, "body": "Inner.", "fields": {"since": "9.0.0"}},
            {"op": "create", "type": "claim", "id": "$c_hid", "parent": sec, "body": "Hidden.", "links": {"decided_by": [dec(&ids, "d_hid")]}}
        ]),
    );
    take(&mut ids, &w, &["$c_inner", "$c_hid"]);
    land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    Fixture { _dir: dir, r, ids }
}

#[test]
fn ab6_release_matrix_checks_the_shown_scope() {
    let mut f = fixture();
    let base = [
        ("c_high", "since"),
        ("c_v", "since"),
        ("c_late", "since"),
        ("c_late2", "since"),
        ("c_inner", "since"),
        ("d_late", "built"),
    ];

    // semantic order: 1.10.0 is after 1.9.0 and 1.2.0; own since wins over an inherited planned
    // section; a decision counts once however many claims it ships; the prefix v is only spelling
    for profile in ["pub", "orgm"] {
        let v = check(&f.r, profile, None, Some("1.2.0"));
        // a supporting decision keeps its closure: the hidden claim's decision is retained, so
        // its later built is a row even where the claim itself is hidden
        let mut want = base.to_vec();
        want.push(("d_hid", "built"));
        assert_eq!(rows(&v), f.expect(&want), "{profile}: {v}");
        assert_eq!(v["release"]["version"], "1.2.0");
        assert_eq!(v["release"]["status"], "blocked");
        assert_eq!(v["release"]["blocks_later"], 5);
        assert_eq!(v["release"]["decisions_later"], 2);
        assert_eq!(v["release"]["omitted_rows"], 0);
        // a version violation never reads as a reference violation
        assert_eq!(v["references"]["status"], "clear");
        assert_eq!(v["references"]["total"], 0);
        assert_eq!(v["status"], "blocked", "either audience: {profile}");
        // the planned claim is shown by mark and is no row; the hidden one never is
        for gone in [
            "c_plan", "d_plan", "c_hid", "c_lower", "c_equal", "c_pre", "c_old",
        ] {
            assert!(
                !rows(&v).iter().any(|(i, _)| i == f.id(gone)),
                "{profile}: {gone}"
            );
        }
        assert_eq!(check(&f.r, profile, None, Some("v1.2.0")), v);
    }

    let v = check(&f.r, "pub", None, Some("1.2.0"));
    let row = v["release"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == f.id("c_high"))
        .unwrap();
    assert_eq!(
        row,
        &json!({
            "id": f.id("c_high"),
            "field": "since",
            "version": "1.10.0",
            "message": format!("Block {} is since 1.10.0, after release 1.2.0.", f.id("c_high")),
            "fix": "Check built and since at this scope. Correct the records or check the release that ships this content.",
        })
    );
    let row = v["release"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == f.id("d_late"))
        .unwrap();
    assert_eq!(
        row,
        &json!({
            "id": f.id("d_late"),
            "field": "built",
            "version": "2.0.0",
            "message": format!("Decision {} is built in 2.0.0, after release 1.2.0.", f.id("d_late")),
            "fix": "Check built at this scope. Correct the record or check the release that ships this decision.",
        })
    );

    // 1.9 sorts before 1.10; 1.10 is not after itself; prerelease precedes its release
    let v = check(&f.r, "pub", None, Some("1.9.0"));
    assert!(rows(&v).contains(&(f.id("c_high").into(), "since".into())));
    let v = check(&f.r, "pub", None, Some("1.10.0"));
    assert!(!rows(&v).iter().any(|(i, _)| i == f.id("c_high")));
    let v = check(&f.r, "pub", None, Some("1.2.0-rc.1"));
    assert!(rows(&v).contains(&(f.id("c_equal").into(), "since".into())));
    assert!(!rows(&v).iter().any(|(i, _)| i == f.id("c_pre")));
    let v = check(&f.r, "pub", None, Some("1.2.0"));
    assert!(!rows(&v).iter().any(|(i, _)| i == f.id("c_equal")));

    // a release that ships everything is clear for both audiences and the implicit profile
    for profile in ["pub", "orgm", "implicit:org"] {
        let v = check(&f.r, profile, None, Some("9.0.0"));
        assert_eq!(rows(&v), vec![], "{profile}");
        assert_eq!(v["release"]["status"], "clear");
        assert_eq!(v["status"], "clear");
        assert_eq!(v["release"]["blocks_later"], 0);
        assert_eq!(v["release"]["decisions_later"], 0);
    }

    // the implicit profile (org, planned marked) holds the same rows as the marking profile
    let implicit = check(&f.r, "implicit:org", None, Some("1.2.0"));
    assert_eq!(implicit["status"], "blocked");
    assert!(rows(&implicit).contains(&(f.id("c_high").into(), "since".into())));

    // no release: no new text, an explicit null, and the status of the references alone
    let none = check(&f.r, "pub", None, None);
    assert!(none["release"].is_null(), "{none}");
    assert_eq!(none["status"], none["references"]["status"]);

    // tag, view and proposal scopes read their own content; canon and the tag hold no 5.0.0
    let guide = f.id("guide").to_string();
    write(
        &mut f.r,
        "edit",
        json!([{"op": "create", "type": "claim", "id": "$c_view", "parent": guide,
                "body": "View.", "fields": {"since": "5.0.0"}}]),
    );
    let before = check(&f.r, "pub", None, Some("2.0.0"));
    assert_eq!(rows(&before).len(), 2, "{before}");
    let in_view = check(&f.r, "pub", Some("edit"), Some("2.0.0"));
    assert_eq!(rows(&in_view).len(), 3, "{in_view}");
    let in_tag = check(&f.r, "pub", Some("tag:v1.0.0"), Some("2.0.0"));
    assert_eq!(rows(&in_tag), rows(&before));
    let p = propose(&mut f.r, "edit");
    let in_proposal = check(
        &f.r,
        "pub",
        Some(&format!("proposal:{}", p.r#ref)),
        Some("2.0.0"),
    );
    assert_eq!(rows(&in_proposal), rows(&in_view));
    assert_eq!(check(&f.r, "pub", None, Some("2.0.0")), before);

    // against chooses a disclosure baseline and never supplies a release
    let with =
        publication::check(&f.r.read(), "pub", None, Some("tag:v1.0.0"), Some("1.2.0")).unwrap();
    let alone = check(&f.r, "pub", None, Some("1.2.0"));
    assert_eq!(with["release"], alone["release"]);
    assert_eq!(with["status"], alone["status"]);
    let without = publication::check(&f.r.read(), "pub", None, Some("tag:v1.0.0"), None).unwrap();
    assert!(without["release"].is_null());
    assert_eq!(without["status"], "clear");
}

#[test]
fn ab6_release_report_is_complete_bounded_and_read_only() {
    // n content blocks since 9.0.0, one of them a duplicate claim of one decision
    let build = |n: usize, refs: bool| {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let w = write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "g", "title": "G", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
            ]),
        );
        let (g, out, admin) = (id(&w, "$g"), id(&w, "$out"), id(&w, "$admin"));
        let mut ops = vec![
            json!({"op": "create", "type": "publication_profile", "parent": admin,
                   "fields": {"key": "pub", "title": "Pub", "audience": "public", "select": [g]}}),
        ];
        for i in 0..n {
            ops.push(json!({"op": "create", "type": "claim", "parent": g,
                            "body": format!("Block {i}."), "fields": {"since": "9.0.0"}}));
        }
        if refs {
            ops.push(json!({"op": "create", "type": "prose", "parent": g,
                            "body": format!("See {{{{{out}}}}}.")}));
        }
        write(&mut r, "work", json!(ops));
        land(&mut r, "work");
        (dir, r)
    };
    for (n, shown, omitted) in [(199, 199, 0), (200, 200, 0), (201, 200, 1)] {
        let (_d, r) = build(n, false);
        let v = check(&r, "pub", None, Some("1.0.0"));
        assert_eq!(v["release"]["blocks_later"], n, "{n}");
        assert_eq!(v["release"]["rows"].as_array().unwrap().len(), shown);
        assert_eq!(v["release"]["omitted_rows"], omitted);
        // sorted by full id, each block once
        let got = rows(&v);
        let mut sorted = got.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(got, sorted);
        assert_eq!(v["status"], "blocked");
        assert_eq!(v["references"]["status"], "clear");
    }

    // a version violation and a reference violation stand together, and each keeps its own count
    let (_d, r) = build(3, true);
    let v = check(&r, "pub", None, Some("1.0.0"));
    assert_eq!(v["references"]["status"], "blocked");
    assert_eq!(v["references"]["total"], 1);
    assert_eq!(v["release"]["status"], "blocked");
    assert_eq!(v["release"]["blocks_later"], 3);
    assert_eq!(v["status"], "blocked");
    // a reference violation alone leaves the release clear; no release, no release object
    let v = check(&r, "pub", None, Some("9.0.0"));
    assert_eq!(v["release"]["status"], "clear");
    assert_eq!(v["status"], "blocked");
    let v = check(&r, "pub", None, None);
    assert!(v["release"].is_null());
    assert_eq!(v["status"], "blocked");
    assert_eq!(v["references"]["status"], "blocked");

    // the whole report, with the caller's reserve, stays within 8 MiB
    let max = publication::REPORT_BYTES;
    let e =
        publication::check_reserving(&r.read(), "pub", None, None, Some("1.0.0"), max).unwrap_err();
    assert_err(
        &e,
        E_LIMIT,
        "Publication report exceeds the limit of 8388608 bytes.",
        "Reduce the selection or compare smaller profiles.",
    );
    publication::check_reserving(&r.read(), "pub", None, None, Some("1.0.0"), 1024).unwrap();

    // a bad release is refused before anything resolves, even beside a bad scope or profile
    let head = r.read().canon_head();
    for bad in ["", "x", "1.2", "1.2.0+build", "vv1.0.0", "1.2.0 ", "latest"] {
        for (profile, scope) in [("pub", None), ("nope", Some("nope"))] {
            let e = publication::check(&r.read(), profile, scope, None, Some(bad)).unwrap_err();
            assert_err(
                &e,
                E_BAD_INPUT,
                "Release must be a release version.",
                VERSION_FIX,
            );
        }
    }
    let long = format!("1.0.0-{}", "a".repeat(260));
    let e = publication::check(&r.read(), "pub", None, None, Some(&long)).unwrap_err();
    assert_err(
        &e,
        E_LIMIT,
        "Release exceeds 256 bytes.",
        "Use at most 256 bytes for release.",
    );
    let edge = format!("1.0.0-{}", "a".repeat(256 - 6));
    assert_eq!(edge.len(), 256);
    publication::check(&r.read(), "pub", None, None, Some(&edge)).unwrap();
    assert_eq!(r.read().canon_head(), head, "the check wrote");
}
