//! relay1 (FW-B9): a human's approval relayed by an agent is recorded as such.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

/// Runs `f` on the project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
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
        r.propose(&writer(), "work", "t").unwrap().r#ref
    });
    (s, pref)
}

fn out(o: &std::process::Output) -> (bool, String, String) {
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

/// The keys of a JSON object, sorted (a parsed object does not keep the printed order).
fn keys(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort_unstable();
    k
}

#[test]
fn relay1_a_relayed_approval_round_trips_in_json_and_log_and_fsck_accepts_it() {
    let (s, pref) = proposed();
    let (ok, v) = s.json(&[
        "--json",
        "approve",
        &pref,
        "--relayed-by",
        "agent:relay/run-7",
        "--evidence",
        "yes, land it",
        "--question",
        "land it?",
    ]);
    assert!(ok, "{v}");
    assert_eq!(
        (v["approved"].as_str(), v["assurance"].as_str()),
        (Some(pref.as_str()), Some("relayed"))
    );
    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    let top = &rows.as_array().unwrap()[0];
    assert_eq!(top["assurance"], "relayed");
    assert_eq!(
        top["relayed_by"],
        json!({"actor": "agent:relay", "run": "run-7"})
    );
    assert_eq!(
        top["relay_evidence"],
        json!({"quoted": "yes, land it", "session_ref": "cli", "question": "land it?"})
    );
    assert_eq!(
        top["evidence"],
        json!({"kind": "text", "options": null, "choice": null, "card_hash": null})
    );
    assert_eq!(top["approved"], pref.as_str());
    assert_eq!(top["approved_version"].as_str().unwrap().len(), 64);
    assert_eq!(top["prev"], rows[1]["landing"]);
    assert_eq!(top["rebased_from"], json!([]));
    assert!(
        rows[1]["relayed_by"].is_null(),
        "a local landing carries no relay"
    );
    assert!(rows[1]["evidence"].is_null());
    let (ok, v) = s.json(&["--json", "fsck"]);
    assert!(ok, "{v}");
    let (ok, v) = s.json(&["--json", "approve", &pref]);
    assert!(!ok);
    assert_eq!(v["code"], "E_ALREADY_LANDED", "{v}");
}

#[test]
fn relay1_a_plain_approve_stays_local_and_prints_the_version_first() {
    let (s, pref) = proposed();
    let (ok, text, err) = out(&s.rdy(&["approve", &pref]));
    assert!(ok, "{text}{err}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], format!("approving {pref}: t (1 change(s))"));
    assert!(
        lines[1].starts_with("landed ") && lines[1].ends_with(&format!("({pref})")),
        "{text}"
    );
    let (_, rows) = s.json(&["--json", "log"]);
    assert_eq!(rows[0]["assurance"], "local");
    assert!(rows[0]["relay_evidence"].is_null());
}

#[test]
fn relay1_flags_pair_the_version_must_be_exact_only_the_human_relays_and_a_moved_proposal_is_refused()
 {
    let (s, pref) = proposed();
    let (ok, _, err) = out(&s.rdy(&["approve", &pref, "--relayed-by", "agent:r"]));
    assert!(
        !ok && err.contains("--evidence"),
        "clap pairs the flags: {err}"
    );
    let (ok, _, err) = out(&s.rdy(&["approve", &pref, "--evidence", "q"]));
    assert!(!ok && err.contains("--relayed-by"), "{err}");
    let stable = pref.split('@').next().unwrap().to_string();
    let (ok, v) = s.json(&[
        "--json",
        "approve",
        &stable,
        "--relayed-by",
        "agent:r",
        "--evidence",
        "q",
    ]);
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("exact version"),
        "{v}"
    );
    let (ok, v) = s.json(&[
        "--json",
        "approve",
        &pref,
        "--relayed-by",
        "agent:r",
        "--evidence",
        "   ",
    ]);
    assert!(!ok && v["code"] == "E_BAD_INPUT", "{v}");
    let (ok, v) = s.json_as(
        "agent:r",
        Some("x"),
        &[
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            "agent:r",
            "--evidence",
            "q",
        ],
    );
    assert!(
        !ok && v["code"] == "E_PERMISSION",
        "an agent cannot approve, relayed or not: {v}"
    );
    // a second version appears; relaying the first is a moved proposal
    with_rdy(&s, |r| {
        let section = r
            .read()
            .materialized(&Scope::Canon)
            .unwrap()
            .iter()
            .find(|b| b.block_type == "section")
            .unwrap()
            .id
            .to_string();
        write(
            r,
            "work",
            json!([{"op": "create", "type": "claim", "parent": section, "body": "y"}]),
        )
        .unwrap();
        r.propose(&writer(), "work", "t2").unwrap();
    });
    let (ok, v) = s.json(&[
        "--json",
        "approve",
        &pref,
        "--relayed-by",
        "agent:r",
        "--evidence",
        "q",
    ]);
    assert!(!ok);
    assert_eq!(v["code"], "E_PROPOSAL_MOVED", "{v}");
    let (ok, v) = s.json(&[
        "--json",
        "approve",
        &format!("{stable}@2"),
        "--relayed-by",
        "agent:r",
        "--evidence",
        "q",
        "--session",
        "chat-42",
    ]);
    assert!(ok, "{v}");
    let (_, rows) = s.json(&["--json", "log"]);
    assert_eq!(
        rows[0]["relay_evidence"],
        json!({"quoted": "q", "session_ref": "chat-42", "question": null})
    );
    assert_eq!(
        rows[0]["relayed_by"],
        json!({"actor": "agent:r", "run": null})
    );
}

#[test]
fn relay1_the_approve_op_refuses_half_a_relay() {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "x"}]),
    )
    .unwrap();
    let pref = r.propose(&writer(), "work", "t").unwrap().r#ref;
    let e = Local::new(r);
    let h = writer();
    let cx = OpContext {
        engine: &e,
        session: &h,
        cwd: d.path(),
    };
    let err = dispatch(
        &cx,
        "approve",
        json!({"proposal": pref, "relayed_by": "agent:r"}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    let err = dispatch(
        &cx,
        "approve",
        json!({"proposal": pref, "session_ref": "x"}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    let err = dispatch(&cx, "approve", json!({"proposal": pref, "question": "q?"})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    let v: Value = dispatch(&cx, "approve", json!({"proposal": pref})).unwrap();
    assert_eq!(v["assurance"], "local");
    e.close().unwrap();
}

#[test]
fn logj1_log_text_shows_the_version_and_the_relay_and_tags_json_carries_the_tag_hash() {
    let (s, pref) = proposed();
    let (ok, v) = s.json(&[
        "--json",
        "approve",
        &pref,
        "--relayed-by",
        "agent:relay/run-7",
        "--evidence",
        "yes",
        "--question",
        "land it?",
    ]);
    assert!(ok, "{v}");
    let (ok, text, err) = out(&s.rdy(&["log"]));
    assert!(ok, "{err}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].contains(&format!("approved {pref}"))
            && lines[0].contains("relayed by agent:relay/run-7"),
        "{text}"
    );
    assert_eq!(
        lines[1],
        "    evidence (text): \"yes\" (asked: \"land it?\"; session cli)"
    );
    let (ok, v) = s.json(&["--json", "tag", "v1.0.0"]);
    assert!(ok, "{v}");
    let (ok, tags) = s.json(&["--json", "tags"]);
    assert!(ok, "{tags}");
    let t = &tags["versions"][0];
    assert_eq!(
        (
            t["tag"].as_str().unwrap().len(),
            t["landing"].as_str().unwrap().len()
        ),
        (64, 64),
        "{t}"
    );
    // every row carries every key (rdyhub reads weak evidence from the row)
    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    let mut all = [
        "landing",
        "prev",
        "proposal",
        "approved_version",
        "approved",
        "title",
        "approved_by",
        "proposed_by",
        "assurance",
        "relayed_by",
        "relay_evidence",
        "evidence",
        "rebased_from",
        "at",
        "at_ms",
        "changes",
        "tags",
    ];
    all.sort_unstable();
    let (relayed, local) = (&rows[0], &rows[1]);
    assert_eq!(keys(relayed), all, "{relayed}");
    assert_eq!(keys(local), all, "{local}");
    assert_eq!(relayed["tags"], json!(["v1.0.0"]));
    assert_eq!(relayed["landing"], t["landing"]);
    assert_eq!(relayed["approved_by"], "human:qa");
    assert_eq!(
        relayed["relayed_by"],
        json!({"actor": "agent:relay", "run": "run-7"})
    );
    assert_eq!(
        relayed["relay_evidence"],
        json!({"quoted": "yes", "session_ref": "cli", "question": "land it?"})
    );
    assert_eq!(relayed["evidence"]["kind"], "text");
    let at = relayed["at"].as_str().unwrap();
    assert!(
        at.len() == 20 && at.ends_with('Z') && &at[10..11] == "T",
        "RFC 3339 UTC: {at}"
    );
    assert!(relayed["at_ms"].as_i64().unwrap() > 0);
    assert!(
        local["prev"].is_null() && local["tags"] == json!([]),
        "{local}"
    );
    assert_eq!(local["assurance"], "local");
    assert_eq!(local["approved_by"], "human:qa");
    for k in ["relayed_by", "relay_evidence", "evidence"] {
        assert!(local[k].is_null(), "{k}: {local}");
    }
    assert_eq!(local["approved_version"].as_str().unwrap().len(), 64);
    assert!(
        local["approved"].as_str().unwrap().starts_with("P-"),
        "{local}"
    );
    assert_eq!(local["rebased_from"], json!([]));
}

#[test]
fn evk1_a_question_needs_a_relay() {
    let (s, pref) = proposed();
    let (ok, _, err) = out(&s.rdy(&["approve", &pref, "--question", "q?"]));
    assert!(
        !ok && err.contains("--relayed-by"),
        "clap pairs the flags: {err}"
    );
}

#[test]
fn hb1_strict_refuses_a_moved_base_for_one_call() {
    let (s, pref) = proposed();
    with_rdy(&s, |r| {
        let section = r
            .read()
            .materialized(&Scope::Canon)
            .unwrap()
            .iter()
            .find(|b| b.block_type == "section")
            .unwrap()
            .id
            .to_string();
        let o = as_agent("agent:o", "r1");
        write_as(
            r,
            &o,
            "other",
            json!([{"op": "create", "type": "claim", "parent": section, "body": "unrelated"}]),
        )
        .unwrap();
        let po = r.propose(&o, "other", "o").unwrap();
        r.land_report(&approver(), &po.r#ref).unwrap();
    });
    let (ok, v) = s.json(&["--json", "approve", &pref, "--strict"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_PROPOSAL_MOVED", "{v}");
    let (ok, v) = s.json(&["--json", "approve", &pref]);
    assert!(
        ok,
        "without --strict the approval survives the unrelated landing: {v}"
    );
}
