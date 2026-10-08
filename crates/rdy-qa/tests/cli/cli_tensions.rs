//! cten1: rdy tensions, rdy resolve and the propose line, through the daemon.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::tensions::*;

#[test]
fn cten1_rdy_tensions_and_rdy_resolve_from_the_command_line() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let sp = spec_doc(&mut r);
    put(&mut r, "b", cite(&sp.section, &sp.a));
    put(&mut r, "a", upd(&sp.a, "A changed."));
    r.close().unwrap();
    let (ok, v) = s.json(&["tensions", "--json"]);
    assert!(ok, "{v}");
    let tn = v["tensions"][0]["id"].as_str().unwrap().to_string();
    let out = s.rdy(&["tensions"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains(&tn) && text.contains("blocking") && text.contains("premise_shift"),
        "{text}"
    );
    assert!(text.contains("sides:"), "{text}");
    // A cut listing names the real flag, and a larger --max-tokens lists the omitted tension.
    let out = s.rdy(&["tensions", "--max-tokens", "1"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cut = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!cut.contains(&tn), "{cut}");
    assert!(
        cut.contains("1 more not listed (pass --max-tokens for more)"),
        "{cut}"
    );
    let (ok, v) = s.json(&["tensions", "--max-tokens", "1", "--json"]);
    assert!(ok, "{v}");
    assert_eq!(v["tensions"], serde_json::json!([]));
    assert_eq!(v["omitted"], 1);
    let out = s.rdy(&["tensions", "--max-tokens", "2000"]);
    let more = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        more.contains(&tn) && !more.contains("more not listed"),
        "{more}"
    );
    let out = s.rdy(&["propose", "--title", "b", "--view", "b"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("tensions: 1 (0 would refuse the landing)"),
        "{text}"
    );
    let out = s.rdy(&["resolve", &tn, "--dismiss", "B still holds"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("dismissed {tn}")));
    let (_, v) = s.json(&["tensions", "--status", "dismissed", "--json"]);
    assert_eq!(v["tensions"][0]["dismissal"]["actor"], "human:qa");
    let (ok, v) = s.json(&["resolve", "tn-000000000000", "--dismiss", "x", "--json"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_NOT_FOUND");
    let (_, v) = s.json(&["tensions", "--status", "dismissed", "--json"]);
    assert_eq!(v["counts"]["dismissed"], 1);
    s.stop();
}
