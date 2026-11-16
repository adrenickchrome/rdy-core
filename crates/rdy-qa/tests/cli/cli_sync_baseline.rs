//! scc7 (D-CDQF7, D-6WD17, D-2VM48, D-KQ4CA): `rdy publication baseline` makes the portable
//! baseline of a profile at a tag, and `rdy publication check --against-baseline` compares a
//! candidate with it. The chat tools `rdy_publication_baseline` and `rdy_publication_check`
//! return the same values. Both are reads: they add nothing to the log.
use rdy_engine::Rdy;
use rdy_engine::publication::Baseline;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;

/// A landed and tagged `v1.0.0`, then a second landing that adds a doc. The org profile `site`
/// selects the folder `pubs`. The project is closed for the daemon.
fn seed(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$pubs", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (pubs, a, admin) = (
        w.created["$pubs"].clone(),
        w.created["$a"].clone(),
        w.created["$admin"].clone(),
    );
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": "Alpha holds."},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [pubs]}}
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
            {"op": "create", "type": "prose", "parent": "$b", "body": "Beta holds."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
}

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The log's bytes, by segment name: the record, which a read leaves alone.
fn log_of(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir.join(".rdy/store/segments"))
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// The baseline file of `site` at `v1.0.0`, written to `name` in a directory beside the project.
fn make(s: &Served, out: &Path) -> Value {
    let o = out.to_string_lossy().into_owned();
    let (ok, v) = s.json(&[
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
        "--out",
        &o,
    ]);
    assert!(ok, "{v}");
    v
}

