//! ap5 (D-ZWP7Q "Disputes", n-J783T): `rdy dispute open | resolve | list`, through the daemon, as
//! the person and as an agent that relays the person's words. The CLI writes and reads the same
//! records as `rdy_dispute`.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::{McpState, rdy_dispute, rdy_session};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::sync::Arc;

fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

/// Two landings: the seed, then `fix`.
fn two_landings(s: &Served) -> (String, String) {
    with_rdy(s, |r| {
        seed_doc(r, "seed");
        let l = land(r, "seed").unwrap();
        write(
            r,
            "fix",
            json!([{"op": "create", "type": "doc", "fields": {"name": "fix", "title": "Fix", "doc_type": "page"}}]),
        )
        .unwrap();
        (l, land(r, "fix").unwrap())
    })
}

fn out(o: &std::process::Output) -> (bool, String, String) {
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

fn ok(s: &Served, args: &[&str]) -> Value {
    let (ok, v) = s.json(args);
    assert!(ok, "{args:?}: {v}");
    v
}

#[test]
fn ap5_cli_dispute_matches_mcp() {
    let s = Served::new();
    let (l1, fix) = two_landings(&s);
    // the person, locally: no answer
    let v = ok(
        &s,
        &[
            "--json",
            "dispute",
            "open",
            &l1[..10],
            "--reason",
            "It dropped a rule.",
        ],
    );
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        v["dispute"]["opened_by"],
        json!({"actor": "human:qa", "run": "cli"})
    );
    assert_eq!(v["dispute"]["state"], "open", "{v}");
    assert!(v.get("answer").is_none(), "{v}");
    let v = ok(
        &s,
        &[
            "--json",
            "dispute",
            "resolve",
            &id[..8],
            "--outcome",
            "upheld",
            "--correction",
            &fix,
        ],
    );
    assert_eq!(
        v["dispute"]["resolution"]["correction"],
        fix.as_str(),
        "{v}"
    );
    // an agent relays the person's words
    let (ok_, v) = s.json_as(
        "agent:relay",
        Some("r1"),
        &[
            "--json",
            "dispute",
            "open",
            &l1,
            "--reason",
            "Another reason.",
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "dispute it again",
            "--question",
            "Dispute the landing?",
            "--session",
            "chat-9",
        ],
    );
    assert!(ok_, "{v}");
    assert_eq!(
        v["dispute"]["opened_by"],
        json!({"actor": "human:qa", "run": "cli"})
    );
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay", "run": "r1"})
    );
    assert_eq!(v["answer"].as_str().map(str::len), Some(64), "{v}");
    let cli = ok(&s, &["--json", "dispute", "list"]);
    let open = ok(
        &s,
        &["--json", "dispute", "list", "--landing", &l1, "--open"],
    );
    assert_eq!(open["disputes"].as_array().map(Vec::len), Some(1), "{open}");
    // the same records through rdy_dispute
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    let mcp = rdy_dispute(
        &mut st,
        serde_json::from_value(json!({"op": "list"})).unwrap(),
    )
    .unwrap();
    assert_eq!(cli, mcp);
    let mcp_open = rdy_dispute(
        &mut st,
        serde_json::from_value(json!({"op": "list", "landing": l1, "open": true})).unwrap(),
    )
    .unwrap();
    assert_eq!(open, mcp_open);
    assert_eq!(cli["disputes"][0]["id"], id.as_str());
    assert_eq!(cli["disputes"][0]["state"], "upheld");
}

#[test]
fn ap5_cli_list_prints_the_closed_lines() {
    let s = Served::new();
    let (l1, fix) = two_landings(&s);
    let (okay, text, err) = out(&s.rdy(&["dispute", "list"]));
    assert!(
        okay && text.trim().is_empty(),
        "no dispute, no line: {text}{err}"
    );
    let a = ok(
        &s,
        &[
            "--json",
            "dispute",
            "open",
            &l1,
            "--reason",
            "It dropped a rule.",
        ],
    )["dispute"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = ok(
        &s,
        &[
            "--json",
            "dispute",
            "open",
            &fix,
            "--reason",
            "Wrong title.",
        ],
    )["dispute"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(
        &s,
        &[
            "--json",
            "dispute",
            "resolve",
            &a,
            "--outcome",
            "upheld",
            "--correction",
            &fix,
        ],
    );
    let (okay, text, err) = out(&s.rdy(&["dispute", "list"]));
    assert!(okay, "{err}");
    assert_eq!(
        text,
        format!(
            "{} upheld landing {}: It dropped a rule.\n  resolved upheld by human:qa, correction {}\n{} open landing {}: Wrong title.\n",
            &a[..12],
            &l1[..12],
            &fix[..12],
            &b[..12],
            &fix[..12],
        )
    );
    // resolve prints the row it resolved; a resolution with no correction names none
    let (okay, text, err) = out(&s.rdy(&["dispute", "resolve", &b, "--outcome", "dismissed"]));
    assert!(okay, "{err}");
    assert_eq!(
        text,
        format!(
            "{} dismissed landing {}: Wrong title.\n  resolved dismissed by human:qa\n",
            &b[..12],
            &fix[..12]
        )
    );
    let (okay, text, _) = out(&s.rdy(&["dispute", "list", "--open"]));
    assert!(okay && text.trim().is_empty(), "{text}");
}

#[test]
fn ap5_cli_an_agent_without_relay_flags_exits_with_e_permission() {
    let s = Served::new();
    let (l1, _) = two_landings(&s);
    let o = s.rdy_as(
        "agent:relay",
        Some("r1"),
        &["dispute", "open", &l1, "--reason", "It dropped a rule."],
    );
    let (okay, _, err) = out(&o);
    assert!(!okay, "{err}");
    assert!(
        err.contains("error E_PERMISSION: only a person opens a dispute\nfix: relay the person's words with evidence"),
        "{err}"
    );
    let (okay, v) = s.json_as(
        "agent:relay",
        Some("r1"),
        &[
            "--json",
            "dispute",
            "resolve",
            "abcdef",
            "--outcome",
            "dismissed",
        ],
    );
    assert!(!okay);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    assert_eq!(v["message"], "only a person resolves a dispute", "{v}");
    let v = ok(&s, &["--json", "dispute", "list"]);
    assert_eq!(v["disputes"], json!([]), "nothing is written: {v}");
}
