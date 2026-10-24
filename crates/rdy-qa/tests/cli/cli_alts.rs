//! malt1: rdy alt and rdy propose --alt through the daemon; approve reports the choice.
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn malt1_cli_create_propose_approve_closes_the_other_member() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let (_doc, _s, claim) = seed_doc(&mut r, "seed");
        land(&mut r, "seed").unwrap();
        let v = version_of(&r, &Scope::Canon, &claim);
        write(
            &mut r,
            "a",
            json!([{"op": "update", "id": claim, "base": v, "body": "A"}]),
        )
        .unwrap();
        write(
            &mut r,
            "b",
            json!([{"op": "update", "id": claim, "base": v, "body": "B"}]),
        )
        .unwrap();
        r.close().unwrap();
    }
    let (ok, v) = s.json(&[
        "--json", "alt", "create", "Wording", "--member", "x=a", "--member", "y=b",
    ]);
    assert!(ok, "{v}");
    let set = v["set"]["id"].as_str().unwrap().to_string();
    let o = s.rdy(&["alt", "ls"]);
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        t.contains(&set) && t.contains("x=a") && t.contains("(open)"),
        "{t}"
    );
    let (ok, v) = s.json(&["--json", "propose", "--title", "pick a", "--view", "a"]);
    assert!(!ok && v["code"] == "E_ALT_UNCHOSEN", "{v}");
    let (ok, v) = s.json(&[
        "--json", "propose", "--title", "pick a", "--view", "a", "--chosen", "x",
    ]);
    assert!(!ok && v["code"] == "E_BAD_INPUT", "{v}");
    let (ok, v) = s.json(&[
        "--json",
        "propose",
        "--title",
        "pick a",
        "--view",
        "a",
        "--alt",
        &set,
        "--chosen",
        "x",
        "--summary",
        "test summary",
    ]);
    assert!(ok, "{v}");
    let pref = v["ref"].as_str().unwrap().to_string();
    let o = s.rdy(&["approve", &pref]);
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains(&format!("chose a of {set}")) && t.contains("closed b"),
        "{t}"
    );
    let (ok, v) = s.json(&["--json", "alt", "ls", "--all"]);
    assert!(ok && v["sets"][0]["status"] == "chosen", "{v}");
}

#[test]
fn malt1_cli_compare_and_abandon_text() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let (_doc, _s, claim) = seed_doc(&mut r, "seed");
        land(&mut r, "seed").unwrap();
        let v = version_of(&r, &Scope::Canon, &claim);
        write(
            &mut r,
            "a",
            json!([{"op": "update", "id": claim, "base": v, "body": "A"}]),
        )
        .unwrap();
        write(
            &mut r,
            "b",
            json!([{"op": "update", "id": claim, "base": v, "body": "B"}]),
        )
        .unwrap();
        r.close().unwrap();
    }
    let o = s.rdy(&[
        "alt", "create", "Wording", "--member", "x=a", "--member", "y",
    ]);
    assert!(!o.status.success());
    let o = s.rdy(&[
        "alt", "create", "Wording", "--member", "x=a", "--member", "y=b",
    ]);
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        o.status.success() && t.contains("Wording (open): x=a, y=b"),
        "{t}"
    );
    let (_, v) = s.json(&["--json", "alt", "ls"]);
    let set = v["sets"][0]["id"].as_str().unwrap().to_string();
    let o = s.rdy(&["alt", "compare", &set]);
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        t.contains("x (a):") && t.contains("y (b):") && t.contains("a vs canon"),
        "{t}"
    );
    let o = s.rdy(&["alt", "abandon", &set, "--reason", "neither"]);
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        o.status.success() && t.contains("(abandoned)") && t.contains("closed a, b"),
        "{t}"
    );
    let o = s.rdy(&["alt", "ls"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "no alternative sets"
    );
}