#[test]
fn scc7_baseline_cli_paths() {
    let s = Served::new();
    seed(s.dir.path());
    let side = tempfile::tempdir().unwrap();
    let file = side.path().join("site.baseline.json");

    // a baseline written to a new file: one line names the tag, the hash and the path
    let v = make(&s, &file);
    let bytes = std::fs::read(&file).unwrap();
    let baseline = Baseline::read(&bytes).expect("the file verifies");
    assert_eq!(
        v["baseline"]["baseline_hash"],
        baseline.baseline_hash().to_string()
    );
    assert_eq!(v["baseline"]["tag"], "v1.0.0");
    assert_eq!(v["out"], file.to_string_lossy().as_ref());
    assert_eq!(v["bytes"], bytes.len());
    assert!(v.get("content").is_none(), "no body beside a file: {v}");
    let other = side.path().join("again.json");
    let text = stdout(&s.rdy(&[
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
        "--out",
        &other.to_string_lossy(),
    ]));
    assert_eq!(
        text,
        format!(
            "Wrote the baseline of tag v1.0.0 ({}) to {}.\n",
            baseline.baseline_hash(),
            other.display()
        )
    );
    assert_eq!(
        std::fs::read(&other).unwrap(),
        bytes,
        "equal inputs, equal bytes"
    );

    // no --out: the baseline prints, and it verifies as the file does
    let printed = stdout(&s.rdy(&[
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
    ]));
    let again = Baseline::read(printed.trim().as_bytes()).expect("the printed baseline verifies");
    assert_eq!(again.baseline_hash(), baseline.baseline_hash());

    // the output guards of a dump: an existing file, a missing directory and .rdy
    let (ok, e) = s.json(&[
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
        "--out",
        &file.to_string_lossy(),
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_ALREADY_EXISTS", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("rdy publication baseline never overwrites a file"),
        "{e}"
    );
    let missing = side.path().join("nope/b.json");
    let (ok, e) = s.json(&[
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
        "--out",
        &missing.to_string_lossy(),
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    let inside = s.dir.path().join(".rdy/b.json");
    let (ok, e) = s.json(&[
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
        "--out",
        &inside.to_string_lossy(),
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(e["message"].as_str().unwrap().contains(".rdy"), "{e}");
    assert!(!inside.exists());

    // a baseline needs a tag
    let (ok, e) = s.json(&[
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "canon",
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(e["message"], "A publication baseline requires a tag.");
    assert_eq!(e["fix"], "Use scope tag:<name>.");
    // the scope is required
    let o = s.rdy(&["publication", "baseline", "--profile", "site"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--scope"));

    // the comparison: the baseline's identity and both redaction sets sit under `disclosure`
    let f = file.to_string_lossy().into_owned();
    let (ok, report) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "site",
        "--against-baseline",
        &f,
    ]);
    assert!(ok, "{report}");
    let d = &report["disclosure"];
    assert_eq!(d["baseline"]["tag"], "v1.0.0");
    assert_eq!(
        d["baseline"]["baseline_hash"],
        baseline.baseline_hash().to_string()
    );
    assert_eq!(d["redactions"], json!({"candidate": [], "baseline": []}));
    assert_eq!(report["against"]["kind"], "baseline");
    assert_eq!(d["added_docs"].as_array().unwrap().len(), 1, "{d}");
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "site",
        "--against-baseline",
        &f,
    ]));
    assert!(
        t.contains(&format!(
            "Compared with baseline of tag v1.0.0 ({}).\n",
            baseline.baseline_hash()
        )),
        "{t}"
    );
    assert!(t.contains("New B (1 block)"), "{t}");
    // the same comparison against the tag of this store is the same changes
    let (ok, same) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "site",
        "--against",
        "tag:v1.0.0",
    ]);
    assert!(ok, "{same}");
    assert_eq!(same["disclosure"]["added_docs"], d["added_docs"]);
    assert!(same["disclosure"].get("baseline").is_none());

    // against and a baseline file exclude each other
    let o = s.rdy(&[
        "publication",
        "check",
        "--profile",
        "site",
        "--against",
        "tag:v1.0.0",
        "--against-baseline",
        &f,
    ]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot be used with"));

    // a file that does not verify, and a file that is not there
    let junk = side.path().join("junk.json");
    std::fs::write(&junk, b"{\"format\":\"rdy-publication-baseline-1\"}").unwrap();
    let (ok, e) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "site",
        "--against-baseline",
        &junk.to_string_lossy(),
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_PUBLICATION_BASELINE", "{e}");
    assert_eq!(e["message"], "The publication baseline does not verify.");
    let (ok, e) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "site",
        "--against-baseline",
        &side.path().join("gone.json").to_string_lossy(),
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_IO", "{e}");
}

#[test]
fn scc7_baseline_chat_has_cli_parity() {
    let s = Served::new();
    seed(s.dir.path());
    let side = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "role": "reader", "view": "canon"}),
    )
    .ok();

    // the make: the chat reply and the CLI reply are the same value, key for key
    let (ok, cli) = s.json(&[
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
    ]);
    assert!(ok, "{cli}");
    let chat = c
        .call(
            "rdy_publication_baseline",
            json!({"profile": "site", "scope": "tag:v1.0.0"}),
        )
        .ok();
    assert_eq!(cli, chat);
    assert_eq!(cli.to_string(), chat.to_string());
    let baseline = Baseline::read(cli["content"].to_string().as_bytes()).unwrap();
    assert_eq!(baseline.to_json(), cli["content"]);

    // with a file, every role writes the new file under the same guards
    let by_chat = side.path().join("chat.json");
    let by_cli = side.path().join("cli.json");
    let chat = c
        .call(
            "rdy_publication_baseline",
            json!({"profile": "site", "scope": "tag:v1.0.0", "out": by_chat.to_string_lossy()}),
        )
        .ok();
    let cli = make(&s, &by_cli);
    assert_eq!(chat["baseline"], cli["baseline"]);
    assert_eq!(chat["bytes"], cli["bytes"]);
    assert_eq!(
        std::fs::read(&by_chat).unwrap(),
        std::fs::read(&by_cli).unwrap()
    );
    let again = c.call(
        "rdy_publication_baseline",
        json!({"profile": "site", "scope": "tag:v1.0.0", "out": by_chat.to_string_lossy()}),
    );
    assert_eq!(again.code(), Some("E_ALREADY_EXISTS"));
    let canon = c.call(
        "rdy_publication_baseline",
        json!({"profile": "site", "scope": "canon"}),
    );
    assert_eq!(canon.code(), Some("E_BAD_INPUT"));

    // the compare: both disclosure objects, and the whole report, are the same value
    let f = by_cli.to_string_lossy().into_owned();
    let (ok, cli) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "site",
        "--against-baseline",
        &f,
    ]);
    assert!(ok, "{cli}");
    let chat = c
        .call(
            "rdy_publication_check",
            json!({"profile": "site", "against_baseline": f}),
        )
        .ok();
    assert_eq!(cli, chat);
    assert_eq!(cli.to_string(), chat.to_string());
    assert_eq!(
        cli["disclosure"]["baseline"],
        json!({
            "repo_id": cli["disclosure"]["baseline"]["repo_id"],
            "tag": "v1.0.0",
            "landing": cli["disclosure"]["baseline"]["landing"],
            "baseline_hash": baseline.baseline_hash().to_string(),
        })
    );
    assert_eq!(
        cli["disclosure"]["redactions"],
        json!({"candidate": [], "baseline": []})
    );
    // the refusals are the same: both inputs, a baseline that does not verify
    let both = c.call(
        "rdy_publication_check",
        json!({"profile": "site", "against": "tag:v1.0.0", "against_baseline": f}),
    );
    assert_eq!(both.code(), Some("E_BAD_INPUT"));
    let junk = side.path().join("junk.json");
    std::fs::write(&junk, b"[]").unwrap();
    let bad = c.call(
        "rdy_publication_check",
        json!({"profile": "site", "against_baseline": junk.to_string_lossy()}),
    );
    assert_eq!(bad.code(), Some("E_PUBLICATION_BASELINE"));
}

