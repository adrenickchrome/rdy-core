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
                .to_string();
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

    // the text: three lines, then the comparison
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
        "Publication profile site: warning.\n5 blocks in 2 docs and 1 folders.\nReferences outside selection: 2.\nProfile revision changed: no.\nDisclosure changes: 1 added docs, 0 removed docs, 0 changed docs, 0 changed folders, 0 attachment changes, 1 new references.\n"
    );
    let t = stdout(&s.rdy(&["publication", "check", "--profile", "site"]));
    assert_eq!(t.lines().count(), 3, "{t}");
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
            "--scope",
            "work",
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
