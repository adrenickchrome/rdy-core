//! di14 (plan Decision intake, Task 14; D-KQ4CA, D-JV6Z8, D-YFDG3, D-X61DT, D-HA7YZ): the chat path
//! end to end. One agent session through the real binary's MCP proxy captures decisions, proposes
//! them, reads the "needs you" queue, relays the person's answers to the exception and the batch,
//! and tags the version. No step is a CLI act, and nothing reaches the site.
use rdy_engine::Rdy;
use rdy_model::{AnswerAct, EvidenceKind};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::land;
use rdy_qa::http;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::tensions::put;
use serde_json::{Value, json};

/// The view the agent writes in, and the batch's title and summary (D-4PG9Y).
const VIEW: &str = "guest";
const TITLE: &str = "Guest links";
const SUMMARY: &str = "Lets a visitor open a shared doc through a link, and records two decisions.";
/// The agent's actor and run, and the chat session the person speaks in.
const AGENT: &str = "agent:relay";
const RUN: &str = "r1";
const CHAT: &str = "chat-14";

/// A served project that holds the spec doc in canon.
fn seeded() -> (Served, Spec) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    r.close().unwrap();
    (s, spec)
}

/// The prompt of the open question [`seeded_with_question`] lands.
const PROMPT: &str = "Who approves refunds?";

/// [`seeded`], and an open question with authored options in canon: it is a card in the queue
/// and a line on `questions.html`.
fn seeded_with_question() -> (Served, Spec) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    put(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "parent": spec.section.to_string(),
                "fields": {"prompt": PROMPT, "title": PROMPT, "why": "Support waits on this",
                           "options": ["Finance", "Support lead", "Either"]}}]),
    );
    land(&mut r, "qs").unwrap();
    r.close().unwrap();
    (s, spec)
}

/// The agent's MCP session through the proxy (the person is the served human, `human:qa`).
fn agent(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": AGENT, "run": RUN, "role": "writer"}),
    )
    .ok();
    c
}

/// Calls `tool`; when it asks for a briefing, calls it again with the fresh token.
fn briefed(c: &mut McpClient, tool: &str, args: Value) -> Value {
    match c.call(tool, args.clone()) {
        Reply::Ok(v) => v,
        Reply::ToolError(e) if e["briefing"]["base"]["token"].is_string() => {
            let mut again = args;
            again["brief"] = e["briefing"]["base"]["token"].clone();
            c.call(tool, again).ok()
        }
        other => panic!("{tool}: {other:?}"),
    }
}

