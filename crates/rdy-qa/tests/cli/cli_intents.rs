//! mint1: rdy intent through the daemon, as an agent and as the human.
use rdy_qa::daemon::Served;

#[test]
fn mint1_cli_post_list_withdraw() {
    let s = Served::new();
    let (ok, v) = s.json_as(
        "agent:a",
        Some("ra"),
        &[
            "--json", "intent", "post", "--blocks", "d-XXXXX", "--angle", "x",
        ],
    );
    assert!(!ok && v["code"] == "E_UNKNOWN_BLOCK", "{v}");
    let o = s.rdy(&["intent", "post", "--angle", "x"]);
    assert!(!o.status.success());
    std::fs::write(
        s.dir.path().join(".rdy/packs/rdy-qa.yaml"),
        rdy_qa::packs::QA_PACK_YAML,
    )
    .unwrap();
    std::fs::write(s.dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    s.stop();
    let (ok, v) = s.json_as(
        "agent:a",
        Some("ra"),
        &[
            "--json", "intent", "post", "--area", "payments", "--angle", "fees", "--ttl", "2h",
        ],
    );
    assert!(ok, "{v}");
    let id = v["intent"]["id"].as_str().unwrap().to_string();
    let o = s.rdy(&["intent", "list"]);
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        text.contains(&id) && text.contains("agent:a/ra") && text.contains("fees"),
        "{text}"
    );
    let (ok, v) = s.json(&["--json", "intent", "withdraw", &id]);
    assert!(ok && v["intent"]["status"] == "withdrawn", "{v}");
    let o = s.rdy(&["intent", "list"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "no intents");
    let o = s.rdy(&["intent", "list", "--include-expired"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("withdrawn"));
}
