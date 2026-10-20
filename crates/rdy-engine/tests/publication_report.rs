//! pub9 (D-0VCQ5, D-KDFZ3, D-KQ4CA): `publication::check` is the one report behind the MCP tool,
//! the ctl op and the command. It reads one snapshot, takes only canon, tags and landings, caps
//! every listed array at 200 rows while its counts cover every row, and returns success for a
//! blocked policy result.
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

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(
            &session(Role::Writer),
            view,
            "publication",
            Some(common::TEST_SUMMARY),
        )
        .unwrap();
    common::confirm_exceptions(r, &session(Role::Approver), &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
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

fn is_full_hash(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Every key of `v`, one level down, sorted.
fn keys(v: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    keys
}

const ARRAYS: [&str; 8] = [
    "added_docs",
    "removed_docs",
    "changed_docs",
    "changed_folders",
    "added_attachments",
    "removed_attachments",
    "changed_attachments",
    "new_references",
];

/// A baseline tag `v1.0.0` and a candidate canon that differ in `n` rows of every disclosure
/// array: `n` docs added, `n` removed (moved out of the selected folder), `n` changed, `n`
/// folders renamed, `n` figures added, removed and changed, and `n` references that leave the
/// selection. The profile `pubsite` is an org profile selecting folder `pubs`.
fn capped_project(n: usize) -> (tempfile::TempDir, Rdy) {
    let (dir, mut r) = project();
    let blob_a = r.store_mut().put_blob(b"blob a").unwrap();
    let blob_b = r.store_mut().put_blob(b"blob b").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$outside", "fields": {"name": "outside", "title": "Outside", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (pubs, outside, admin) = (id(&w, "$pubs"), id(&w, "$outside"), id(&w, "$admin"));
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
            "fields": {"key": "pubsite", "title": "Site", "audience": "org", "select": [pubs]}}]),
    );
    let fig = |blob: Hash, parent: &str, name: &str| {
        json!({"op": "create", "type": "figure", "id": name, "parent": parent,
               "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "figure"}})
    };
    let mut ops = vec![];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "folder", "id": format!("$sf{i}"), "parent": pubs, "fields": {"name": format!("sf{i}")}}));
        for kind in ["rm", "ch"] {
            ops.push(
                json!({"op": "create", "type": "doc", "id": format!("${kind}{i}"), "parent": pubs,
                "fields": {"name": format!("{kind}{i}"), "title": "Doc", "doc_type": "page"}}),
            );
            ops.push(fig(blob_a, &format!("${kind}{i}"), &format!("${kind}f{i}")));
        }
    }
    let w = write(&mut r, "work", Value::Array(ops));
    land(&mut r, "work");
    tag(&mut r, "v1.0.0");

    let mut ops = vec![];
    for i in 0..n {
        ops.push(json!({"op": "update", "id": id(&w, &format!("$sf{i}")), "fields": {"name": format!("sf-renamed{i}")}}));
        ops.push(json!({"op": "move", "id": id(&w, &format!("$rm{i}"))}));
        ops.push(json!({"op": "update", "id": id(&w, &format!("$chf{i}")), "fields": {"blob": blob_b.to_string()}}));
        ops.push(
            json!({"op": "create", "type": "doc", "id": format!("$add{i}"), "parent": pubs,
            "fields": {"name": format!("add{i}"), "title": "Doc", "doc_type": "page"}}),
        );
        ops.push(fig(blob_a, &format!("$add{i}"), &format!("$addf{i}")));
        ops.push(
            json!({"op": "create", "type": "prose", "parent": format!("$add{i}"),
            "body": format!("See {{{{{outside}}}}}.")}),
        );
    }
    write(&mut r, "work", Value::Array(ops));
    land(&mut r, "work");
    (dir, r)
}

