//! sinst1: the server instructions say to read in full before editing and to supersede only the
//! part a change retires; rdy_supersede's description names part.
use rdy_qa::fixtures::project;
use rdy_qa::mcp::McpClient;

#[test]
fn sinst1_instructions_name_reading_before_editing_and_partial_supersession() {
    let i = rdy_mcp::server::INSTRUCTIONS;
    assert!(
        i.contains("Before editing a block, read it with format markdown or yaml: compact lines are a summary."),
        "{i}"
    );
    assert!(
        i.contains("When a change retires only some of a decision's rules, supersede that part (rdy_supersede with part), not the whole decision."),
        "{i}"
    );
    assert!(
        i.contains("Brief before editing"),
        "guard: the earlier lines stay: {i}"
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
    let s = rdy_mcp::server::INSTRUCTION_SENTENCES;
    // ap11: O27's rdy_land sentence follows them, and ck8's authoring-check sentence is last
    let n = s.len() - 2;
    assert_eq!(
        s[n - 2],
        "Fact sheets, search hits and briefings carry a confidence level: act on high; on medium or low, widen the search, fix the coverage issues you can, or ask the person before you act."
    );
    assert_eq!(
        s[n - 1],
        "Link every entity a normative statement names, and elsewhere the entities a block is about; dismiss a passing mention of an everyday word with rdy_resolve (mention, dismiss)."
    );
}

#[test]
fn ib7_the_instructions_tell_an_agent_to_read_its_inbox() {
    let i = rdy_mcp::server::INSTRUCTIONS;
    assert!(
        i.contains("Read rdy_inbox at session start and when rdy://inbox changes, and ack what you have handled; notifications are best effort, the inbox is reliable."),
        "{i}"
    );
}

const DI13_SENTENCES: [&str; 8] = [
    "At session start, also call rdy_pending and tell the person in one line what waits; do not list it unless asked.",
    "When the person states something that should hold, record it at once with rdy_decide as a proposed decision, in their words, and say so in one line.",
    "A decision is worth recording when every agent should keep to it next month; a passing preference is not.",
    "Propose at the point rdy_session's propose.cadence sets, by default at a natural break; under changes, propose when a write reply says propose_due.",
    "Present a batch card from rdy_pending at the end of an iteration, then each exception on its own; show each option label word for word and never change the options, the recommendation or the version.",
    "Relay the person's answer with rdy_answer: pass the card_hash you showed, their words as evidence and your question; interrupt the person only for a blocking item.",
    "While coding, a change within the design is just code; a gap is a proposed decision plus the doc update; a contradiction is a finding plus a superseding decision, and you stop when it is blocking or one_way.",
    "When you start a project or a new area, plan the tree first: folders by area, a doc type for each doc, decisions recorded with rdy_decide and no parent, open questions as question blocks and terms in the glossary; show the person the plan before the first content write.",
];

fn di13_has(sentence: &str) {
    let s = rdy_mcp::server::INSTRUCTION_SENTENCES;
    assert!(s.contains(&sentence), "missing: {sentence}");
    assert!(rdy_mcp::server::INSTRUCTIONS.contains(sentence));
}

#[test]
fn di13_the_instructions_check_the_queue_at_session_start() {
    di13_has(DI13_SENTENCES[0]);
}

#[test]
fn di13_the_instructions_capture_without_nagging() {
    di13_has(DI13_SENTENCES[1]);
    di13_has(DI13_SENTENCES[2]);
}

#[test]
fn di13_the_instructions_explain_the_cadence() {
    di13_has(DI13_SENTENCES[3]);
}

#[test]
fn di13_the_instructions_present_the_batch_and_relay_with_rdy_answer() {
    di13_has(DI13_SENTENCES[4]);
    di13_has(DI13_SENTENCES[5]);
}

#[test]
fn di13_the_instructions_carry_the_adjustment_lines() {
    di13_has(DI13_SENTENCES[6]);
}

#[test]
fn di13_the_instructions_plan_structure_first() {
    di13_has(DI13_SENTENCES[7]);
}

#[test]
fn di13_the_new_sentences_sit_after_the_inbox_line() {
    let s = rdy_mcp::server::INSTRUCTION_SENTENCES;
    // ap11: O27 adds the rdy_land sentence; ck8 adds the authoring-check sentence after it
    assert_eq!(s.len(), 28);
    assert!(s[12].starts_with("Read rdy_inbox at session start"));
    for (k, want) in DI13_SENTENCES.iter().enumerate() {
        assert_eq!(s[13 + k], *want, "index {}", 13 + k);
    }
    assert!(s[21].starts_with("Record a decision with rdy_decide and no parent"));
    // F1 (D-X3136): where a decision with no parent goes, and never a doc to hold decisions
    assert!(rdy_mcp::server::INSTRUCTIONS.contains(
        "Record a decision with rdy_decide and no parent: it goes to the decision home, or to the project root when the project names none; link content to it with decided_by, and never put a decision in a content doc or create a doc to hold decisions."
    ));
}
