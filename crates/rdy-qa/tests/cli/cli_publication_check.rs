//! pub9 (D-0VCQ5, D-KDFZ3, D-KQ4CA): `rdy publication check` and the MCP tool `rdy_publication_check`
//! return one report; the check reads source, writes nothing, and works with no daemon.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

/// A landed and tagged `v1.0.0`, then a second landing that adds a doc and a reference that
/// leaves the selection. The org profile `site` selects the folder `pubs`; the public profile
/// `open` selects the doc `a`, which refers to the doc `out` outside it. Returns nothing: the
/// project is closed for the daemon or the in-process commands.
fn seed(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$pubs", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (pubs, a, out, admin) = (
        w.created["$pubs"].clone(),
        w.created["$a"].clone(),
        w.created["$out"].clone(),
        w.created["$admin"].clone(),
    );
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{out}}}}}.")},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [pubs]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [a]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$b", "parent": pubs, "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$b", "body": format!("Also {{{{{out}}}}}.")}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
}

/// A fingerprint of the log and of every file outside `.rdy`: unchanged bytes, unchanged print.
/// The index and the run directory are not the record; the segments hold the log and the refs.
fn fingerprint(dir: &Path) -> BTreeMap<String, u64> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, u64>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if rel == ".rdy/run" || rel.starts_with(".rdy/run/") || rel.contains("index.redb") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let mut h = DefaultHasher::new();
                bytes.hash(&mut h);
                out.insert(rel, h.finish());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    assert!(
        out.keys().any(|k| k.starts_with(".rdy/store/segments/")),
        "the log is in the print: {out:?}"
    );
    out
}

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn pub9_cli_and_mcp_return_the_same_check_and_diff() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();
    for (profile, against) in [
        ("site", Some("tag:v1.0.0")),
        ("site", None),
        ("open", Some("canon")),
    ] {
        let mut args = vec!["--json", "publication", "check", "--profile", profile];
        let mut chat = json!({"profile": profile});
        if let Some(a) = against {
            args.extend(["--against", a]);
            chat["against"] = json!(a);
        }
        let (ok, cli) = s.json(&args);
        assert!(ok, "{cli}");
        let mcp = c.call("rdy_publication_check", chat).ok();
        // no transport envelope is left to strip: both are the engine's value, key for key
        assert_eq!(cli, mcp, "{profile} {against:?}");
        assert_eq!(cli.to_string(), mcp.to_string(), "{profile} {against:?}");
    }

    // the text: four lines, then the comparison
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "site",
        "--against",
        "tag:v1.0.0",
    ]));
    assert_eq!(
        t,
        "Publication profile site: warning.\n5 blocks in 2 docs and 1 folders.\nReferences outside selection: 2.\nLinks outside selection: 0.\nProfile revision changed: no.\nDisclosure changes: 1 added docs, 0 removed docs, 0 changed docs, 0 changed folders, 0 changed blocks outside docs, 0 attachment changes, 1 new references.\n"
    );
    let t = stdout(&s.rdy(&["publication", "check", "--profile", "site"]));
    assert_eq!(t.lines().count(), 4, "{t}");
    // a public profile that refers outside its selection is blocked, and the command succeeds
    let t = stdout(&s.rdy(&["publication", "check", "--profile", "open"]));
    assert!(t.starts_with("Publication profile open: blocked.\n"), "{t}");
    for word in ["approve", "publish", "authoriz", "answer"] {
        assert!(!t.to_lowercase().contains(word), "{t}");
    }
    // the profile is required
    let out = s.rdy(&["publication", "check"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--profile"));
}

/// The closed keys of the report (RULING F4, D-KQ4CA): nothing rides beside them.
const REPORT_KEYS: [&str; 10] = [
    "against",
    "disclosure",
    "hidden_planned",
    "profile",
    "references",
    "release",
    "scope",
    "selection",
    "status",
    "warnings",
];

fn keys_of(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort();
    k
}

#[test]
fn pub9_a_pending_pack_notice_stays_outside_the_report() {
    const PACK_A: &str = "name: proj\nversion: '1'\nextends: rdy-standard\n";
    const PACK_BAD: &str = "name: proj\nversion: '1'\nextends: no-such-pack\n";
    let s = Served::new();
    seed(s.dir.path());
    let packs = s.dir.path().join(".rdy/packs");
    std::fs::create_dir_all(&packs).unwrap();
    let config = std::fs::read_to_string(s.dir.path().join(".rdy.yaml")).unwrap();
    let config: String = config
        .lines()
        .filter(|l| !l.starts_with("pack:"))
        .map(|l| format!("{l}\n"))
        .collect();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();
    let args = ["--json", "publication", "check", "--profile", "site"];
    let chat = json!({"profile": "site"});
    let quiet = c.call("rdy_publication_check", chat.clone()).ok();
    assert_eq!(keys_of(&quiet), REPORT_KEYS, "{quiet}");

    // a pack edit followed by a write: the pack reloads and this session has not heard of it
    std::fs::write(packs.join("proj.yaml"), PACK_A).unwrap();
    std::fs::write(
        s.dir.path().join(".rdy.yaml"),
        format!("pack: proj\n{config}"),
    )
    .unwrap();
    let out = s.rdy(&["new", "doc", "fresh", "--title", "Fresh"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, cli) = s.json(&args);
    assert!(ok, "{cli}");
    let mcp = c.call("rdy_publication_check", chat.clone()).ok();
    assert_eq!(
        keys_of(&mcp),
        REPORT_KEYS,
        "the notice is not a report key: {mcp}"
    );
    assert_eq!(cli, mcp);
    assert_eq!(cli.to_string(), mcp.to_string());
    // the notice is deferred, not lost: the next tool reply carries it
    let next = c.call("rdy_overview", json!({})).ok();
    assert!(
        next["notice"]
            .as_str()
            .is_some_and(|n| n.starts_with("pack_changed")),
        "{next}"
    );

    // a standing pack error is repeated on every reply, and still stays out of the report
    std::fs::write(packs.join("proj.yaml"), PACK_BAD).unwrap();
    let out = s.rdy(&["new", "doc", "again", "--title", "Again"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = s.rdy(&args);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("notice: pack_error: "),
        "the CLI prints the notice on stderr"
    );
    let cli: Value = serde_json::from_slice(&out.stdout).unwrap();
    for _ in 0..2 {
        let mcp = c.call("rdy_publication_check", chat.clone()).ok();
        assert_eq!(keys_of(&mcp), REPORT_KEYS, "{mcp}");
        assert_eq!(cli, mcp);
    }
}

#[test]
fn pub9_check_is_read_only_and_works_without_a_daemon() {
    let s = Served::new();
    seed(s.dir.path());
    let before = fingerprint(s.dir.path());
    // the person is the approver: every role may inspect, the approver included
    let first = stdout(&s.rdy(&[
        "--no-daemon",
        "publication",
        "check",
        "--profile",
        "site",
        "--against",
        "tag:v1.0.0",
    ]));
    assert!(
        first.starts_with("Publication profile site: warning.\n"),
        "{first}"
    );
    assert!(
        s.daemon_pid().is_none(),
        "the check started no daemon: it ran in the command"
    );
    for args in [
        vec![
            "--no-daemon",
            "--json",
            "publication",
            "check",
            "--profile",
            "open",
        ],
        vec![
            "--no-daemon",
            "--json",
            "publication",
            "check",
            "--profile",
            "site",
            "--scope",
            "tag:v1.0.0",
        ],
        // the seed's view `work` exists: a view is a candidate (D-CWC91)
        vec![
            "--no-daemon",
            "--json",
            "publication",
            "check",
            "--profile",
            "site",
            "--scope",
            "work",
        ],
    ] {
        let (ok, v) = s.json(&args);
        assert!(ok, "{v}");
    }
    // a refusal changes nothing either
    for args in [
        vec![
            "--no-daemon",
            "publication",
            "check",
            "--profile",
            "missing",
        ],
        vec![
            "--no-daemon",
            "publication",
            "check",
            "--profile",
            "site",
            "--against",
            "tag:9.9.9",
        ],
    ] {
        assert!(!s.rdy(&args).status.success(), "{args:?}");
    }
    assert_eq!(
        fingerprint(s.dir.path()),
        before,
        "log, refs and files are unchanged"
    );
    // the same through the daemon, as an agent too
    let served = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "site",
        "--against",
        "tag:v1.0.0",
    ]));
    assert_eq!(served, first, "a daemon returns the same text");
    let (ok, v) = s.json_as(
        "agent:a",
        Some("r1"),
        &["--json", "publication", "check", "--profile", "site"],
    );
    assert!(ok, "{v}");
    assert!(matches!(&v["references"]["status"], Value::String(_)));
    s.stop();
    assert_eq!(
        fingerprint(s.dir.path()),
        before,
        "the daemon wrote nothing"
    );
}

