//! sinst1: the server instructions say to read in full before editing and to supersede only the
//! part a change retires; rdy_supersede's description names part.
//! agi5: the server text is short; the guidance it once held is loaded from the built-in rulesets.
use rdy_qa::fixtures::project;
use rdy_qa::instructions::World;
use rdy_qa::mcp::McpClient;
use serde_json::json;

/// The server text, one element per entry (agi5, D-GEKKY): the description, then six elements.
pub(crate) const SHORT: [&str; 7] = [
    "rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents.",
    "State rdy/actor and rdy/run in _meta, or call rdy_session.",
    "Call rdy_instructions at the start of every task.",
    "Call it before rdy_overview, and load the rulesets your task needs.",
    "Read and write through rdy's tools, not the working-copy files.",
    "Never approve, tag or answer on your own judgment. Relay the person's exact words.",
    "If rdy_instructions refuses, follow its fix and call again before you work.",
];

/// The short server text as the host sees it: the elements joined with one space.
pub(crate) fn short_text() -> String {
    SHORT.join(" ")
}

/// Each removed server element, with the built-in ruleset that now holds it and the delivered
/// sentence (identifiers in code spans). The old server string is the sentence without backticks.
/// Elements 5 and 23 held several sentences; each has its own entry.
pub(crate) const MOVED: &[(&str, &str)] = &[
    (
        "write",
        r##"Brief before editing (`rdy_brief`); writes to stable content must cite a briefing (`brief: <token>`)."##,
    ),
    (
        "write",
        r##"Before editing a block, read it with format markdown or yaml: compact lines are a summary."##,
    ),
    (
        "write",
        r##"Work in your own view; include others' changes, never write into their views."##,
    ),
    (
        "write",
        r##"When a block comes back as a consequence a second time in a session, do not rewrite it again: confirm it (repin) or ask the person, and show what remains as a change plan."##,
    ),
    (
        "write",
        r##"When you start a project or a new area, plan the tree first: folders by area, a doc type for each doc, decisions recorded with `rdy_decide` and no parent, open questions as question blocks and terms in the glossary; show the person the plan before the first content write."##,
    ),
    (
        "write",
        r##"When the person states something that should hold, record it at once with `rdy_decide` as a proposed decision, in their words, and say so in one line."##,
    ),
    (
        "write",
        r##"A decision is worth recording when every agent should keep to it next month; a passing preference is not."##,
    ),
    (
        "write",
        r##"Record a decision with `rdy_decide` and no parent: it goes to the decision home, or to the project root when the project names none; link content to it with `decided_by`, and never put a decision in a content doc or create a doc to hold decisions."##,
    ),
    (
        "write",
        r##"When a change retires only some of a decision's rules, supersede that part (`rdy_supersede` with `part`), not the whole decision."##,
    ),
    (
        "write",
        r##"While coding, a change within the design is just code; a gap is a proposed decision plus the doc update; a contradiction is a finding plus a superseding decision, and you stop when it is blocking or `one_way`."##,
    ),
    (
        "write",
        r##"Search a thing's name, key or alias with `rdy_search` to get its fact sheet."##,
    ),
    (
        "write",
        r##"Write responses carry suggestions (links to the entities your text names, new entities for literals that recur): accept the right ones with one `rdy_accept` call; they never block."##,
    ),
    (
        "write",
        r##"Fact sheets, search hits and briefings carry a confidence level: act on high; on medium or low, widen the search, fix the coverage issues you can, or ask the person before you act."##,
    ),
    (
        "write",
        r##"Link every entity a normative statement names, and elsewhere the entities a block is about; dismiss a passing mention of an everyday word with `rdy_resolve` (mention, dismiss)."##,
    ),
    (
        "after-write",
        r##"Conflicts are recorded, not errors: resolve one by writing the block."##,
    ),
    (
        "after-write",
        r##"Write with `rdy_write`; propose with `rdy_propose`. Before proposing, read `rdy_plan` and close or defer each item. Give every deferral a reason. Re-affirm a suspect decision that still holds with a `reaffirm` op, and supersede one that does not."##,
    ),
    (
        "after-write",
        r##"Tensions: `rdy_tensions` lists disagreements between changes and with canon; resolve one with a change, or dismiss it with `rdy_resolve` and a reason; a blocking one refuses a landing that has a side in it and would put every side into canon."##,
    ),
    (
        "approvals",
        r##"At session start, also call `rdy_pending` and tell the person in one line what waits; do not list it unless asked."##,
    ),
    (
        "approvals",
        r##"Propose at the point `rdy_session`'s `propose.cadence` sets, by default at a natural break; under changes, propose when a write reply says `propose_due`."##,
    ),
    (
        "approvals",
        r##"Present a batch card from `rdy_pending` at the end of an iteration, then each exception on its own; show each option label word for word and never change the options, the recommendation or the version."##,
    ),
    (
        "approvals",
        r##"Relay the person's answer with `rdy_answer`: pass the `card_hash` you showed, their words as evidence and your question; interrupt the person only for a blocking item."##,
    ),
    (
        "approvals",
        r##"Use `rdy_answer` with `elicit: true` to ask a supporting client to show the card. Never send a request to `/~ui/`. Use `rdy_answer`, with elicitation or the person's relayed words."##,
    ),
    (
        "approvals",
        r##"When `rdy_propose` reports policy accepts, land with `rdy_land`; else show the card to the person."##,
    ),
    (
        "read",
        r##"Read `rdy_inbox` at session start and when `rdy://inbox` changes, and ack what you have handled; notifications are best effort, the inbox is reliable."##,
    ),
    (
        "read",
        r##"Why: to learn why a block says what it says, call `rdy_trace`."##,
    ),
    (
        "as-built",
        r##"Finding: record what surfaced with `rdy_write`, type finding, statement and 1 to 20 evidence entries; cite it from a decision with `depends_on`."##,
    ),
    (
        "parallel",
        r##"Before a larger change, post an intent (`rdy_intent`) on its area or blocks; write responses name other sessions' intents on what you wrote, and nothing is ever locked."##,
    ),
    (
        "project-ops",
        r##"Before proposing content removal, call `rdy_redact` with `dry_run` true. Show its report and warnings. Record the request, then use `rdy_propose` and `rdy_pending`. Only a person can approve it."##,
    ),
    (
        "review",
        r##"Run an authoring check only when the author or a person asks. Show its verdict on the current approval card before relaying a person's answer."##,
    ),
];

