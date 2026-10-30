//! ck3 (D-JQD5X, D-4WC6R, D-Z8P09; O3, O16): the read-time fact `found`, `missing` or `unchecked`
//! for each source, shown in briefings and `rdy_context` and in the displayed coverage, and left
//! out of the gate, the assessment and every token. The checkouts are real git repositories; the
//! facts depend on the machine, so no write ever carries one.
use rdy_engine::brief::Coverage;
use rdy_engine::coverage::{IssueCode, gate_rollup};
use rdy_engine::source_exists::{
    MAX_LOCATOR_BYTES, MAX_SOURCE_PROBES, PROBE_OUTPUT_LIMIT, SourceExistence, run_probe_for_tests,
};
use rdy_engine::*;
use rdy_model::{BlockId, Role};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

mod common;

const VIEW: &str = "work";
const FIX: &str = "check the source's locator, or point the source at a tag or commit that exists";

fn writer() -> Session {
    Session {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn work() -> Scope {
    Scope::View(VIEW.into())
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: VIEW.into(),
            ops: parse_ops(serde_json::from_value(ops).unwrap()).unwrap(),
            justification: None,
            message: "ck3".into(),
        },
    )
    .unwrap_or_else(|e| panic!("write: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A checkout with one commit, tagged `v1`; returns it and its commit.
fn checkout() -> (tempfile::TempDir, String) {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    std::fs::write(d.path().join("a.txt"), "a\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "--no-verify", "-m", "c"]);
    git(d.path(), &["tag", "v1"]);
    let head = git(d.path(), &["rev-parse", "HEAD"]);
    (d, head)
}

/// Links the checkouts `repos` (name, path) to the project at `dir`.
fn link(dir: &Path, repos: &[(&str, &Path)]) {
    let yaml = dir.join(".rdy.yaml");
    let text: String = std::fs::read_to_string(&yaml)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("code_repos:"))
        .map(|l| format!("{l}\n"))
        .collect();
    let names: Vec<&str> = repos.iter().map(|(n, _)| *n).collect();
    std::fs::write(
        &yaml,
        format!("{text}\ncode_repos: [{}]\n", names.join(", ")),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join(".rdy")).unwrap();
    let mut local = String::from("code_paths:\n");
    for (n, p) in repos {
        local.push_str(&format!("  {n}: {}\n", p.display()));
    }
    std::fs::write(dir.join(".rdy/local.yaml"), local).unwrap();
}

/// A project with doc `notes`, section `s` and claim `c`; returns the ids.
fn project() -> (tempfile::TempDir, Rdy, BTreeMap<String, String>) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$notes", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$notes", "fields": {"title": "Retention"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Retention is thirty days."}
        ]),
    );
    (dir, r, w.created.into_iter().collect())
}

/// Creates sources `(kind, locator)` under `parent`; returns their ids in the given order.
fn sources(r: &mut Rdy, parent: &str, rows: &[(&str, String)]) -> Vec<String> {
    let ops: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(i, (kind, locator))| {
            json!({"op": "create", "type": "source", "id": format!("$x{i}"), "parent": parent,
                "fields": {"title": format!("Source {i}"), "kind": kind, "locator": locator}})
        })
        .collect();
    let w = write(r, Value::Array(ops));
    (0..rows.len())
        .map(|i| w.created[&format!("$x{i}")].clone())
        .collect()
}

