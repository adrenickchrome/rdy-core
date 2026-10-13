//! pcq1: consequences at the gate (Q2b, amendment §4.5 item 4): rdy_propose reports what landing
//! the version would make suspect in canon; the approval reports the same, with the same ids.
use rdy_engine::consequence::{consequence_id, land_and_report, propose_and_report};
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

fn upd(id: &BlockId, body: &str) -> Value {
    json!([{"op": "update", "id": id.to_string(), "body": body}])
}
fn entry<'a>(v: &'a Value, block: &BlockId) -> &'a Value {
    v["consequences"]
        .as_array()
        .unwrap_or_else(|| panic!("no consequences: {v}"))
        .iter()
        .find(|c| c["block"] == block.to_string())
        .unwrap_or_else(|| panic!("no entry for {block}: {v}"))
}
fn changed_a() -> (tempfile::TempDir, Local, Spec) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    write(&mut r, "w", upd(&s.a, "A, changed.")).unwrap();
    (d, Local::new(r), s)
}

#[test]
fn pcq1_propose_reports_what_landing_would_make_suspect_in_canon() {
    let (_d, e, s) = changed_a();
    let v = propose_and_report(&e, None, |r| r.propose(&writer(), "w", "Change A")).unwrap();
    let b = entry(&v, &s.b);
    assert_eq!(
        (b["kind"].as_str(), b["follow_up"].as_str()),
        (Some("suspect"), Some("update"))
    );
    let version: Hash = v["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(b["id"], consequence_id(&version, "suspect", &s.b));
    assert!(
        b.get("already_written").is_none(),
        "canon consequences never mark it"
    );
}

#[test]
fn pcq1_the_approval_reports_the_same_consequences_with_the_same_ids() {
    let (_d, e, _s) = changed_a();
    let p = propose_and_report(&e, None, |r| r.propose(&writer(), "w", "Change A")).unwrap();
    let pref = p["ref"].as_str().unwrap().to_string();
    let l = land_and_report(&e, move |r| {
        r.land_with(&approver(), &pref, LandOptions::default())
    })
    .unwrap();
    assert_eq!(l["version"], p["id"]);
    assert_eq!(l["consequences"], p["consequences"]);
}

#[test]
fn pcq1_a_version_that_does_not_merge_onto_canon_says_why_it_has_no_consequences() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(&mut r, "w", upd(&s.a, "A from w.")).unwrap();
    write(&mut r, "o", upd(&s.a, "A from o.")).unwrap();
    land(&mut r, "o").unwrap();
    let e = Local::new(r);
    let v = propose_and_report(&e, None, |r| r.propose(&writer(), "w", "Change A")).unwrap();
    assert!(!v["blocking"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(v["consequences"], json!([]));
    assert!(
        v["consequences_error"]
            .as_str()
            .unwrap()
            .starts_with("E_CONFLICT_UNRESOLVED: no consequences while "),
        "{v}"
    );
}

#[test]
fn pcq1_the_cli_prints_consequences_at_propose_and_after_the_landing_line() {
    let s = Served::new();
    let sp = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let sp = spec_doc(&mut r);
        write(&mut r, "w", upd(&sp.a, "A, changed.")).unwrap();
        r.close().unwrap();
        sp
    };
    let line = format!("suspect {}: update (references {})", sp.b, sp.a);
    let out = s.rdy(&["propose", "--title", "Change A", "--view", "w"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.lines().any(|l| l.trim() == line), "{text}");
    assert!(
        text.trim_end()
            .lines()
            .last()
            .unwrap()
            .starts_with("approve with: "),
        "{text}"
    );
    let pref = text
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();
    let out = s.rdy(&["approve", &pref]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[1].starts_with("landed "), "{text}");
    assert_eq!(lines[2], "consequences:", "{text}");
    assert!(lines.iter().any(|l| l.trim() == line), "{text}");
    s.stop();
}

#[test]
fn pcq1_rdy_propose_over_mcp_returns_consequences_for_canon() {
    let s = Served::new();
    let (sp, cb) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let sp = spec_doc(&mut r);
        let cb = write_as(
            &mut r,
            &as_agent("agent:b", "r1"),
            "vb",
            upd(&sp.a, "A, by b."),
        )
        .unwrap()
        .change;
        r.close().unwrap();
        (sp, cb)
    };
    let mut c = McpClient::start_proxy(&s);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "writer"}),
    );
    assert!(r.code().is_none());
    c.call("rdy_view", json!({"op": "include", "changes": [cb]}))
        .ok();
    let p = c
        .call(
            "rdy_propose",
            json!({"title": "Take b's A", "summary": "B's wording of A.", "max_tokens": 4000}),
        )
        .ok();
    assert!(
        p["consequences"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["block"] == sp.b.to_string() && x["kind"] == "suspect"),
        "{p}"
    );
    assert_eq!(p["summary"], "B's wording of A.");
    drop(c);
    s.stop();
}

#[test]
fn psum1_a_summary_is_stored_on_the_version_and_a_new_version_keeps_it_unless_given() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(&mut r, "w", upd(&s.a, "A, changed.")).unwrap();
    let p1 = r
        .propose_subset(
            &writer(),
            "w",
            "Change A",
            None,
            Some("A now says what we ship."),
        )
        .unwrap();
    assert_eq!(p1.summary.as_deref(), Some("A now says what we ship."));
    let v1: Hash = p1.id.parse().unwrap();
    assert_eq!(
        r.read().reader().proposal(&v1).unwrap().summary.as_deref(),
        Some("A now says what we ship.")
    );
    write(&mut r, "w", upd(&s.c, "C, changed.")).unwrap();
    let p2 = r
        .propose_subset(&writer(), "w", "Change A and C", None, None)
        .unwrap();
    assert_eq!(p2.summary, p1.summary, "a new version keeps the summary");
    let p3 = r
        .propose_subset(&writer(), "w", "Change A and C", None, Some("  "))
        .unwrap();
    assert_eq!(
        p3.summary, p1.summary,
        "a blank summary is none, so it is kept"
    );
    let p4 = r
        .propose_subset(
            &writer(),
            "w",
            "Change A and C",
            None,
            Some("A and C, together."),
        )
        .unwrap();
    assert_eq!(p4.summary.as_deref(), Some("A and C, together."));
    let l = r
        .land_with(&approver(), &p4.r#ref, LandOptions::default())
        .unwrap();
    assert_eq!(l.summary.as_deref(), Some("A and C, together."));
    assert_eq!(
        r.read().log_rows().unwrap()[0].summary.as_deref(),
        Some("A and C, together.")
    );
}

#[test]
fn psum1_the_cli_takes_a_summary_and_approve_prints_it_before_the_landing_line() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let sp = spec_doc(&mut r);
        write(&mut r, "w", upd(&sp.c, "C, changed.")).unwrap();
        r.close().unwrap();
    }
    let out = s.rdy(&[
        "propose",
        "--title",
        "Change C",
        "--view",
        "w",
        "--summary",
        "C is clearer.",
    ]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[1], "summary: C is clearer.", "{text}");
    let pref = lines[0].split_whitespace().nth(1).unwrap().to_string();
    let out = s.rdy(&["approve", &pref]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("approving "), "{text}");
    assert_eq!(lines[1], "summary: C is clearer.", "{text}");
    assert!(lines[2].starts_with("landed "), "{text}");
    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    assert_eq!(rows[0]["summary"], "C is clearer.");
    s.stop();
}