/// A public profile `plan` over four docs. At `v1.0.0` it hides one planned note in `guide`, the
/// planned doc `ghost` with its prose, and the empty planned doc `nothing`; `late` is shown. A
/// second landing hides one more note in `guide` and marks `late` planned.
fn seed_planned(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$ghost", "fields": {"name": "ghost", "title": "Ghost", "doc_type": "page", "planned": true}},
            {"op": "create", "type": "doc", "id": "$nothing", "fields": {"name": "nothing", "title": "Nothing", "doc_type": "page", "planned": true}},
            {"op": "create", "type": "doc", "id": "$late", "fields": {"name": "late", "title": "Late", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    let (admin, guide, ghost, nothing, late) = (
        id("$admin"),
        id("$guide"),
        id("$ghost"),
        id("$nothing"),
        id("$late"),
    );
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": guide, "body": "Shown words."},
            {"op": "create", "type": "note", "parent": guide, "body": "Planned note.", "fields": {"planned": true}},
            {"op": "create", "type": "prose", "parent": ghost, "body": "Ghost words."},
            {"op": "create", "type": "prose", "parent": late, "body": "Late words."},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "plan", "title": "Plan", "audience": "public", "select": [guide, ghost, nothing, late]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "note", "parent": guide, "body": "Second planned note.", "fields": {"planned": true}},
            {"op": "update", "id": late, "fields": {"planned": true}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
}

#[test]
fn relc4_mcp_cli_and_no_daemon_reports_agree() {
    let s = Served::new();
    seed_planned(s.dir.path());
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();
    let mut seen = vec![];
    let mut runs = vec![];
    for against in [None, Some("tag:v1.0.0")] {
        let mut args = vec!["--json", "publication", "check", "--profile", "plan"];
        let mut chat = json!({"profile": "plan"});
        if let Some(a) = against {
            args.extend(["--against", a]);
            chat["against"] = json!(a);
        }
        let (ok, cli) = s.json(&args);
        assert!(ok, "{cli}");
        let mcp = c.call("rdy_publication_check", chat).ok();
        assert_eq!(cli, mcp, "{against:?}");
        assert_eq!(cli.to_string(), mcp.to_string(), "{against:?}");
        seen.push(cli);
        runs.push((against, args));
    }
    // the same report with no daemon: one process holds the store at a time
    drop(c);
    s.stop();
    for ((against, args), cli) in runs.iter().zip(&seen) {
        let mut bare = vec!["--no-daemon"];
        bare.extend(args);
        let (ok, direct) = s.json(&bare);
        assert!(ok, "{direct}");
        assert_eq!(cli, &direct, "{against:?}");
        assert_eq!(cli.to_string(), direct.to_string(), "{against:?}");
    }
    // canon holds: guide (two notes), ghost (hidden, one prose), nothing (hidden, empty), late
    // (hidden, one prose)
    let rows = |v: &Value| -> Vec<(String, u64, bool)> {
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["title"].as_str().unwrap().to_string(),
                    r["hidden_blocks"].as_u64().unwrap(),
                    r["doc_hidden"].as_bool().unwrap(),
                )
            })
            .collect()
    };
    let full = &seen[0]["hidden_planned"];
    assert_eq!(
        (full["docs"].as_u64(), full["blocks"].as_u64()),
        (Some(4), Some(4)),
        "{full}"
    );
    assert_eq!(full["omitted_rows"], 0);
    assert!(seen[0]["disclosure"].is_null());
    let mut got = rows(full);
    got.sort();
    assert_eq!(
        got,
        [
            ("Ghost".to_string(), 1, true),
            ("Guide".to_string(), 2, false),
            ("Late".to_string(), 1, true),
            ("Nothing".to_string(), 0, true),
        ]
    );
    for row in full["rows"].as_array().unwrap() {
        assert_eq!(row["redacted"], false);
        let mut keys: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            ["doc", "doc_hidden", "hidden_blocks", "redacted", "title"]
        );
    }
    // against v1.0.0: one more note in guide, and late is newly hidden with its prose
    let new = &seen[1]["disclosure"]["newly_hidden_planned"];
    assert_eq!(
        (new["docs"].as_u64(), new["blocks"].as_u64()),
        (Some(2), Some(2)),
        "{new}"
    );
    let mut got = rows(new);
    got.sort();
    assert_eq!(
        got,
        [
            ("Guide".to_string(), 1, false),
            ("Late".to_string(), 1, true)
        ]
    );
    // a comparison carries no first-publication list beside it
    assert_eq!(seen[1]["hidden_planned"]["docs"], 4);
}

#[test]
fn relc4_cli_hides_empty_lines_and_names_new_counts() {
    let s = Served::new();
    seed_planned(s.dir.path());
    let first = stdout(&s.rdy(&["publication", "check", "--profile", "plan"]));
    assert!(
        first.contains("\nHidden as planned: 4 blocks in 4 docs."),
        "{first}"
    );
    assert!(!first.contains("Newly hidden"), "{first}");
    let later = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "plan",
        "--against",
        "tag:v1.0.0",
    ]));
    let lines: Vec<&str> = later.lines().collect();
    assert_eq!(
        lines[4], "Hidden as planned: 4 blocks in 4 docs.",
        "{later}"
    );
    assert_eq!(
        lines.last().copied(),
        Some("Newly hidden as planned: 2 blocks in 2 docs."),
        "{later}"
    );
    // a baseline that already hides everything: the new list is empty, so its line is absent
    let same = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "plan",
        "--against",
        "canon",
    ]));
    assert!(
        same.contains("Hidden as planned: 4 blocks in 4 docs."),
        "{same}"
    );
    assert!(!same.contains("Newly hidden"), "{same}");
}
