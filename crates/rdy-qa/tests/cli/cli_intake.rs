//! di12 (D-KQ4CA, D-JV6Z8, F4, F5): the CLI's parity for the "needs you" queue. `rdy pending`
//! prints the cards; `rdy answer` answers an item: a human session answers `local`, an agent
//! session only by relaying the person's words (the relay table, D-HA7YZ). The CLI has no Later.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{brief_of, spec_doc, stable_spec, write_cited};
use rdy_qa::fixtures::{land, version_of};
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{ag, created, put};
use serde_json::{Value, json};

/// Runs `f` on the project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

fn text(o: &std::process::Output) -> (bool, String) {
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

/// A served project with the spec doc in canon and one proposal for each name in `ready` (a new
/// claim and a summary) and each name in `bare` (no summary, so not ready). The refs of the
/// ready ones.
fn queued(ready: &[&str], bare: &[&str]) -> (Served, Vec<String>, Vec<String>) {
    let s = Served::new();
    let (r1, r2) = with_rdy(&s, |r| {
        let spec = spec_doc(r);
        let mut one = |view: &str, summary: Option<&str>| {
            put(
                r,
                view,
                json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                        "body": format!("A claim of {view}.")}]),
            );
            r.propose_subset(&ag(view), view, view, None, summary)
                .unwrap_or_else(|e| panic!("propose {view}: {e}"))
                .r#ref
        };
        let ready: Vec<String> = ready
            .iter()
            .map(|v| one(v, Some("Adds one claim.")))
            .collect();
        let bare: Vec<String> = bare.iter().map(|v| one(v, None)).collect();
        (ready, bare)
    });
    (s, r1, r2)
}

/// The queue as `rdy --json pending` prints it.
fn queue(s: &Served) -> Value {
    let (ok, v) = s.json(&["--json", "pending"]);
    assert!(ok, "{v}");
    v
}

fn item<'a>(q: &'a Value, r#ref: &str) -> &'a Value {
    q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == r#ref)
        .unwrap_or_else(|| panic!("{ref} in {q}"))
}

fn landings(s: &Served) -> usize {
    with_rdy(s, |r| r.read().log_rows().unwrap().len())
}

#[test]
fn di12_rdy_pending_prints_the_cards() {
    let (s, ready, bare) = queued(&["a", "b"], &["bare"]);
    let q = queue(&s);
    assert_eq!(q["snoozed"], 0, "{q}");
    assert_eq!(q["omitted"], 0, "{q}");
    let (ok, out) = text(&s.rdy(&["pending"]));
    assert!(ok, "{out}");
    // each card, one blank line between them; then each version that is not ready; then the count
    let cards: Vec<&str> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["card"].as_str().unwrap())
        .collect();
    assert_eq!(cards.len(), 2, "{q}");
    assert_eq!(item(&q, &ready[0])["kind"], "approve");
    let head = cards.join("\n\n");
    assert!(out.starts_with(&head), "{out}");
    let rest = &out[head.len()..];
    let not_ready: Vec<&str> = rest
        .lines()
        .filter(|l| l.starts_with("not ready: "))
        .collect();
    assert_eq!(not_ready.len(), 1, "{rest}");
    assert!(
        not_ready[0].starts_with(&format!("not ready: {}: ", bare[0])),
        "{not_ready:?}"
    );
    assert_eq!(out.trim_end().lines().last(), Some("0 snoozed"), "{out}");
    // the card budget: a small --max-tokens cuts the cards, not the hashes
    let (ok, v) = s.json(&["--json", "pending", "--max-tokens", "10"]);
    assert!(ok, "{v}");
    assert_eq!(
        item(&v, &ready[0])["card_hash"],
        item(&q, &ready[0])["card_hash"]
    );
    // an over-large budget is E_LIMIT
    let (ok, e) = s.json(&["--json", "pending", "--max-tokens", "100001"]);
    assert!(!ok);
    assert_eq!(e["code"], "E_LIMIT", "{e}");
    // with a session the queue still reads (the CLI has no Later, so nothing is hidden)
    let (ok, v) = s.json(&["--json", "pending", "--session", "chat-1"]);
    assert!(ok, "{v}");
    assert_eq!(v["snoozed"], 0, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
}