/// The text of the built-in rulesets `sets`, loaded through a real `rdy_instructions` call with
/// `load: ["rdy:<name>", ...]`. It claims a reader session when the connection has none.
pub(crate) fn loaded(c: &mut McpClient, sets: &[&str]) -> String {
    let load: Vec<String> = sets.iter().map(|s| format!("rdy:{s}")).collect();
    let mut r = c.call("rdy_instructions", json!({"load": load}));
    if r.code() == Some("E_NO_SESSION") {
        c.call(
            "rdy_session",
            json!({"actor": "agent:guide", "role": "reader"}),
        )
        .ok();
        r = c.call("rdy_instructions", json!({"load": load}));
    }
    let v = r.ok();
    v["instructions"]
        .as_array()
        .expect("instructions")
        .iter()
        .filter(|i| i["layer"] == "rdy")
        .map(|i| i["text"].as_str().expect("row text"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of the built-in rulesets `sets`, from a fresh project and server.
pub(crate) fn guide(sets: &[&str]) -> String {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    loaded(&mut c, sets)
}

/// Asserts that the loaded text of `set` holds `sentence`, and the server text does not.
fn guide_has(set: &str, sentence: &str) {
    let g = guide(&[set]);
    assert!(g.contains(sentence), "rdy:{set} lacks: {sentence}");
    assert!(
        !rdy_mcp::server::INSTRUCTIONS.contains(sentence),
        "the server text still holds: {sentence}"
    );
}

fn moved(set: &str, starts: &str) -> &'static str {
    MOVED
        .iter()
        .find(|(s, t)| *s == set && t.starts_with(starts))
        .unwrap_or_else(|| panic!("no moved element {set}: {starts}"))
        .1
}

#[test]
fn agi5_initialize_keeps_session_and_instruction_order() {
    let want = short_text();
    let (dir, r) = project();
    drop(r);
    // the stdio server
    let c = McpClient::start(dir.path());
    assert_eq!(c.initialize_result()["result"]["instructions"], want);
    drop(c);
    // the proxy to the daemon
    let w = World::new();
    let pd = w.project();
    let c = w.mcp(pd.path());
    let got = c.initialize_result()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(got, want);
    // the server constants agree, and the claim comes before the instructions call, which comes
    // before the overview
    assert_eq!(rdy_mcp::server::INSTRUCTION_SENTENCES, SHORT);
    assert_eq!(rdy_mcp::server::INSTRUCTIONS, want);
    let at = |needle: &str| {
        got.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {got}"))
    };
    assert!(at("rdy_session") < at("Call rdy_instructions at the start"));
    assert!(at("Call rdy_instructions at the start") < at("rdy_overview"));
}

#[test]
fn agi5_removed_server_guidance_is_loadable() {
    // all 29 moved entries: from a real instructions call, in the ruleset the mapping names
    assert_eq!(MOVED.len(), 29);
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let names = [
        "write",
        "after-write",
        "approvals",
        "read",
        "as-built",
        "parallel",
        "project-ops",
        "review",
    ];
    for name in names {
        let text = loaded(&mut c, &[name]);
        let mut seen = 0;
        for (set, sentence) in MOVED.iter().filter(|(s, _)| *s == name) {
            assert!(text.contains(sentence), "rdy:{set} lacks: {sentence}");
            seen += 1;
        }
        assert!(seen > 0, "{name}");
    }
    // none of them stays in the server text, with or without its code spans
    let server = rdy_mcp::server::INSTRUCTIONS;
    for (_, sentence) in MOVED {
        let old = sentence.replace('`', "");
        assert!(!server.contains(sentence), "{sentence}");
        assert!(!server.contains(&old), "{old}");
    }
}

#[test]
fn sinst1_instructions_name_reading_before_editing_and_partial_supersession() {
    guide_has(
        "write",
        "Before editing a block, read it with format markdown or yaml: compact lines are a summary.",
    );
    guide_has(
        "write",
        "When a change retires only some of a decision's rules, supersede that part (`rdy_supersede` with `part`), not the whole decision.",
    );
    assert!(
        guide(&["write"]).contains("Brief before editing"),
        "guard: the earlier lines stay"
    );
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let d = tools.iter().find(|t| t["name"] == "rdy_supersede").unwrap()["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        d.contains("pass part (the words of the rules it retires) and the rest stays in force"),
        "{d}"
    );
}

#[test]
fn cc18_the_instructions_carry_confidence_and_linking() {
    let g = guide(&["write"]);
    let conf = moved("write", "Fact sheets, search hits");
    let link = moved("write", "Link every entity");
    assert!(conf.contains("carry a confidence level: act on high"));
    let (a, b) = (g.find(conf), g.find(link));
    assert!(a.is_some() && b.is_some(), "{g}");
    assert!(a < b, "the confidence line comes before the linking line");
}

#[test]
fn ib7_the_instructions_tell_an_agent_to_read_its_inbox() {
    guide_has(
        "read",
        "Read `rdy_inbox` at session start and when `rdy://inbox` changes, and ack what you have handled; notifications are best effort, the inbox is reliable.",
    );
}

#[test]
fn di13_the_instructions_check_the_queue_at_session_start() {
    guide_has(
        "approvals",
        moved("approvals", "At session start, also call"),
    );
}

#[test]
fn di13_the_instructions_capture_without_nagging() {
    guide_has("write", moved("write", "When the person states something"));
    guide_has("write", moved("write", "A decision is worth recording"));
}

#[test]
fn di13_the_instructions_explain_the_cadence() {
    guide_has("approvals", moved("approvals", "Propose at the point"));
}

#[test]
fn di13_the_instructions_present_the_batch_and_relay_with_rdy_answer() {
    let g = guide(&["approvals"]);
    let batch = moved("approvals", "Present a batch card");
    let relay = moved("approvals", "Relay the person's answer");
    assert!(g.contains(batch), "{g}");
    assert!(g.contains(relay), "{g}");
    // ui10: the elicitation sentence follows the relay sentence
    let elicit = moved("approvals", "Use `rdy_answer` with `elicit: true`");
    assert!(g.find(relay).unwrap() < g.find(elicit).unwrap(), "{g}");
    assert!(!rdy_mcp::server::INSTRUCTIONS.contains("Present a batch card"));
}

#[test]
fn di13_the_instructions_carry_the_adjustment_lines() {
    guide_has("write", moved("write", "While coding"));
    guide_has("as-built", moved("as-built", "Finding:"));
}

#[test]
fn di13_the_instructions_plan_structure_first() {
    guide_has("write", moved("write", "When you start a project"));
    // F1 (D-X3136): where a decision with no parent goes, and never a doc to hold decisions
    guide_has("write", moved("write", "Record a decision with"));
}

#[test]
fn ain6_server_says_to_load_instructions_at_every_task() {
    let s = rdy_mcp::server::INSTRUCTION_SENTENCES;
    assert_eq!(
        s[0],
        "rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents."
    );
    assert_eq!(
        s[1],
        "State rdy/actor and rdy/run in _meta, or call rdy_session."
    );
    assert_eq!(s[2], "Call rdy_instructions at the start of every task.");
    assert_eq!(
        s[3],
        "Call it before rdy_overview, and load the rulesets your task needs."
    );
}