fn facts(r: &Rdy) -> BTreeMap<String, SourceExistence> {
    r.read()
        .source_facts(&work())
        .unwrap()
        .by_source
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

use SourceExistence::{Found, Missing, Unchecked};

#[test]
fn ck3_source_states_follow_the_closed_table() {
    let (dir, mut r, id) = project();
    let (app, head) = checkout();
    // a second checkout that is no repository: unreadable
    let plain = tempfile::tempdir().unwrap();
    link(dir.path(), &[("app", app.path()), ("plain", plain.path())]);
    let zero = "0".repeat(40);
    let ids = sources(
        &mut r,
        &id["$s"],
        &[
            ("spec", "spec/v1.md".into()),
            ("release", "v1".into()),
            ("release", "v9".into()),
            ("commit", format!("app@{head}")),
            ("commit", format!("app@{zero}")),
            ("commit", "no at sign".into()),
            ("commit", format!("nope@{head}")),
            ("commit", format!("plain@{head}")),
            ("commit", "app@abc".into()),
            ("release", "bad..name".into()),
            ("release", "ends.lock".into()),
        ],
    );
    let got = facts(&r);
    let want = [
        Unchecked, // spec: always, no network
        Found,     // tag v1 in app
        Unchecked, // v9 is missing in app, but plain is unreadable
        Found,     // commit in app
        Missing,   // commit absent from a readable app
        Unchecked, // malformed locator
        Unchecked, // unknown repo
        Unchecked, // plain is no repository
        Unchecked, // short hash
        Unchecked, // not a valid tag name
        Unchecked, // not a valid tag name
    ];
    for (i, w) in want.iter().enumerate() {
        assert_eq!(got[&ids[i]], *w, "source {i}");
    }
    // the same project with only the readable checkout linked: a tag no checkout holds is missing
    link(dir.path(), &[("app", app.path())]);
    let got = facts(&r);
    assert_eq!(got[&ids[1]], Found);
    assert_eq!(got[&ids[2]], Missing);
    assert_eq!(got[&ids[7]], Unchecked, "plain is not linked any more");
    // an absent checkout: nothing is read
    let gone = dir.path().join("gone");
    link(dir.path(), &[("app", &gone)]);
    let got = facts(&r);
    for i in [1, 2, 3, 4] {
        assert_eq!(got[&ids[i]], Unchecked, "absent checkout, source {i}");
    }
    // no checkout linked at all
    let (dir2, mut r2, id2) = project();
    let ids2 = sources(
        &mut r2,
        &id2["$s"],
        &[("release", "v1".into()), ("spec", "x".into())],
    );
    let got = facts(&r2);
    assert_eq!(got[&ids2[0]], Unchecked);
    assert_eq!(got[&ids2[1]], Unchecked);
    drop(dir2);
}

#[test]
fn ck3_source_reads_are_offline_and_refresh_without_a_write() {
    let (dir, mut r, id) = project();
    let (app, head) = checkout();
    link(dir.path(), &[("app", app.path())]);
    let ids = sources(
        &mut r,
        &id["$s"],
        &[("release", "v5".into()), ("release", "v1".into())],
    );
    let mark = |r: &Rdy| {
        let st = r.read();
        (
            format!("{:?}", st.seq()),
            st.view_head(VIEW).map(|h| h.head),
            std::fs::read(dir.path().join(".rdy/local.yaml")).unwrap(),
        )
    };
    let before = mark(&r);
    assert_eq!(facts(&r)[&ids[0]], Missing);
    git(app.path(), &["tag", "v5"]);
    assert_eq!(facts(&r)[&ids[0]], Found, "a new local tag shows at once");
    git(app.path(), &["tag", "-d", "v5"]);
    assert_eq!(facts(&r)[&ids[0]], Missing, "a removed tag shows at once");
    assert_eq!(facts(&r)[&ids[1]], Found);
    // no write: the log, the view head and the local config are as they were
    assert_eq!(mark(&r), before);
    // no fetch: git leaves no FETCH_HEAD, and the checkout has no remote to reach
    assert!(!app.path().join(".git/FETCH_HEAD").exists());
    let _ = head;
}

#[test]
fn ck3_source_probe_bounds_are_fixed() {
    assert_eq!(MAX_SOURCE_PROBES, 256);
    assert_eq!(MAX_LOCATOR_BYTES, 4096);
    assert_eq!(PROBE_OUTPUT_LIMIT, 65_536);
    let (dir, mut r, id) = project();
    let (app, _) = checkout();
    link(dir.path(), &[("app", app.path())]);

    // 257 distinct release locators: the first 256 in id order are checked, the 257th is not
    let rows: Vec<(&str, String)> = (0..257).map(|i| ("release", format!("t{i:03}"))).collect();
    let ids = sources(&mut r, &id["$s"], &rows);
    let got = facts(&r);
    let mut sorted = ids.clone();
    sorted.sort();
    for (i, s) in sorted.iter().enumerate() {
        let want = if i < 256 { Missing } else { Unchecked };
        assert_eq!(got[s], want, "source {i} in id order");
    }
    // equal locators count once: three hundred sources of one tag are all checked
    let same: Vec<(&str, String)> = (0..300).map(|_| ("release", "v1".to_string())).collect();
    let more = sources(&mut r, &id["$s"], &same);
    let got = facts(&r);
    // the 257th distinct value is still past the cap, and every v1 source repeats one value
    let admitted = got
        .iter()
        .filter(|(k, _)| more.contains(k))
        .map(|(_, v)| *v)
        .collect::<Vec<_>>();
    assert_eq!(admitted.len(), 300);
    assert!(admitted.iter().all(|v| *v == admitted[0]), "{admitted:?}");

    // a locator of 4096 bytes passes the length check, and 4097 does not
    let (dir2, mut r2, id2) = project();
    let (app2, _) = checkout();
    link(dir2.path(), &[("app", app2.path())]);
    let long = |n: usize| -> String {
        (0..n)
            .map(|i| if (i + 1) % 100 == 0 { '/' } else { 'a' })
            .collect()
    };
    let ids2 = sources(
        &mut r2,
        &id2["$s"],
        &[("release", long(4096)), ("release", long(4097))],
    );
    let got = facts(&r2);
    assert_eq!(got[&ids2[0]], Missing, "4096 bytes are read");
    assert_eq!(got[&ids2[1]], Unchecked, "4097 bytes are not");

    // git output is cut at the cap, on either pipe, and the child is reaped
    let mut c = Command::new("yes");
    c.arg("x");
    let e = run_probe_for_tests(c).unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e}");
    let mut c = Command::new("sh");
    c.args(["-c", "yes x >&2"]);
    let e = run_probe_for_tests(c).unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e}");
    let mut c = Command::new("sh");
    c.args(["-c", "printf ok"]);
    assert_eq!(run_probe_for_tests(c).unwrap().stdout, b"ok");
}