#[test]
fn di12_rdy_answer_answers_locally_from_a_human_session() {
    let (s, ready, _) = queued(&["a", "b"], &[]);
    let q = queue(&s);
    let base = landings(&s);
    // the CLI has no Later (F4), and half a relay never reaches rdy
    let (ok, e) = s.json(&["--json", "answer", &ready[0], "later"]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(e["message"], "the CLI has no Later", "{e}");
    let (ok, out) = text(&s.rdy(&["answer", &ready[0], "approve", "--session", "chat-1"]));
    assert!(!ok && out.contains("--relayed-by"), "{out}");
    // a person answers as themselves
    let (ok, e) = s.json(&[
        "--json",
        "answer",
        &ready[0],
        "approve",
        "--relayed-by",
        "agent:relay",
        "--evidence",
        "yes",
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_PERMISSION", "{e}");
    // a stale card refuses; the fresh card comes back in `corrected`
    let stale = "0".repeat(64);
    let (ok, e) = s.json(&["--json", "answer", &ready[0], "approve", "--card", &stale]);
    assert!(!ok);
    assert_eq!(e["code"], "E_PROPOSAL_MOVED", "{e}");
    assert_eq!(
        e["corrected"]["card_hash"],
        item(&q, &ready[0])["card_hash"],
        "{e}"
    );
    // and the text mode shows that card to the person
    let (ok, out) = text(&s.rdy(&["answer", &ready[0], "approve", "--card", &stale]));
    assert!(!ok && out.contains("error E_PROPOSAL_MOVED"), "{out}");
    assert!(
        out.contains(item(&q, &ready[0])["card"].as_str().unwrap()),
        "{out}"
    );
    assert_eq!(landings(&s), base, "a refusal lands nothing");
    // a local approval: text names the answer and the landing lines of rdy approve
    let label = item(&q, &ready[0])["options"][0]["label"].as_str().unwrap();
    let (ok, out) = text(&s.rdy(&["answer", &ready[0], "approve"]));
    assert!(ok, "{out}");
    assert!(
        out.starts_with(&format!("answered {}: {label}\n", ready[0])),
        "{out}"
    );
    assert!(out.contains("\nlanded "), "{out}");
    // JSON: the answer, bound to the card the person was shown; an approval keeps no note
    let fresh = queue(&s);
    let card = item(&fresh, &ready[1])["card_hash"].as_str().unwrap();
    let (ok, e) = s.json(&[
        "--json", "answer", &ready[1], "approve", "--card", card, "--note", "go",
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    let (ok, v) = s.json(&["--json", "answer", &ready[1], "approve", "--card", card]);
    assert!(ok, "{v}");
    assert_eq!(
        (
            v["act"].as_str(),
            v["assurance"].as_str(),
            v["relayed_by"].is_null()
        ),
        (Some("approve"), Some("local"), true),
        "{v}"
    );
    let rows = with_rdy(&s, |r| r.read().log_rows().unwrap());
    assert_eq!(rows.len(), base + 2);
    assert!(
        rows.iter()
            .all(|r| r.assurance == "local" && r.relayed_by.is_none())
    );
    let (_, fsck) = s.json(&["--json", "fsck"]);
    assert!(fsck.is_object(), "{fsck}");
}

/// A served project whose one proposal changes claim A, so landing it makes claim B suspect: the
/// proposal's ref and the consequence line `rdy approve` prints for it.
fn changing_a() -> (Served, String, String) {
    let s = Served::new();
    let (pref, line) = with_rdy(&s, |r| {
        let spec = spec_doc(r);
        put(
            r,
            "w",
            json!([{"op": "update", "id": spec.a.to_string(), "body": "A, changed."}]),
        );
        let p = r
            .propose_subset(&ag("w"), "w", "w", None, Some("Changes A."))
            .expect("propose");
        (
            p.r#ref,
            format!("suspect {}: confirm (references {})", spec.b, spec.a),
        )
    });
    (s, pref, line)
}

/// The consequence lines of an approval's text: one line for each entry.
fn consequence_lines(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("suspect "))
        .map(String::from)
        .collect()
}

#[test]
fn di12_rdy_answer_prints_the_consequences_rdy_approve_prints() {
    // the same landing, once through rdy approve and once through each way of rdy answer; block
    // ids differ between projects, so the lines compare with the ids blanked
    let blank = |l: &str| {
        l.split_whitespace()
            .map(|w| {
                let id = w.trim_matches(|c| c == ':' || c == '(' || c == ')');
                if id.starts_with("c-") { "<id>" } else { w }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (s, pref, line) = changing_a();
    let (ok, approved) = text(&s.rdy(&["approve", &pref]));
    assert!(ok, "{approved}");
    assert_eq!(consequence_lines(&approved), vec![line], "{approved}");
    let want: Vec<String> = consequence_lines(&approved)
        .iter()
        .map(|l| blank(l))
        .collect();
    assert_eq!(
        want,
        vec!["suspect <id> confirm (references <id>"],
        "{approved}"
    );
    for all in [false, true] {
        let (s, pref, line) = changing_a();
        let args: Vec<&str> = if all {
            vec!["answer", "--all-recommended", &pref]
        } else {
            vec!["answer", &pref, "approve"]
        };
        let (ok, out) = text(&s.rdy(&args));
        assert!(ok, "{out}");
        assert_eq!(consequence_lines(&out), vec![line], "{out}");
        let got: Vec<String> = consequence_lines(&out).iter().map(|l| blank(l)).collect();
        assert_eq!(got, want, "answer prints what approve prints");
        // and the JSON carries them beside the landing
        let (s, pref, _) = changing_a();
        let args: Vec<&str> = if all {
            vec!["--json", "answer", "--all-recommended", &pref]
        } else {
            vec!["--json", "answer", &pref, "approve"]
        };
        let (ok, v) = s.json(&args);
        assert!(ok, "{v}");
        let done = if all { &v[0] } else { &v };
        let c = done["landing"]["consequences"].as_array().unwrap();
        assert_eq!(c.len(), 1, "{v}");
        assert_eq!(c[0]["kind"], "suspect", "{v}");
    }
}

#[test]
fn di12_rdy_answer_relays_from_an_agent_session() {
    let (s, ready, _) = queued(&["a", "b", "c", "d"], &[]);
    let base = landings(&s);
    let (agent, run) = ("agent:relay", Some("r1"));
    // a landing moves canon, and the batch cards say so: read each hash when it is needed
    let hash_of = |r: &str| {
        item(&queue(&s), r)["card_hash"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let a_hash = hash_of(&ready[0]);
    // with no relay an agent cannot answer
    let (ok, e) = s.json_as(agent, run, &["--json", "answer", &ready[0], "approve"]);
    assert!(!ok);
    assert_eq!(e["code"], "E_PERMISSION", "{e}");
    assert_eq!(
        e["message"], "an agent answers only by relaying a person's words",
        "{e}"
    );
    // half a relay never reaches rdy: clap pairs the flags
    let (ok, out) = text(&s.rdy_as(
        agent,
        run,
        &[
            "answer",
            &ready[0],
            "approve",
            "--relayed-by",
            "agent:relay",
        ],
    ));
    assert!(!ok && out.contains("--evidence"), "{out}");
    // the relay names the agent as itself
    let (ok, e) = s.json_as(
        agent,
        run,
        &[
            "--json",
            "answer",
            &ready[0],
            "approve",
            "--card",
            &hash_of(&ready[0]),
            "--relayed-by",
            "agent:other",
            "--evidence",
            "yes",
        ],
    );
    assert!(!ok);
    assert_eq!(e["code"], "E_PERMISSION", "{e}");
    // a relayed answer names the card the person was shown
    let (ok, e) = s.json_as(
        agent,
        run,
        &[
            "--json",
            "answer",
            &ready[0],
            "approve",
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "yes",
        ],
    );
    assert!(!ok);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(landings(&s), base, "a refusal lands nothing");
    // the relayed approval: the person approves, the agent relays
    let (ok, out) = text(&s.rdy_as(
        agent,
        run,
        &[
            "answer",
            &ready[0],
            "approve",
            "--card",
            &hash_of(&ready[0]),
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "yes, land it",
            "--question",
            "Land the claim?",
            "--session",
            "chat-3",
        ],
    ));
    assert!(ok, "{out}");
    assert!(
        out.starts_with(&format!("answered {}: ", ready[0])),
        "{out}"
    );
    assert!(out.contains(", relayed by agent:relay (run r1)"), "{out}");
    // Later is a chat act: the CLI refuses it whoever asks, relayed or not (F4); nothing is
    // hidden and no record is written (MCP covers the session-scoped Later)
    let before = landings(&s);
    for relay_flags in [
        &[][..],
        &[
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "not now",
            "--session",
            "chat-3",
        ][..],
    ] {
        let card = hash_of(&ready[1]);
        let mut args = vec!["--json", "answer", &ready[1], "later", "--card", &card];
        args.extend_from_slice(relay_flags);
        let (ok, e) = s.json_as(agent, run, &args);
        assert!(!ok, "{e}");
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(e["message"], "the CLI has no Later", "{e}");
    }
    let (_, kept) = s.json_as(agent, run, &["--json", "pending", "--session", "chat-3"]);
    assert_eq!(kept["snoozed"], 0, "{kept}");
    assert!(
        kept["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["ref"] == ready[1].as_str()),
        "{kept}"
    );
    assert_eq!(landings(&s), before, "a refusal lands nothing");
    // the landing records the person, the relay and the card
    let rows = with_rdy(&s, |r| r.read().log_rows().unwrap());
    assert_eq!(rows.len(), base + 1);
    let row = &rows[0];
    assert_eq!(row.approved_by, "human:qa");
    assert_eq!(row.assurance, "relayed");
    let by = row.relayed_by.as_ref().unwrap();
    assert_eq!((by.actor.as_str(), by.run.as_deref()), (agent, run));
    let said = row.relay_evidence.as_ref().unwrap();
    assert_eq!(
        (
            said.quoted.as_str(),
            said.question.as_deref(),
            said.session_ref.as_str()
        ),
        ("yes, land it", Some("Land the claim?"), "chat-3")
    );
    let ev = row.evidence.as_ref().unwrap();
    assert_eq!(ev.kind, "card");
    assert_eq!(ev.card_hash.as_deref(), Some(a_hash.as_str()));
    // --all-recommended: each item its recommended option, each bound to its own card. The
    // first approval moves canon, so the second card the person saw is stale: the call stops
    // there, and the error lists what it did and carries the fresh card
    let both = [ready[2].clone(), ready[3].clone()];
    let (hc, hd) = (hash_of(&both[0]), hash_of(&both[1]));
    eprintln!("EVID before c: {}", item(&queue(&s), &both[1])["card"].as_str().unwrap().lines().filter(|l| l.contains("Base")).collect::<String>());
    let (ok, e) = s.json_as(
        agent,
        run,
        &[
            "--json",
            "answer",
            "--all-recommended",
            &both[0],
            &both[1],
            "--card",
            &hc,
            "--card",
            &hd,
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "do what you recommend",
        ],
    );
    eprintln!("EVID after: ok={ok} {}", item(&queue(&s), &both[1])["card"].as_str().map(|c| c.lines().filter(|l| l.contains("Base")).collect::<String>()).unwrap_or_default());
    eprintln!("EVID hash same: {}", hash_of(&both[1]) == hd);
    assert!(!ok);
    assert_eq!(e["code"], "E_PROPOSAL_MOVED", "{e}");
    let done = e["corrected"]["done"].as_array().expect("what was done");
    assert_eq!(done.len(), 1, "{e}");
    assert_eq!(
        (done[0]["ref"].as_str(), done[0]["act"].as_str()),
        (Some(both[0].as_str()), Some("approve")),
        "{e}"
    );
    assert_eq!(e["corrected"]["ref"], both[1].as_str(), "{e}");
    assert_eq!(landings(&s), base + 2);
    // a person's own --all-recommended binds to the versions, and lands what is left
    let (ok, out) = text(&s.rdy(&["answer", "--all-recommended", &ready[1], &both[1]]));
    assert!(ok, "{out}");
    assert_eq!(
        out.lines().filter(|l| l.starts_with("answered ")).count(),
        2,
        "{out}"
    );
    assert_eq!(landings(&s), base + 4);
    // --card goes once for each ref, or not at all
    let (ok, e) = s.json(&[
        "--json",
        "answer",
        "--all-recommended",
        &ready[0],
        &ready[1],
        "--card",
        &a_hash,
    ]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
}

/// `--brief` reaches the answer of a question under stable content (the ruling of task 8): the
/// refusal names the retry, a briefing the daemon does not know is stale, and the briefed
/// answer writes the decision.
#[test]
fn di12_rdy_answer_carries_the_brief_of_a_question_under_stable_content() {
    let s = Served::new();
    let (item_ref, section) = with_rdy(&s, |r| {
        let spec = stable_spec(r);
        let t = brief_of(r, &rdy_engine::Scope::Canon, &[&spec.section])
            .base
            .token;
        let w = write_cited(
            r,
            "qs",
            json!([{"op": "create", "type": "question", "id": "$q", "parent": spec.section.to_string(),
                    "fields": {"prompt": "Who approves refunds?", "title": "Who approves refunds?",
                               "why": "Support waits on this",
                               "options": ["Finance", "Support lead", "Either"],
                               "recommended": "Finance", "recommend_reason": "Finance owns the ledger"}}]),
            Some(&t),
        )
        .unwrap();
        land(r, "qs").unwrap();
        let q = created(&w, "$q");
        let v = version_of(r, &rdy_engine::Scope::Canon, &q.to_string());
        (format!("{q}@{}", &v[..12]), spec.section.to_string())
    });
    let q = queue(&s);
    let card = item(&q, &item_ref)["card_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let (agent, run) = ("agent:relay", Some("r1"));
    let relay = |extra: &[&str]| {
        let mut args = vec![
            "--json",
            "answer",
            &item_ref,
            "o1",
            "--card",
            &card,
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "Finance, please",
        ];
        args.extend_from_slice(extra);
        s.json_as(agent, run, &args)
    };
    let (ok, e) = relay(&[]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BRIEF_REQUIRED", "{e}");
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .contains("rdy_answer again with brief: <token>"),
        "{e}"
    );
    // the flag reaches the engine: a token the daemon never issued is stale
    let (ok, e) = relay(&["--brief", "brief:none"]);
    assert!(!ok);
    assert_eq!(e["code"], "E_BRIEF_STALE", "{e}");
    // a briefing of the section, from the same session, lets the answer through
    let (ok, b) = s.json_as(agent, run, &["--json", "brief", &section]);
    assert!(ok, "{b}");
    let token = b["base"]["token"]
        .as_str()
        .or_else(|| b["token"].as_str())
        .unwrap_or_else(|| panic!("a briefing token in {b}"))
        .to_string();
    let (ok, v) = relay(&["--brief", &token]);
    assert!(ok, "{v}");
    assert_eq!(v["act"], "answer_question", "{v}");
}

#[test]
fn di12_cli_and_mcp_give_the_same_items() {
    let (s, ready, _) = queued(&["a", "b"], &["bare"]);
    // the human's CLI queue, and the same queue through an agent session's MCP tool on the
    // project's daemon: the person is the project's, whoever asks
    let cli = queue(&s);
    let mut c = McpClient::start_proxy(&s);
    let ok = c.call(
        "rdy_session",
        json!({"actor": "agent:relay", "run": "r1", "role": "writer"}),
    );
    assert!(ok.is_ok(), "{ok:?}");
    let mcp = c.call("rdy_pending", json!({})).ok();
    assert_eq!(cli, mcp, "parity: one queue on every surface");
    assert_eq!(cli["items"].as_array().unwrap().len(), ready.len());
    assert_eq!(cli["not_ready"].as_array().unwrap().len(), 1);
    // the same card budget on both
    let (ok, cli) = s.json(&["--json", "pending", "--max-tokens", "50"]);
    assert!(ok, "{cli}");
    let mcp = c.call("rdy_pending", json!({"max_tokens": 50})).ok();
    assert_eq!(cli, mcp);
    // an agent's CLI sees the same queue as the human's
    let (ok, as_agent) = s.json_as(
        "agent:relay",
        Some("r1"),
        &["--json", "pending", "--max-tokens", "50"],
    );
    assert!(ok, "{as_agent}");
    assert_eq!(as_agent, cli);
}
