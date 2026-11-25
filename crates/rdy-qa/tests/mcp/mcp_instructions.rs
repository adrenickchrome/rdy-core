//! sinst1: the write ruleset carries reading in full before editing and superseding only the part
//! a change retires; rdy_supersede's description names part.
//! agi5: the server text is short; the guidance it once held is loaded from the built-in rulesets.
//! The guide's words are rdy-design's: these tests compare what rdy serves with the bundle
//! (`release/agent-guide.json`, `rdy_qa::guide_bundle`) and hold no guide sentence.
use rdy_qa::fixtures::project;
use rdy_qa::guide_bundle;
use rdy_qa::instructions::World;
use rdy_qa::mcp::McpClient;
use std::collections::BTreeSet;

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
    // the guidance that left the server text is served from the built-in rulesets, each as the
    // bundle holds it (the words are rdy-design's, so this file holds none of them)
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let names = guide_bundle::rulesets();
    assert!(!names.is_empty(), "the bundle has no ruleset");
    let all: Vec<&str> = names.iter().map(String::as_str).collect();
    guide_bundle::assert_served(&mut c, &all);
    // every rule id of the bundle is served, and no other built-in row
    let got: BTreeSet<String> = guide_bundle::served(&mut c, &all)
        .into_iter()
        .map(|r| r.id)
        .collect();
    let want: BTreeSet<String> = names
        .iter()
        .flat_map(|n| guide_bundle::rules(n))
        .map(|r| r.id)
        .collect();
    assert_eq!(got, want);
    // the server text holds the short text and nothing else
    assert_eq!(rdy_mcp::server::INSTRUCTIONS, short_text());
}

#[test]
fn sinst1_instructions_name_reading_before_editing_and_partial_supersession() {
    // the reading and partial-supersession rules are in the write ruleset as bundled
    guide_bundle::assert_served_fresh(&["write"]);
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
    // the confidence and linking rules are in the write ruleset, in the bundle's order
    guide_bundle::assert_served_fresh(&["write"]);
}

#[test]
fn ib7_the_instructions_tell_an_agent_to_read_its_inbox() {
    guide_bundle::assert_served_fresh(&["read"]);
}

#[test]
fn di13_the_instructions_check_the_queue_at_session_start() {
    guide_bundle::assert_served_fresh(&["approvals"]);
}

#[test]
fn di13_the_instructions_capture_without_nagging() {
    guide_bundle::assert_served_fresh(&["write"]);
}

#[test]
fn di13_the_instructions_explain_the_cadence() {
    guide_bundle::assert_served_fresh(&["approvals"]);
}

#[test]
fn di13_the_instructions_present_the_batch_and_relay_with_rdy_answer() {
    guide_bundle::assert_served_fresh(&["approvals"]);
    assert!(!rdy_mcp::server::INSTRUCTIONS.contains("batch card"));
}

#[test]
fn di13_the_instructions_carry_the_adjustment_lines() {
    guide_bundle::assert_served_fresh(&["write", "as-built"]);
}

#[test]
fn di13_the_instructions_plan_structure_first() {
    guide_bundle::assert_served_fresh(&["write"]);
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