#[test]
fn pub9_reports_cap_rows_but_keep_full_counts() {
    for n in [199usize, 200, 201] {
        let (_dir, r) = capped_project(n);
        let v = publication::check(&r.read(), "pubsite", None, Some("tag:v1.0.0")).unwrap();
        let left = n.saturating_sub(200);
        let shown = n.min(200);

        // selection: 1 folder + n folders; per added/changed doc: a doc, a figure (added: a prose)
        let blocks = 1 + n + 3 * n + 2 * n;
        let s = &v["selection"];
        assert_eq!(s["blocks"], blocks, "{n}");
        assert_eq!(s["docs"], 2 * n, "{n}");
        assert_eq!(s["folders"], 1 + n, "{n}");
        assert_eq!(s["attachments"], 2 * n, "{n}");
        assert_eq!(
            s["entries"].as_array().unwrap().len(),
            blocks.min(200),
            "{n}"
        );
        assert_eq!(s["omitted_entries"], blocks - blocks.min(200), "{n}");
        assert!(is_full_hash(&s["hash"]), "{n}");
        for e in s["entries"].as_array().unwrap() {
            assert!(is_full_hash(&e["version"]), "{e}");
            assert!(e["redaction"].is_null(), "{e}");
        }
        let ids: Vec<&str> = s["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "entries are the first sorted rows");

        // references: n occurrences outside, one row per source
        let refs = &v["references"];
        assert_eq!(refs["status"], "warning", "{n}");
        assert_eq!(
            (&refs["excluded"], &refs["missing"], &refs["total"]),
            (&json!(n), &json!(0), &json!(n)),
            "{n}"
        );
        assert_eq!(refs["rows"].as_array().unwrap().len(), shown, "{n}");
        assert_eq!(refs["omitted_rows"], left, "{n}");

        // disclosure: every array lists its first 200 rows and counts the rest
        let d = &v["disclosure"];
        for name in ARRAYS {
            assert_eq!(d[name].as_array().unwrap().len(), shown, "{n} {name}");
            assert_eq!(d["omitted"][name], left, "{n} {name}");
        }
        assert_eq!(keys(&d["omitted"]).len(), 9, "{n}");
        // no removal applies: an empty request list, none left out
        assert_eq!(d["redactions"], json!([]), "{n}");
        assert_eq!(d["omitted"]["redactions"], 0, "{n}");
        assert_eq!(
            (publication::REPORT_ROWS, publication::REPORT_BYTES),
            (200, 8_388_608)
        );
        assert_eq!(d["new_reference_count"], n, "{n}");
        assert_eq!(d["profile_changed"], false, "{n}");
        for h in ["before_selection_hash", "after_selection_hash"] {
            assert!(is_full_hash(&d[h]), "{n} {h}");
        }
        for row in d["added_docs"].as_array().unwrap() {
            assert!(is_full_hash(&row["digest"]), "{row}");
        }
        for row in d["changed_attachments"].as_array().unwrap() {
            assert!(is_full_hash(&row["before_blob"]) && is_full_hash(&row["after_blob"]));
        }
        // the warning is the one Task 4 text
        assert_eq!(
            v["warnings"],
            json!([{
                "code": "W_PUBLICATION_REFERENCE",
                "message": format!("Org profile has {n} references outside its selection."),
                "fix": "Review these references before you share the output.",
            }]),
            "{n}"
        );
        // without a baseline there is no disclosure
        let alone = publication::check(&r.read(), "pubsite", None, None).unwrap();
        assert!(alone["disclosure"].is_null() && alone["against"].is_null());
        assert_eq!(alone["selection"], v["selection"]);
    }
}

#[test]
fn pub9_default_scope_is_canon_and_baseline_uses_the_same_profile_id() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$c", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, b, c, admin) = (id(&w, "$a"), id(&w, "$b"), id(&w, "$c"), id(&w, "$admin"));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}]),
    );
    let p1 = id(&w, "$pf");
    land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    let v1 = publication::check(&r.read(), "site", None, None).unwrap();
    assert_eq!(v1["scope"]["kind"], "canon");
    assert_eq!(v1["profile"]["id"], p1.as_str());
    assert_eq!(v1["profile"]["key"], "site");
    assert_eq!(
        v1["scope"]["landing"],
        r.read().canon_head().unwrap().to_string()
    );

    // a session's view holds unlanded changes: the omitted scope is canon, not that view
    write(
        &mut r,
        "agent-view",
        json!([{"op": "update", "id": p1, "fields": {"select": [a, b]}}]),
    );
    let still = publication::check(&r.read(), "site", None, None).unwrap();
    assert_eq!(still, v1);
    assert_eq!(
        publication::check(&r.read(), "site", Some("canon"), None).unwrap(),
        v1
    );

    // the key moves to a new profile; the old profile keeps its id under another key
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": p1, "fields": {"key": "retired", "select": [a, b]}},
            {"op": "create", "type": "publication_profile", "id": "$p2", "parent": admin,
             "fields": {"key": "site", "title": "Site two", "audience": "org", "select": [c]}}
        ]),
    );
    let p2 = id(&w, "$p2");
    land(&mut r, "work");

    // the baseline resolves the candidate's stable id: the retired key finds the old profile
    let v = publication::check(&r.read(), "retired", None, Some("tag:v1.0.0")).unwrap();
    assert_eq!(v["profile"]["id"], p1.as_str());
    assert_eq!(v["against"]["kind"], "landing");
    assert_eq!(v["disclosure"]["profile_id"], p1.as_str());
    assert_eq!(v["disclosure"]["profile_changed"], true);
    assert_eq!(v["disclosure"]["added_docs"].as_array().unwrap().len(), 1);
    assert_eq!(v["disclosure"]["added_docs"][0]["id"], b.as_str());
    // the same profile by id gives the same report
    assert_eq!(
        publication::check(&r.read(), &p1, None, Some("tag:v1.0.0")).unwrap(),
        v
    );

    // the key `site` now names a profile the tag never held: no fallback by key or to canon
    let e = publication::check(&r.read(), "site", None, Some("tag:v1.0.0")).unwrap_err();
    assert_err(
        &e,
        E_NOT_FOUND,
        &format!("Publication profile \"{p2}\" was not found."),
        "Use a publication profile id or key from this scope.",
    );

    // a tag and its landing are one scope kind with the same selection
    let tagged = publication::check(&r.read(), &p1, Some("tag:v1.0.0"), None).unwrap();
    let landing = tagged["scope"]["landing"].as_str().unwrap().to_string();
    assert_eq!(tagged["scope"]["kind"], "landing");
    assert!(is_full_hash(&tagged["scope"]["landing"]));
    let by_landing =
        publication::check(&r.read(), &p1, Some(&format!("landing:{landing}")), None).unwrap();
    assert_eq!(by_landing, tagged);
    assert_eq!(tagged["selection"], v1["selection"]);
}