#[test]
fn ck3_context_and_brief_add_existence_only() {
    let (dir, mut r, id) = project();
    let (app, _) = checkout();
    link(dir.path(), &[("app", app.path())]);
    let ids = sources(
        &mut r,
        &id["$s"],
        &[("release", "v1".into()), ("release", "v9".into())],
    );
    write(
        &mut r,
        json!([{"op": "update", "id": id["$c"], "links": {"reflects": [ids[0], ids[1]]}}]),
    );
    let st = r.read();
    // a source context shows the value, always
    let ctx = serde_json::to_value(st.context(&work(), &bid(&ids[0])).unwrap()).unwrap();
    assert_eq!(ctx["existence"], "found");
    let ctx = serde_json::to_value(st.context(&work(), &bid(&ids[1])).unwrap()).unwrap();
    assert_eq!(ctx["existence"], "missing");
    // another block type has no such key, and every old key stays
    let other = serde_json::to_value(st.context(&work(), &bid(&id["$c"])).unwrap()).unwrap();
    assert!(other.get("existence").is_none(), "{other}");
    for key in [
        "id",
        "block_type",
        "version",
        "fields",
        "body",
        "doc",
        "parent",
        "children",
        "outgoing",
        "incoming",
        "status",
        "governing",
        "suspects",
    ] {
        assert!(other.get(key).is_some(), "context lost {key}");
        assert!(ctx.get(key).is_some(), "source context lost {key}");
    }
    // the briefing's source rows keep source, kind, title and blocks, and add existence
    let b = st
        .brief(
            &work(),
            &Coverage {
                ids: vec![id["$c"].clone()],
                path: None,
            },
            Some(100_000),
        )
        .unwrap();
    let v = serde_json::to_value(&b).unwrap();
    let rows = v["sources"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{v:#}");
    for row in rows {
        let keys: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["blocks", "existence", "kind", "source", "title"],
            "{row}"
        );
        assert_eq!(row["kind"], "release");
        assert_eq!(row["blocks"], json!([id["$c"]]));
    }
    let by: BTreeMap<&str, &str> = rows
        .iter()
        .map(|x| {
            (
                x["source"].as_str().unwrap(),
                x["existence"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(by[ids[0].as_str()], "found");
    assert_eq!(by[ids[1].as_str()], "missing");
    // no other part of the briefing names the fact
    let text = v.to_string();
    assert_eq!(text.matches("\"existence\"").count(), 2, "{text}");
}

#[test]
fn ck3_missing_sources_lower_displayed_connected_only() {
    let (dir, mut r, id) = project();
    let (app, _) = checkout();
    link(dir.path(), &[("app", app.path())]);
    let ids = sources(
        &mut r,
        &id["$s"],
        &[
            ("release", "v1".into()),
            ("release", "v9".into()),
            ("spec", "spec/a.md".into()),
        ],
    );
    let (found, missing, spec) = (&ids[0], &ids[1], &ids[2]);
    let c = bid(&id["$c"]);
    let connected = |e: &rdy_engine::coverage::BlockEntry| {
        let i = rdy_engine::coverage::Dimension::ALL
            .iter()
            .position(|d| *d == rdy_engine::coverage::Dimension::Connected)
            .unwrap();
        e.checks[i]
    };
    let base = r.read().coverage_issues(&work()).unwrap();
    let base_entry = base.blocks.get(&c).unwrap().clone();

    // a found source passes; an unchecked one adds no denominator
    write(
        &mut r,
        json!([{"op": "update", "id": id["$c"], "links": {"reflects": [found, spec]}}]),
    );
    let st = r.read();
    let intrinsic = st.coverage_issues(&work()).unwrap();
    let shown = st.display_coverage_issues(&work()).unwrap();
    let (i_e, s_e) = (
        intrinsic.blocks.get(&c).unwrap(),
        shown.blocks.get(&c).unwrap(),
    );
    assert_eq!(
        connected(s_e),
        connected(i_e) + 1,
        "found adds one passing check"
    );
    assert_eq!(s_e.issues, i_e.issues);
    drop(st);

    // a missing direct source: the check and one issue, with the exact fix
    write(
        &mut r,
        json!([{"op": "update", "id": id["$c"], "links": {"reflects": [found, missing, spec]}}]),
    );
    let st = r.read();
    let intrinsic = st.coverage_issues(&work()).unwrap();
    let shown = st.display_coverage_issues(&work()).unwrap();
    let (i_e, s_e) = (
        intrinsic.blocks.get(&c).unwrap(),
        shown.blocks.get(&c).unwrap(),
    );
    assert_eq!(
        connected(s_e),
        connected(i_e) + 2,
        "found and missing each add a check"
    );
    let added: Vec<_> = s_e
        .issues
        .iter()
        .filter(|i| i.code == IssueCode::SourceNotFound)
        .collect();
    assert_eq!(added.len(), 1, "{:#?}", s_e.issues);
    assert_eq!(added[0].subject.as_deref(), Some(missing.as_str()));
    assert_eq!(added[0].fix, FIX);
    assert_eq!(added[0].block, c.to_string());
    assert_eq!(
        serde_json::to_value(added[0]).unwrap()["code"],
        "source_not_found"
    );
    assert!(
        i_e.issues
            .iter()
            .all(|i| i.code != IssueCode::SourceNotFound)
    );
    // the displayed rollups are lower; the intrinsic ones are not
    let lower = st.coverage_report(&work(), None).unwrap();
    let rolled = st.scope_rollup(&work()).unwrap();
    assert!(
        lower.score < rolled.score,
        "{} {}",
        lower.score,
        rolled.score
    );
    assert!(
        lower.dimensions[&rdy_engine::coverage::Dimension::Connected]
            < rolled.dimensions[&rdy_engine::coverage::Dimension::Connected]
    );
    assert!(
        lower
            .issues
            .iter()
            .any(|i| i.code == IssueCode::SourceNotFound)
    );
    let summary = st.coverage_summary(&work()).unwrap();
    assert_eq!(summary.score, lower.score);
    assert_eq!(summary.issues_total, lower.issues_total);
    assert_eq!(base_entry.issues, i_e.issues, "no intrinsic issue appears");
    // the issue sits on the consumer, never on the source
    assert!(
        shown
            .blocks
            .get(&bid(missing))
            .is_none_or(|e| e.issues.iter().all(|i| i.code != IssueCode::SourceNotFound))
    );
    drop(st);

    // two links to one source count once: the reflects link and a reference in the text
    write(
        &mut r,
        json!([{"op": "update", "id": id["$c"], "body": format!("Retention is thirty days {{{{{missing}}}}}."),
            "links": {"reflects": [missing]}}]),
    );
    let st = r.read();
    let intrinsic = st.coverage_issues(&work()).unwrap();
    let shown = st.display_coverage_issues(&work()).unwrap();
    let (i_e, s_e) = (
        intrinsic.blocks.get(&c).unwrap(),
        shown.blocks.get(&c).unwrap(),
    );
    assert_eq!(connected(s_e), connected(i_e) + 1);
    assert_eq!(
        s_e.issues
            .iter()
            .filter(|i| i.code == IssueCode::SourceNotFound)
            .count(),
        1
    );
    // with the tag created the issue goes and the check stays
    drop(st);
    git(app.path(), &["tag", "v9"]);
    let st = r.read();
    let shown = st.display_coverage_issues(&work()).unwrap();
    assert!(
        shown
            .blocks
            .get(&c)
            .unwrap()
            .issues
            .iter()
            .all(|i| i.code != IssueCode::SourceNotFound)
    );
}

#[test]
fn ck3_checkout_changes_leave_gate_and_assessment_identical() {
    let (dir, mut r, id) = project();
    let (app, _) = checkout();
    link(dir.path(), &[("app", app.path())]);
    let ids = sources(&mut r, &id["$s"], &[("release", "v7".into())]);
    write(
        &mut r,
        json!([{"op": "update", "id": id["$c"], "links": {"reflects": [ids[0]]}}]),
    );
    let p = r
        .propose(&writer(), VIEW, VIEW, Some(common::TEST_SUMMARY))
        .unwrap();
    let pref = r.resolve_proposal(&p.r#ref).unwrap();

    let snapshot = |r: &Rdy| {
        let st = r.read();
        let issues = st.coverage_issues(&work()).unwrap();
        let m = st.materialized(&work()).unwrap();
        let gate = gate_rollup(st.pack(), &m, issues.blocks.iter()).score;
        let a = st
            .reader()
            .assessment_of_proposal(&pref.version, st.seq())
            .unwrap()
            .expect("the propose stores the assessment");
        let stored = serde_json::to_vec(&st.reader().assessment(&a).unwrap()).unwrap();
        let brief = st
            .brief(
                &work(),
                &Coverage {
                    ids: vec![id["$c"].clone()],
                    path: None,
                },
                Some(100_000),
            )
            .unwrap();
        (
            serde_json::to_value(&*issues).unwrap(),
            st.scope_rollup(&work()).unwrap().score,
            gate,
            a,
            stored,
            brief.record.token_hex(),
        )
    };
    let missing_state = snapshot(&r);
    let displayed_missing = r.read().coverage_summary(&work()).unwrap().score;
    git(app.path(), &["tag", "v7"]);
    let found_state = snapshot(&r);
    let displayed_found = r.read().coverage_summary(&work()).unwrap().score;
    assert_eq!(missing_state, found_state);
    assert!(
        displayed_missing < displayed_found,
        "the displayed score does move: {displayed_missing} {displayed_found}"
    );
    // the stored record never names the display issue
    let text = String::from_utf8(found_state.4).unwrap();
    assert!(!text.contains("source_not_found"), "{text}");
    // the version is still ready to land under either state
    common::confirm_exceptions(&mut r, &approver(), &p);
    git(app.path(), &["tag", "-d", "v7"]);
    r.land(&approver(), &p.id).unwrap();
}

#[test]
fn ck3_source_facts_change_no_confidence() {
    let (dir, mut r, id) = project();
    let (app, _) = checkout();
    link(dir.path(), &[("app", app.path())]);
    let ids = sources(&mut r, &id["$s"], &[("release", "v7".into())]);
    let e = write(
        &mut r,
        json!([{"op": "create", "type": "entity", "id": "$e", "parent": id["$s"],
            "fields": {"name": "Retention"}}]),
    );
    write(
        &mut r,
        json!([{"op": "update", "id": id["$c"],
            "links": {"describes": [e.created["$e"]], "reflects": [ids[0]]}}]),
    );
    let entity = bid(&e.created["$e"]);
    let claim = bid(&id["$c"]);
    // what every read surface says about confidence, with the reasons
    let surfaces = |r: &Rdy| {
        let st = r.read();
        let brief = st
            .brief(
                &work(),
                &Coverage {
                    ids: vec![id["$c"].clone()],
                    path: None,
                },
                Some(100_000),
            )
            .unwrap();
        let brief_conf = serde_json::to_value(brief.coverage.unwrap().confidence).unwrap();
        let sheet = st
            .fact_sheet(&work(), &entity, rdy_engine::lexicon::NameKind::Name)
            .unwrap();
        let sheet_conf = serde_json::to_value(sheet.confidence).unwrap();
        let found = st
            .search(
                &work(),
                &SearchQuery {
                    text: "thirty".into(),
                    ..Default::default()
                },
                None,
                Some(100_000),
            )
            .unwrap();
        let hit = found.matches.iter().find(|m| m.id == claim.to_string());
        let hit_conf = hit.map(|m| m.confidence);
        assert!(hit_conf.is_some(), "the search hit carries a level");
        (brief_conf, sheet_conf, hit_conf)
    };
    let missing = surfaces(&r);
    let shown_missing = r.read().display_coverage_issues(&work()).unwrap();
    assert!(
        shown_missing
            .blocks
            .get(&claim)
            .unwrap()
            .issues
            .iter()
            .any(|i| i.code == IssueCode::SourceNotFound),
        "the display does carry the missing source"
    );
    git(app.path(), &["tag", "v7"]);
    let found = surfaces(&r);
    assert_eq!(
        missing, found,
        "confidence and its reasons do not read the checkout"
    );
    for v in [&missing.0, &missing.1] {
        assert!(
            !v.to_string().contains("source_not_found"),
            "no source issue is a reason: {v}"
        );
    }
}