#[test]
fn scc7_baseline_make_and_compare_are_read_only() {
    let s = Served::new();
    seed(s.dir.path());
    let side = tempfile::tempdir().unwrap();
    let file = side.path().join("b.json");
    let f = file.to_string_lossy().into_owned();
    let before = log_of(s.dir.path());
    // both commands run in the command itself: no daemon starts, and the log stays as it was
    let (ok, v) = s.json(&[
        "--no-daemon",
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "tag:v1.0.0",
        "--out",
        &f,
    ]);
    assert!(ok, "{v}");
    for _ in 0..2 {
        let (ok, v) = s.json(&[
            "--no-daemon",
            "--json",
            "publication",
            "check",
            "--profile",
            "site",
            "--against-baseline",
            &f,
        ]);
        assert!(ok, "{v}");
    }
    assert!(s.daemon_pid().is_none(), "no daemon was started");
    assert_eq!(log_of(s.dir.path()), before, "the log is unchanged");
    // a refusal changes nothing either
    let (ok, _) = s.json(&[
        "--no-daemon",
        "--json",
        "publication",
        "baseline",
        "--profile",
        "site",
        "--scope",
        "canon",
    ]);
    assert!(!ok);
    assert_eq!(log_of(s.dir.path()), before);
    // a reader session may make and compare
    let mut c = McpClient::start(s.dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:r", "role": "reader", "view": "canon"}),
    )
    .ok();
    // opening the server is its own business; the log is read once it is open
    let before = log_of(s.dir.path());
    let b = c
        .call(
            "rdy_publication_baseline",
            json!({"profile": "site", "scope": "tag:v1.0.0"}),
        )
        .ok();
    assert!(b["content"].is_object());
    c.call(
        "rdy_publication_check",
        json!({"profile": "site", "against_baseline": f}),
    )
    .ok();
    drop(c);
    assert_eq!(log_of(s.dir.path()), before);
}
