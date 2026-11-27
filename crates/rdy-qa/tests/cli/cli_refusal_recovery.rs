//! r5909 (D-8QV9G, D-KQ4CA, D-TXFHR): at the command line, a refusal that the shared engine makes
//! carries the same code, message and recovery text as the MCP tool gives, and the corrected
//! command works. The refusals of the command line's own parser and environment keep their own
//! envelopes. The MCP side is in `mcp_refusal_recovery`.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::{land, seed_doc, write, writer};
use rdy_qa::intake::TEST_SUMMARY;
use serde_json::{Value, json};

/// Runs `f` on the project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

fn refusal(v: &Value) -> (&str, &str, &str) {
    (
        v["code"].as_str().unwrap_or(""),
        v["message"].as_str().unwrap_or(""),
        v["fix"].as_str().unwrap_or(""),
    )
}

/// r5909: the plan, repin, read and approve refusals at the command line, and the corrected
/// commands. The same categories through the MCP tools are pinned in `mcp_refusal_recovery`.
#[test]
fn r5909_mcp_and_cli_follow_refusal_fixes() {
    let s = Served::new();
    let (a, b, sec, ghost) = with_rdy(&s, |r| {
        let spec = spec_doc(r);
        let made = write(
            r,
            "w",
            json!([
                {"op": "update", "id": spec.a.to_string(), "body": "A holds longer."},
                {"op": "create", "type": "claim", "parent": spec.section.to_string(), "body": "Only in w."}
            ]),
        )
        .unwrap();
        let ghost = made.created.values().next().unwrap().clone();
        (
            spec.a.to_string(),
            spec.b.to_string(),
            spec.section.to_string(),
            ghost,
        )
    });
    let _ = sec;

    // a plan item of a kind that does not take the act
    let (ok, plan) = s.json(&["--json", "plan", "--view", "w"]);
    assert!(ok, "{plan}");
    let open = plan["counts"]["open"].as_u64().unwrap();
    let confirm = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "confirm")
        .expect("a confirm item")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, e) = s.json(&["--json", "plan", "done", &confirm, "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(
        refusal(&e),
        (
            "E_BAD_INPUT",
            "done does not answer a confirm item",
            "confirm items take: confirm or defer"
        )
    );
    // a stale item: the open count rides on the fix, as through the tool
    let (ok, e) = s.json(&[
        "--json", "plan", "defer", "pi-zzzz", "--reason", "later", "--view", "w",
    ]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_PLAN_ITEM");
    assert_eq!(
        e["fix"],
        format!(
            "read the plan again after each write. Check counts.open and read every page before answering. An already_written consequence may have no item. One unmatched item or group refuses the whole call. Open items: {open}."
        )
    );
    // an act the command does not have is the parser's refusal, not an rdy envelope
    let o = s.rdy(&["plan", "reaffirm", &confirm, "--view", "w"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    // the corrected command
    let (ok, v) = s.json(&[
        "--json",
        "plan",
        "confirm",
        &confirm,
        "--reason",
        "still true",
        "--view",
        "w",
    ]);
    assert!(ok, "{v}");

    // a repin: the first one moves b's pin, the second finds it current and says what to read
    let (ok, v) = s.json(&["--json", "repin", &b, "--view", "w"]);
    assert!(ok, "{v}");
    let (ok, e) = s.json(&["--json", "repin", &b, "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT");
    assert_eq!(
        e["fix"],
        "read rdy_suspects and the block's own pins before retrying. Drop this repin from the write. A superseded or revoked target needs a changed citation; a reaffirm does not change the target's content version."
    );
    // a repin of the block that does not hold the citation
    let (ok, e) = s.json(&["--json", "repin", &a, "--targets", &b, "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(
        e["fix"],
        "repin the block that holds the citation, with targets from that block's own pins. Read both blocks with rdy_context before retrying."
    );

    // a block that only an open view holds: the read names the view, and the corrected read works
    let (ok, e) = s.json(&["--json", "show", &ghost, "--scope", "canon"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_UNKNOWN_BLOCK");
    assert_eq!(
        e["fix"],
        "this id is in open view w; read with scope view:w"
    );
    let (ok, v) = s.json(&["--json", "show", &ghost, "--scope", "view:w"]);
    assert!(ok, "{v}");

    // a pin hash is no version name
    let hash = "ab".repeat(32);
    let (ok, e) = s.json(&["--json", "tag", &hash]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_TAG_NAME");
    assert_eq!(
        e["fix"],
        "a pin hash names content, not a release or landing. Read the block with rdy_context; choose a version name or a landing hash from rdy_log."
    );
}

fn proposed() -> (Served, String) {
    let s = Served::new();
    let pref = with_rdy(&s, |r| {
        let (_doc, section, _c) = seed_doc(r, "seed");
        land(r, "seed").unwrap();
        write(
            r,
            "work",
            json!([{"op": "create", "type": "claim", "parent": section, "body": "x"}]),
        )
        .unwrap();
        r.propose(&writer(), "work", "t", Some(TEST_SUMMARY))
            .unwrap()
            .r#ref
    });
    (s, pref)
}

/// r5909: a person who passes the relay flags with no `RDY_ACTOR` is refused with the one fix
/// that serves both cases, and a relaying actor that is not the session's keeps naming the
/// actual actor; the answer path says the same through the tool. The corrected call lands.
#[test]
fn r5909_cli_human_relay_names_both_ways_out() {
    let (s, pref) = proposed();
    let fix = "if you are the person, drop the relay flags. If you relay for the person, set RDY_ACTOR=agent:<name> to your own actor and pass that actor in --relayed-by with the person's words in --evidence";
    let relay = |by: &str| {
        vec![
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            by,
            "--evidence",
            "Yes",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>()
    };
    let run = |args: &[String]| {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        s.json(&a)
    };
    let (ok, e) = run(&relay("agent:writer-a"));
    assert!(!ok, "{e}");
    assert_eq!(
        refusal(&e),
        (
            "E_PERMISSION",
            "a person approves as themselves; only the relaying agent records a relay",
            fix
        )
    );
    // the session actor is the agent, but the flag names another actor
    let args = relay("agent:other");
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (ok, e) = s.json_as("agent:writer-a", Some("r1"), &a);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_PERMISSION");
    assert_eq!(
        e["message"],
        "agent:writer-a (run r1) relays only as itself, not as agent:other"
    );
    // the corrected command: the agent relays as itself
    let args = relay("agent:writer-a");
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (ok, v) = s.json_as("agent:writer-a", Some("r1"), &a);
    assert!(ok, "{v}");
    assert_eq!(v["assurance"], "relayed");
}

/// r5909: both output forms of `rdy dump` work, and the refusal of two or none stays the
/// parser's usage error (no new refusal, no parser edit).
#[test]
fn r5909_cli_dump_keeps_both_output_forms() {
    let s = Served::new();
    with_rdy(&s, |r| {
        seed_doc(r, "seed");
        land(r, "seed").unwrap();
    });
    for args in [
        vec!["dump", "p.rdydump"],
        vec!["dump", "--out", "q.rdydump"],
    ] {
        let o = s.rdy(&args);
        assert!(o.status.success(), "{args:?} {o:?}");
    }
    for name in ["p.rdydump", "q.rdydump"] {
        assert!(s.dir.path().join(name).is_file(), "{name}");
    }
    let o = s.rdy(&["dump", "p.rdydump", "--out", "q.rdydump"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
}
