//! relay1 (FW-B9): a human's approval relayed by an agent is recorded as such.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
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
    let (ok, v) = s.json_as(
        "agent:relay",
        Some("run-7"),
        &[
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "yes, land it",
            "--question",
            "land it?",
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(
        (v["approved"].as_str(), v["assurance"].as_str()),
        (Some(pref.as_str()), Some("relayed"))
    );
    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    let top = &rows.as_array().unwrap()[0];
    assert_eq!(top["assurance"], "relayed");
    assert_eq!(top["approved_by"], "human:qa", "the person approves");
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
    assert_eq!(lines[1], "summary: test summary", "{text}");
    assert!(
        lines[2].starts_with("landed ") && lines[2].ends_with(&format!("({pref})")),
        "{text}"
    );
    let (_, rows) = s.json(&["--json", "log"]);
    assert_eq!(rows[0]["assurance"], "local");
    assert!(rows[0]["relay_evidence"].is_null());
}

#[test]
fn relay1_flags_pair_the_version_must_be_exact_and_a_moved_proposal_is_refused() {
    let (s, pref) = proposed();
    let (ok, _, err) = out(&s.rdy(&["approve", &pref, "--relayed-by", "agent:r"]));
    assert!(
        !ok && err.contains("--evidence"),
        "clap pairs the flags: {err}"
    );
    let (ok, _, err) = out(&s.rdy(&["approve", &pref, "--evidence", "q"]));
    assert!(!ok && err.contains("--relayed-by"), "{err}");
    let stable = pref.split('@').next().unwrap().to_string();
    let (ok, v) = s.json_as(
        "agent:r",
        None,
        &[
            "--json",
            "approve",
            &stable,
            "--relayed-by",
            "agent:r",
            "--evidence",
            "q",
        ],
    );
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("exact version"),
        "{v}"
    );
    let (ok, v) = s.json_as(
        "agent:r",
        None,
        &[
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            "agent:r",
            "--evidence",
            "   ",
        ],
    );
    assert!(!ok && v["code"] == "E_BAD_INPUT", "{v}");
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
        r.propose(&writer(), "work", "t2", Some(TEST_SUMMARY))
            .unwrap();
    });
    let (ok, v) = s.json_as(
        "agent:r",
        None,
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
    assert!(!ok);
    assert_eq!(v["code"], "E_PROPOSAL_MOVED", "{v}");
    let (ok, v) = s.json_as(
        "agent:r",
        None,
        &[
            "--json",
            "approve",
            &format!("{stable}@2"),
            "--relayed-by",
            "agent:r",
            "--evidence",
            "q",
            "--session",
            "chat-42",
        ],
    );
    assert!(ok, "the agent relays as itself: {v}");
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
    let pref = r
        .propose(&writer(), "work", "t", Some(TEST_SUMMARY))
        .unwrap()
        .r#ref;
    let e = Local::new(r);
    let h = writer();
    let cx = OpContext {
        engine: &e,
        session: &h,
        human: "human:qa",
        cwd: d.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let a = agent("agent:r", Some("x"));
    let ax = OpContext {
        engine: &e,
        session: &a,
        human: "human:qa",
        cwd: d.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let err = dispatch(
        &ax,
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
    let (ok, v) = s.json_as(
        "agent:relay",
        Some("run-7"),
        &[
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "yes",
            "--question",
            "land it?",
        ],
    );
    assert!(ok, "{v}");
    let (ok, text, err) = out(&s.rdy(&["log"]));
    assert!(ok, "{err}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].contains(&format!("approved {pref}"))
            && lines[0].contains("relayed by agent:relay (run run-7)"),
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
        "summary",
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
        let po = r.propose(&o, "other", "o", Some(TEST_SUMMARY)).unwrap();
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

#[test]
fn msw2_the_approve_op_refuses_an_empty_relay_actor_run_or_session_ref() {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "x"}]),
    )
    .unwrap();
    let pref = r
        .propose(&writer(), "work", "t", Some(TEST_SUMMARY))
        .unwrap()
        .r#ref;
    let e = Local::new(r);
    let a = agent("agent:r", Some("x"));
    let cx = OpContext {
        engine: &e,
        session: &a,
        human: "human:qa",
        cwd: d.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    // a relay flag outside the actor form is the grammar's refusal, another actor is not the
    // session's own (D-HA7YZ, D-PWB82), and an empty session reference is refused
    for (by, sref, code) in [
        ("agent:r/", None, E_BAD_INPUT),
        ("  ", None, E_BAD_INPUT),
        ("agent:r/x", None, E_PERMISSION),
        ("agent:r", Some(""), E_BAD_INPUT),
        ("agent:r", Some("  "), E_BAD_INPUT),
    ] {
        let mut args = json!({"proposal": pref, "relayed_by": by, "evidence": "yes"});
        if let Some(sr) = sref {
            args["session_ref"] = json!(sr);
        }
        let err = dispatch(&cx, "approve", args).unwrap_err();
        assert_eq!(err.code, code, "{by:?} {sref:?}: {err}");
    }
    let v = dispatch(
        &cx,
        "approve",
        json!({"proposal": pref, "relayed_by": "agent:r", "evidence": "yes"}),
    )
    .unwrap();
    assert_eq!(v["assurance"], "relayed");
    e.close().unwrap();
}

/// The relay's argv after `approve <pref>`: the flags an agent passes for `by`.
fn relay_args<'a>(pref: &'a str, by: &'a str) -> Vec<&'a str> {
    vec![
        "--json",
        "approve",
        pref,
        "--relayed-by",
        by,
        "--evidence",
        "Yes",
    ]
}

#[test]
fn df2_relay_needs_human_actor() {
    let (s, pref) = proposed();
    let (ok, v) = s.json_as(
        "agent:writer-a",
        Some("r1"),
        &[
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            "agent:writer-a",
            "--question",
            "Land it?",
            "--evidence",
            "Yes",
            "--session",
            "chat:x",
        ],
    );
    assert!(ok, "the agent relays the person's approval as itself: {v}");
    assert_eq!(v["assurance"], "relayed", "{v}");
    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    let top = &rows[0];
    assert_eq!(top["approved_by"], "human:qa", "{top}");
    assert_eq!(
        top["relayed_by"],
        json!({"actor": "agent:writer-a", "run": "r1"}),
        "the relay names the agent that ran it, with its run"
    );
    assert_eq!(
        top["relay_evidence"],
        json!({"quoted": "Yes", "session_ref": "chat:x", "question": "Land it?"})
    );
    let (ok, v) = s.json(&["--json", "fsck"]);
    assert!(ok, "{v}");
}

#[test]
fn df2_an_agent_relays_only_as_itself_and_never_alone() {
    let (s, pref) = proposed();
    let refused = |v: &Value, message: &str| {
        assert_eq!(v["code"], "E_PERMISSION", "{v}");
        assert_eq!(v["message"], message, "{v}");
    };
    // (a) no relay flags: an agent never approves alone
    let (ok, v) = s.json_as("agent:writer-a", Some("r1"), &["--json", "approve", &pref]);
    assert!(!ok);
    refused(&v, "an agent approves only by relaying a person's approval");
    assert_eq!(
        v["fix"],
        "ask the person, then pass --relayed-by <your actor> --evidence \"<their words>\" --question \"<what you asked>\"",
        "{v}"
    );
    // (b) another actor
    let (ok, v) = s.json_as(
        "agent:writer-a",
        Some("r1"),
        &relay_args(&pref, "agent:other"),
    );
    assert!(!ok);
    refused(
        &v,
        "agent:writer-a (run r1) relays only as itself, not as agent:other",
    );
    assert_eq!(
        v["fix"], "pass --relayed-by agent:writer-a with the person's words in --evidence",
        "{v}"
    );
    // (c) its own actor joined to another run: another actor, never split
    let (ok, v) = s.json_as(
        "agent:writer-a",
        Some("r1"),
        &relay_args(&pref, "agent:writer-a/r2"),
    );
    assert!(!ok);
    refused(
        &v,
        "agent:writer-a (run r1) relays only as itself, not as agent:writer-a/r2",
    );
    // (c') its own actor joined to its own run: refused too, the flag names the actor alone
    let (ok, v) = s.json_as(
        "agent:writer-a",
        Some("r1"),
        &relay_args(&pref, "agent:writer-a/r1"),
    );
    assert!(!ok);
    refused(
        &v,
        "agent:writer-a (run r1) relays only as itself, not as agent:writer-a/r1",
    );
    // (d) no evidence: clap pairs the flags, as today
    let o = s.rdy_as(
        "agent:writer-a",
        Some("r1"),
        &["--json", "approve", &pref, "--relayed-by", "agent:writer-a"],
    );
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    // (e) a session with no run names a run
    let (ok, v) = s.json_as(
        "agent:writer-b",
        None,
        &relay_args(&pref, "agent:writer-b/r9"),
    );
    assert!(!ok);
    refused(
        &v,
        "agent:writer-b relays only as itself, not as agent:writer-b/r9",
    );
    let (ok, v) = s.json(&["--json", "log"]);
    assert!(ok, "{v}");
    assert_eq!(v.as_array().unwrap().len(), 1, "only the seed landed: {v}");
    let (ok, v) = s.json(&["--json", "show", &pref]);
    assert!(ok, "{v}");
    assert!(
        v["proposal"]["state"] == "open" && v["proposal"]["landing"].is_null(),
        "the proposal is still unlanded: {v}"
    );
}

#[test]
fn df2_a_person_does_not_relay_for_an_agent() {
    let (s, pref) = proposed();
    let (ok, v) = s.json(&relay_args(&pref, "agent:writer-a"));
    assert!(!ok);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    assert_eq!(
        v["message"], "a person approves as themselves; only the relaying agent records a relay",
        "{v}"
    );
    assert_eq!(
        v["fix"],
        "drop --relayed-by to approve yourself, or let the agent run rdy approve with RDY_ACTOR set to its actor",
        "{v}"
    );
    let (ok, v) = s.json(&["--json", "approve", &pref]);
    assert!(ok, "{v}");
    assert_eq!(v["assurance"], "local", "{v}");
}

#[test]
fn df2_a_relay_lands_without_a_daemon() {
    let (s, pref) = proposed();
    // `proposed` stops the daemon; --no-daemon opens the store in the command's own process
    assert!(s.daemon_pid().is_none(), "no daemon runs");
    let mut args = vec!["--no-daemon"];
    args.extend(relay_args(&pref, "agent:writer-a"));
    let (ok, v) = s.json_as("agent:writer-a", Some("r1"), &args);
    assert!(ok, "{v}");
    assert_eq!(v["assurance"], "relayed", "{v}");
    let (ok, rows) = s.json(&["--no-daemon", "--json", "log"]);
    assert!(ok, "{rows}");
    assert_eq!(rows[0]["approved_by"], "human:qa", "{rows}");
    assert_eq!(
        rows[0]["relayed_by"],
        json!({"actor": "agent:writer-a", "run": "r1"})
    );
    assert!(s.daemon_pid().is_none(), "no daemon started");
}

/// The `--relayed-by` value a refusal's fix names (`pass --relayed-by <X> with …`).
fn fix_flag(v: &Value) -> String {
    let fix = v["fix"].as_str().unwrap_or_else(|| panic!("no fix: {v}"));
    let rest = fix
        .strip_prefix("pass --relayed-by ")
        .unwrap_or_else(|| panic!("the fix names a flag: {v}"));
    rest.split(' ').next().unwrap().to_string()
}

#[test]
fn df2_an_actor_with_a_slash_relays_as_itself() {
    // an agent actor may contain '/' (Session::agent refuses it only in a run)
    for (by, no_daemon) in [("agent:a/b", false), ("agent:a/b", true)] {
        let (s, pref) = proposed();
        let mut args = if no_daemon {
            vec!["--no-daemon"]
        } else {
            vec![]
        };
        args.extend(relay_args(&pref, by));
        let (ok, v) = s.json_as("agent:a/b", Some("r1"), &args);
        assert!(ok, "{by} (no daemon: {no_daemon}): {v}");
        assert_eq!(v["assurance"], "relayed", "{v}");
        let mut log = if no_daemon {
            vec!["--no-daemon"]
        } else {
            vec![]
        };
        log.extend(["--json", "log"]);
        let (ok, rows) = s.json(&log);
        assert!(ok, "{rows}");
        assert_eq!(rows[0]["approved_by"], "human:qa", "{rows}");
        assert_eq!(
            rows[0]["relayed_by"],
            json!({"actor": "agent:a/b", "run": "r1"}),
            "{by}: the record is the session's actor and run"
        );
    }
    // the actor joined to a run is outside the actor form, with a run or without one
    let (s, pref) = proposed();
    for run in [Some("r1"), None] {
        let (ok, v) = s.json_as("agent:a/b", run, &relay_args(&pref, "agent:a/b/r1"));
        assert!(!ok);
        assert_eq!(
            (v["code"].as_str(), v["message"].as_str()),
            (
                Some("E_BAD_INPUT"),
                Some("the actor \"agent:a/b/r1\" is not in rdy's actor form")
            ),
            "{v}"
        );
    }
    let (ok, v) = s.json_as("agent:a/b", None, &relay_args(&pref, "agent:a/b"));
    assert!(ok, "{v}");
    let (_, rows) = s.json(&["--json", "log"]);
    assert_eq!(
        rows[0]["relayed_by"],
        json!({"actor": "agent:a/b", "run": null})
    );
    // a prefix of the actor is another actor
    let (s, pref) = proposed();
    for by in ["agent:a", "agent:a/r1"] {
        let (ok, v) = s.json_as("agent:a/b", Some("r1"), &relay_args(&pref, by));
        assert!(!ok, "{by}: {v}");
        assert_eq!(
            v["message"],
            format!("agent:a/b (run r1) relays only as itself, not as {by}"),
            "{v}"
        );
    }
}

#[test]
fn df2_a_refused_relay_s_fix_when_followed_lands() {
    for (actor, run, wrong) in [
        ("agent:writer-a", Some("r1"), "agent:other"),
        ("agent:writer-a", Some("r1"), "agent:writer-a/r2"),
        ("agent:writer-b", None, "agent:writer-b/r9"),
        ("agent:a/b", Some("r1"), "agent:a"),
        ("agent:a/b", Some("r1"), "agent:a/c"),
        ("agent:a/b", None, "agent:b"),
    ] {
        let (s, pref) = proposed();
        let (ok, v) = s.json_as(actor, run, &relay_args(&pref, wrong));
        assert!(!ok, "{actor} as {wrong}: {v}");
        assert_eq!(v["code"], "E_PERMISSION", "{v}");
        let by = fix_flag(&v);
        let (ok, v) = s.json_as(actor, run, &relay_args(&pref, &by));
        assert!(
            ok,
            "{actor}/{run:?}: the fix's --relayed-by {by} lands: {v}"
        );
        assert_eq!(v["assurance"], "relayed", "{v}");
    }
}

#[test]
fn df2_the_approve_reply_names_the_recorded_relayer() {
    // the flag names no run; the record takes the session's run, and so does the reply
    let (s, pref) = proposed();
    let (ok, text, err) = out(&s.rdy_as(
        "agent:w",
        Some("rb"),
        &[
            "approve",
            &pref,
            "--relayed-by",
            "agent:w",
            "--evidence",
            "Yes",
        ],
    ));
    assert!(ok, "{text}{err}");
    assert!(text.contains(", relayed by agent:w (run rb)"), "{text}");
    let (_, rows) = s.json(&["--json", "log"]);
    assert_eq!(
        rows[0]["relayed_by"],
        json!({"actor": "agent:w", "run": "rb"})
    );
    let (s, pref) = proposed();
    let (ok, v) = s.json_as("agent:w", Some("rb"), &relay_args(&pref, "agent:w"));
    assert!(ok, "{v}");
    let (_, rows) = s.json(&["--json", "log"]);
    assert_eq!(v["relayed_by"], rows[0]["relayed_by"], "{v}");
    assert_eq!(v["relayed_by"], json!({"actor": "agent:w", "run": "rb"}));
    // a local approval's reply names no relayer
    let (s, pref) = proposed();
    let (ok, v) = s.json(&["--json", "approve", &pref]);
    assert!(ok, "{v}");
    assert!(v.get("relayed_by").is_none(), "{v}");
}