#[test]
fn pub9_check_returns_a_closed_report_and_blocked_is_a_result() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$in", "fields": {"name": "in", "title": "In", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (inside, out, admin) = (id(&w, "$in"), id(&w, "$out"), id(&w, "$admin"));
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": inside, "body": format!("Refers to {{{{{out}}}}}.")},
            {"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [inside]}}
        ]),
    );
    land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    let v = publication::check(&r.read(), "open", None, Some("canon")).unwrap();
    assert_eq!(v["references"]["status"], "blocked");
    assert_eq!(v["warnings"], json!([]));
    assert_eq!(
        keys(&v),
        [
            "against",
            "disclosure",
            "profile",
            "references",
            "scope",
            "selection",
            "warnings"
        ]
    );
    assert_eq!(
        keys(&v["profile"]),
        ["audience", "id", "key", "title", "trail", "version"]
    );
    assert_eq!(keys(&v["scope"]), ["kind", "landing"]);
    assert_eq!(
        keys(&v["selection"]),
        [
            "attachments",
            "blocks",
            "docs",
            "entries",
            "folders",
            "hash",
            "identity",
            "omitted_entries"
        ]
    );
    assert_eq!(
        keys(&v["references"]),
        [
            "excluded",
            "missing",
            "omitted_rows",
            "rows",
            "status",
            "total"
        ]
    );
    assert_eq!(
        keys(&v["references"]["rows"][0]),
        [
            "classification",
            "location",
            "occurrences",
            "source",
            "target"
        ]
    );
    assert_eq!(v["profile"]["audience"], "public");
    assert_eq!(v["profile"]["trail"], false);
    // comparing a profile with itself in one scope changes nothing
    assert_eq!(v["disclosure"]["new_reference_count"], 0);
    assert_eq!(v["disclosure"]["profile_changed"], false);

    // only canon, tag:<name> and landing:<hash> are scopes
    let bad = |s: &str| publication::check(&r.read(), "open", Some(s), None).unwrap_err();
    for s in [
        "work",
        "view:work",
        "inflight",
        "proposal:P-ABCDE@1",
        "",
        "Canon",
    ] {
        assert_err(
            &bad(s),
            E_BAD_INPUT,
            "Publication check scope must be canon, tag:<name>, or landing:<hash>.",
            "Use canon, tag:<name>, or landing:<hash>.",
        );
    }
    assert_eq!(bad("tag:9.9.9").code, E_NOT_FOUND);
    // the candidate resolves first: a missing profile wins over a bad baseline
    let e = publication::check(&r.read(), "nope", None, Some("work")).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND);
    let e = publication::check(&r.read(), "open", None, Some("work")).unwrap_err();
    assert_err(
        &e,
        E_BAD_INPUT,
        "Publication check against must be canon, tag:<name>, or landing:<hash>.",
        "Use canon, tag:<name>, or landing:<hash>.",
    );
    // a baseline of another audience is the disclosure comparison's own refusal
    let p = r.read().resolve_id(&Scope::Canon, "open").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": p.to_string(), "fields": {"audience": "org"}}]),
    );
    land(&mut r, "work");
    let e = publication::check(&r.read(), "open", None, Some("tag:v1.0.0")).unwrap_err();
    assert_err(
        &e,
        E_BAD_INPUT,
        "Disclosure comparison requires the same audience.",
        "Choose a baseline with the same audience as the candidate profile.",
    );
}
