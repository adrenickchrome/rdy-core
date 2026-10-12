//! ib9: `rdy inbox [--ack]`, the CLI twin of `rdy_inbox` (D-TPZ0W, FW-UX1), and the `rdy check`
//! help that names the export manifest's format.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::tensions::*;

fn stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn ib9_the_first_rdy_inbox_starts_the_human_inbox() {
    let s = Served::new();
    let first = stdout(&s.rdy(&["inbox"]));
    assert_eq!(
        first.trim_end(),
        "inbox started: events from now on show here"
    );
    let (ok, second) = s.json(&["--json", "inbox"]);
    assert!(ok, "{second}");
    assert_eq!(second["entries"], serde_json::json!([]), "{second}");
    assert!(second.get("started").is_none(), "{second}");
    // the human's run is `cli` (D-16YS5): the subscription is the ref `inbox/human:qa/cli`
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    assert!(
        r.read()
            .reader()
            .get_ref("inbox/human:qa/cli")
            .unwrap()
            .is_some()
    );
    r.close().unwrap();
}

#[test]
fn ib9_an_agent_reads_its_own_inbox_from_rdy_actor() {
    let s = Served::new();
    let (ok, v) = s.json_as("agent:a", Some("a"), &["--json", "inbox"]);
    assert!(ok, "{v}");
    assert_eq!(v["started"], true, "{v}");
    // the agent's call did not start the human's inbox
    let (ok, h) = s.json(&["--json", "inbox"]);
    assert!(ok, "{h}");
    assert_eq!(h["started"], true, "{h}");
}

#[test]
fn ib9_ack_prints_acked_and_empties_the_inbox() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let sp = spec_doc(&mut r);
    r.close().unwrap();
    let (ok, v) = s.json_as("agent:a", Some("a"), &["--json", "inbox"]);
    assert!(ok, "{v}");
    assert_eq!(v["started"], true, "{v}");
    // a second agent's change makes a tension with agent a's, with no daemon running
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    put(&mut r, "b", cite(&sp.section, &sp.a));
    put(&mut r, "a", upd(&sp.a, "A changed."));
    r.close().unwrap();
    let out = s.rdy_as("agent:a", Some("a"), &["inbox", "--ack"]);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("tension_opened  tension ")),
        "{text}"
    );
    assert!(lines.iter().any(|l| l.starts_with("cursor: ")), "{text}");
    assert_eq!(lines.last(), Some(&"acked"), "{text}");
    let again = stdout(&s.rdy_as("agent:a", Some("a"), &["inbox"]));
    let lines: Vec<&str> = again.lines().collect();
    assert_eq!(lines.len(), 1, "{again}");
    assert!(lines[0].starts_with("cursor: "), "{again}");
}