/// The words the agent relays for the person.
fn words(extra: Value) -> Value {
    let mut v = json!({
        "evidence": "Yes, do that.",
        "question": "Shall I record your answer?",
        "session_ref": CHAT,
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

/// What the agent did before the person was asked.
struct Round {
    /// `P-x@n` of the version, which is the batch item's ref.
    pref: String,
    /// The item ref of the one exception: the `one_way` decision.
    exception: String,
}

/// Captures two decisions in view [`VIEW`], one of them `one_way`, and proposes the view with a
/// summary. The reply of `rdy_propose` names the version.
fn capture_and_propose(c: &mut McpClient, spec: &Spec) -> Round {
    for (statement, reversibility) in [
        ("Guests open a shared doc by link", "two_way"),
        ("A guest link never expires", "one_way"),
    ] {
        briefed(
            c,
            "rdy_decide",
            json!({
                "view": VIEW,
                "parent": spec.section.to_string(),
                "statement": statement,
                "title": statement,
                "why": "The person decides how guests reach a doc",
                "reversibility": reversibility,
                "affects": [spec.c.to_string()],
            }),
        );
    }
    let proposed = briefed(
        c,
        "rdy_propose",
        json!({"view": VIEW, "title": TITLE, "summary": SUMMARY}),
    );
    let pref = proposed["ref"]
        .as_str()
        .expect("the version's ref")
        .to_string();
    let queue = c.call("rdy_pending", json!({"session_ref": CHAT})).ok();
    let exception = exceptions(&queue, &pref).first().expect("an exception")["ref"]
        .as_str()
        .unwrap()
        .to_string();
    Round { pref, exception }
}

fn items(queue: &Value) -> Vec<Value> {
    queue["items"].as_array().expect("items").clone()
}

fn item_of(queue: &Value, r#ref: &str) -> Value {
    items(queue)
        .into_iter()
        .find(|i| i["ref"] == r#ref)
        .unwrap_or_else(|| panic!("{ref} in {queue}"))
}

/// The exception items that stand beside batch `pref`.
fn exceptions(queue: &Value, pref: &str) -> Vec<Value> {
    items(queue)
        .into_iter()
        .filter(|i| i["kind"] == "confirm" && i["batch"] == pref)
        .collect()
}

/// Relays the person's `option` on item `r#ref`, bound to the card in `queue`.
fn relay(c: &mut McpClient, queue: &Value, r#ref: &str, option: &str) -> Reply {
    let card = item_of(queue, r#ref);
    c.call(
        "rdy_answer",
        words(json!({"ref": r#ref, "option": option, "card_hash": card["card_hash"]})),
    )
}

/// The whole round: capture, propose, see the queue, confirm the exception, approve the batch,
/// tag. Every step is a tool call of one agent session.
fn chat_round(s: &Served, spec: &Spec) -> Round {
    let mut c = agent(s);
    let round = capture_and_propose(&mut c, spec);
    let queue = c.call("rdy_pending", json!({"session_ref": CHAT})).ok();
    assert_eq!(
        items(&queue).len(),
        2,
        "one batch and one exception: {queue}"
    );
    let batch = item_of(&queue, &round.pref);
    assert_eq!(batch["kind"], "approve", "{batch}");
    assert_eq!(batch["summary"], SUMMARY, "{batch}");
    // the card wraps its lines; the summary leads it, before the computed facts
    let card = batch["card"].as_str().unwrap();
    let flat = card.split_whitespace().collect::<Vec<_>>().join(" ");
    let (lead, facts) = flat.split_once("Facts (computed)").expect("computed facts");
    assert!(
        lead.contains(SUMMARY),
        "the author's summary leads the card: {card}"
    );
    assert!(!facts.contains(SUMMARY), "{card}");
    let ex = exceptions(&queue, &round.pref);
    assert_eq!(ex.len(), 1, "{queue}");
    assert_eq!(ex[0]["exception"], "one_way", "{queue}");
    assert_eq!(ex[0]["ref"], round.exception.as_str(), "{queue}");
    // the person confirms the exception on its card
    let done = relay(&mut c, &queue, &round.exception, "confirm").ok();
    assert_eq!(done["act"], "confirm", "{done}");
    assert_eq!(done["assurance"], "relayed", "{done}");
    // the batch card now lists no open exception, and the person approves it
    let queue = c.call("rdy_pending", json!({"session_ref": CHAT})).ok();
    assert_eq!(items(&queue).len(), 1, "only the batch is left: {queue}");
    let landed = relay(&mut c, &queue, &round.pref, "approve").ok();
    assert_eq!(landed["act"], "approve", "{landed}");
    assert_eq!(landed["assurance"], "relayed", "{landed}");
    assert_eq!(
        landed["landing"]["approved"],
        round.pref.as_str(),
        "{landed}"
    );
    // the person names the version
    let tagged = c
        .call(
            "rdy_tag",
            words(json!({"version": "v0.1.0", "evidence": "Tag it v0.1.0."})),
        )
        .ok();
    assert_eq!(tagged["tagged_by"], "human:qa", "{tagged}");
    assert_eq!(
        tagged["relayed_by"],
        json!({"actor": AGENT, "run": RUN}),
        "{tagged}"
    );
    round
}

/// Stops the daemon and opens the project to read its records.
fn with_rdy<T>(s: &Served, f: impl FnOnce(&Rdy) -> T) -> T {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&r);
    r.close().unwrap();
    out
}

#[test]
fn di14_a_round_goes_from_chat_to_canon_with_no_cli_act() {
    let (s, spec) = seeded();
    let round = chat_round(&s, &spec);
    // the log shows the landing as the person's, relayed by the agent, on a card
    let (ok, log) = s.json(&["--json", "log"]);
    assert!(ok, "{log}");
    let row = &log[0];
    assert_eq!(row["approved"], round.pref.as_str(), "{log}");
    assert_eq!(row["approved_by"], "human:qa", "{log}");
    assert_eq!(row["assurance"], "relayed", "{log}");
    assert_eq!(row["relayed_by"]["actor"], AGENT, "{log}");
    assert_eq!(row["evidence"]["kind"], "card", "{log}");
    assert_eq!(row["tags"], json!(["v0.1.0"]), "{log}");
    // the store is whole
    let (ok, fsck) = s.json(&["--json", "fsck"]);
    assert!(ok, "{fsck}");
    // the batch left the queue, and canon holds the decisions
    with_rdy(&s, |r| {
        let versions = r.read().versions().unwrap();
        assert_eq!(versions[0].tagged_by, "human:qa");
        assert!(r.read().open_proposals().unwrap().is_empty());
    });
}

#[test]
fn di14_a_one_word_yes_never_approves_the_exception() {
    let (s, spec) = seeded();
    let mut c = agent(&s);
    let round = capture_and_propose(&mut c, &spec);
    let queue = c.call("rdy_pending", json!({"session_ref": CHAT})).ok();
    let card = item_of(&queue, &round.pref);
    let ex = item_of(&queue, &round.exception);
    // one "yes" over the batch alone: refused, and the exception's card comes back (O21)
    let all = c.call(
        "rdy_answer",
        words(json!({
            "evidence": "Yes.",
            "all_recommended": [{"ref": round.pref, "card_hash": card["card_hash"]}],
        })),
    );
    let Reply::ToolError(e) = all else {
        panic!("expected a refusal, got {all:?}");
    };
    assert_eq!(e["code"], "E_EXCEPTION_OPEN", "{e}");
    assert_eq!(
        e["corrected"]["exceptions"],
        json!([{"ref": ex["ref"], "card_hash": ex["card_hash"], "card": ex["card"]}]),
        "{e}"
    );
    // so does the same word on the batch card alone
    let one = relay(&mut c, &queue, &round.pref, "approve");
    assert_eq!(one.code(), Some("E_EXCEPTION_OPEN"), "{one:?}");
    // nothing landed, and the queue still holds both cards
    let queue = c.call("rdy_pending", json!({"session_ref": CHAT})).ok();
    assert_eq!(items(&queue).len(), 2, "{queue}");
    let (ok, log) = s.json(&["--json", "log"]);
    assert!(ok, "{log}");
    assert_eq!(
        log.as_array().unwrap().len(),
        1,
        "only the spec landed: {log}"
    );
}

/// The `.html` pages one level under `url` (a site's index and the pages it links), as
/// `(path, text)`, with `questions.html` always among them.
fn site_pages(url: &str) -> Vec<(String, String)> {
    let base = url.to_string();
    let get = |path: &str| {
        let got = http::get(&format!("{base}{path}"));
        assert_eq!(got.status, 200, "{base}{path}");
        String::from_utf8_lossy(&got.body).into_owned()
    };
    let index = get("");
    let mut paths: Vec<String> = vec!["questions.html".into()];
    for part in index.split("href=\"").skip(1) {
        let href = part.split('"').next().unwrap_or("");
        if href.ends_with(".html") && !href.contains("://") && !href.starts_with('/') {
            let href = href.trim_start_matches("./").to_string();
            if !href.starts_with("..") && !paths.contains(&href) {
                paths.push(href);
            }
        }
    }
    let mut pages = vec![(String::new(), index)];
    for p in paths.into_iter().take(40) {
        let text = get(&p);
        pages.push((p, text));
    }
    pages
}

/// The refs in `refs` that `page` holds outside its own scope identity. A proposal-scoped page
/// carries the proposal's ref in one place only: the scope in its live-update script's
/// `data-events` URL, which is page chrome and names no item to act on. Every other occurrence,
/// in a title, in content, in a question, is a ref on the site.
fn refs_outside_identity<'a>(page: &str, scope: &str, refs: &'a [String]) -> Vec<&'a str> {
    // the one allowed spot, as the renderer writes it: `/_rdy/events?scope=<scope>&amp;`
    let identity = format!("/_rdy/events?scope={scope}&amp;");
    let rest = if scope.is_empty() {
        page.to_string()
    } else {
        page.replace(&identity, "/_rdy/events?")
    };
    refs.iter()
        .filter(|r| rest.contains(r.as_str()))
        .map(String::as_str)
        .collect()
}

#[test]
fn di14_the_ref_check_allows_the_scope_identity_and_nothing_else() {
    let pref = "P-0123456789ab@1".to_string();
    let refs = vec![pref.clone(), "D-ABCDE".to_string()];
    let scope = format!("proposal:{pref}");
    let chrome = format!(
        "<script src=\"/_rdy/live.js\" data-events=\"/_rdy/events?scope={scope}&amp;page=index.html&amp;rev=1&amp;label=x\" defer></script>"
    );
    let clean =
        format!("<html><head>{chrome}<title>Spec</title></head><body><p>text</p></body></html>");
    assert!(refs_outside_identity(&clean, &scope, &refs).is_empty());
    // the batch ref anywhere else on the page is caught: a title, content, a question
    for (what, extra) in [
        ("title", format!("<title>{pref}</title>")),
        ("content", format!("<p>see {pref}</p>")),
        ("question", format!("<li class=\"q\">Answer {pref}?</li>")),
    ] {
        let page = format!("{clean}{extra}");
        assert_eq!(
            refs_outside_identity(&page, &scope, &refs),
            vec![pref.as_str()],
            "a ref in a {what} is not allowed"
        );
    }
    // another item's ref is caught even inside the identity script's page
    let other = format!("{clean}<p>D-ABCDE</p>");
    assert_eq!(
        refs_outside_identity(&other, &scope, &refs),
        vec!["D-ABCDE"]
    );
    // canon scope: no identity to allow
    assert_eq!(
        refs_outside_identity("<p>text</p>", "", &refs),
        Vec::<&str>::new(),
        "a canon page without a ref is clean"
    );
    let canon = format!("<p>{pref}</p>");
    assert_eq!(
        refs_outside_identity(&canon, "", &refs),
        vec![pref.as_str()]
    );
}

#[test]
fn di14_the_site_shows_no_call_to_action() {
    let (s, spec) = seeded_with_question();
    let mut c = agent(&s);
    let round = capture_and_propose(&mut c, &spec);
    let queue = c.call("rdy_pending", json!({"session_ref": CHAT})).ok();
    // what a page must not hold: each item's ref, each option's label, each card's text
    let mut refs = vec![];
    let mut labels = vec![];
    let mut cards = vec![];
    for i in items(&queue) {
        refs.push(i["ref"].as_str().unwrap().to_string());
        cards.push(i["card"].as_str().unwrap().to_string());
        for o in i["options"].as_array().unwrap() {
            labels.push(o["label"].as_str().unwrap().to_string());
        }
    }
    assert!(refs.contains(&round.pref) && refs.contains(&round.exception));
    assert!(
        items(&queue).iter().any(|i| i["kind"] == "answer"),
        "the open question is a card in the queue: {queue}"
    );
    assert!(!labels.is_empty());
    let mut checked = 0;
    for scope in [
        json!({}),
        json!({"scope": format!("proposal:{}", round.pref)}),
    ] {
        let link = c.call("rdy_site", scope.clone()).ok();
        let url = link["url"].as_str().unwrap_or_else(|| panic!("{link}"));
        for (path, page) in site_pages(url) {
            let at = format!("{scope} {path}");
            for card in &cards {
                assert!(!page.contains(card.as_str()), "card text on {at}");
            }
            for label in &labels {
                assert!(!page.contains(label.as_str()), "label {label:?} on {at}");
            }
            // no item's ref, the proposal's own included: its page holds its ref only in the
            // scope of its live-update script, which is page chrome
            let scope_id = scope["scope"].as_str().unwrap_or("");
            assert_eq!(
                refs_outside_identity(&page, scope_id, &refs),
                Vec::<&str>::new(),
                "an item ref on {at}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 4, "pages of canon and the proposal were read");
    // the open-questions page is plain text: no form, no button, no answer control
    let link = c.call("rdy_site", json!({})).ok();
    let url = link["url"].as_str().unwrap();
    let questions =
        String::from_utf8_lossy(&http::get(&format!("{url}questions.html")).body).into_owned();
    assert!(
        questions.contains(PROMPT),
        "the open question is listed as plain text: {questions}"
    );
    for control in ["<form", "<select", "<textarea", "onclick"] {
        assert!(!questions.contains(control), "{control} on questions.html");
    }
    // the only input is the page's search box
    for part in questions.split("<input").skip(1) {
        assert!(
            part.trim_start()
                .starts_with("id=\"search\" type=\"search\""),
            "an input on questions.html is not the search box: {part:.80}"
        );
    }
    // the only buttons are the page's own chrome: the theme choice, the menu, the copy tools and
    // the review bar's toggles. A button that answers would carry another word.
    const CHROME: [&str; 9] = [
        "Auto",
        "Light",
        "Dark",
        "Menu",
        "Link",
        "Copy ref",
        "Copy",
        "Show changes",
        "Show diff",
    ];
    for part in questions.split("<button").skip(1) {
        let text = part
            .split_once('>')
            .and_then(|(_, rest)| rest.split_once("</button>"))
            .map(|(t, _)| t)
            .unwrap_or_else(|| panic!("a button without an end: {part:.80}"));
        assert!(
            CHROME.contains(&text),
            "button {text:?} on questions.html is not page chrome"
        );
    }
    assert!(
        !questions.to_lowercase().contains("answer this")
            && !questions.contains("rdy answer")
            && !questions.contains("rdy_answer"),
        "questions.html is plain text"
    );
}

#[test]
fn di14_every_relay_names_the_person_and_the_agent() {
    let (s, spec) = seeded();
    let round = chat_round(&s, &spec);
    // the landing, from the log
    let (ok, log) = s.json(&["--json", "log"]);
    assert!(ok, "{log}");
    let row = &log[0];
    assert_eq!(row["approved_by"], "human:qa", "{log}");
    assert_eq!(
        row["relayed_by"],
        json!({"actor": AGENT, "run": RUN}),
        "{log}"
    );
    assert_eq!(row["relay_evidence"]["quoted"], "Yes, do that.", "{log}");
    assert_eq!(row["relay_evidence"]["session_ref"], CHAT, "{log}");
    // the answers, from the store
    with_rdy(&s, |r| {
        let st = r.read();
        let check = |item: &str, act: AnswerAct, quoted: &str, kind: EvidenceKind| {
            let (_, a) = st
                .reader()
                .answer_of(item)
                .unwrap()
                .unwrap_or_else(|| panic!("an answer to {item}"));
            assert_eq!(a.act, act, "{item}");
            assert_eq!(
                a.person.actor, "human:qa",
                "{item}: the person is the actor"
            );
            let by = a.relayed_by.as_ref().unwrap_or_else(|| panic!("{item}"));
            assert_eq!(
                (by.actor.as_str(), by.run.as_deref()),
                (AGENT, Some(RUN)),
                "{item}"
            );
            let ev = a.relay_evidence.as_ref().expect("the person's words");
            assert_eq!(
                (ev.quoted.as_str(), ev.session_ref.as_str()),
                (quoted, CHAT),
                "{item}"
            );
            assert_eq!(a.evidence_kind, Some(kind), "{item}");
        };
        // the confirm answer: the card's words, and the agent's relay
        check(
            &round.exception,
            AnswerAct::Confirm,
            "Yes, do that.",
            EvidenceKind::Card,
        );
        // the tag's answer: a TAG record and an ANSWER record that holds the relay
        let tag = st.versions().unwrap();
        assert_eq!(tag[0].tagged_by, "human:qa");
        check(
            "tag:v0.1.0",
            AnswerAct::Tag,
            "Tag it v0.1.0.",
            EvidenceKind::Text,
        );
    });
}
