//! What the server tells a host about rdy.

/// The server instructions, one sentence per element, in reading order. A plan that adds guidance
/// adds an element where it belongs. `INSTRUCTIONS` joins them with one space at compile time.
pub const INSTRUCTION_SENTENCES: &[&str] = &[
    "rdy keeps decisions and documentation consistent while agents change them.",
    "Call rdy_session first, then rdy_overview.",
    "Read and write through rdy's tools, not the working-copy files.",
    "Brief before editing (rdy_brief); writes to stable content must cite a briefing (brief: <token>).",
    // plan Redaction: four sentences in this element, and plan Change plan adds three before them,
    // so that the existing sentence guards (their count and their positions) hold
    "Write with rdy_write; propose with rdy_propose. Before proposing, read rdy_plan and close or defer each item. Give every deferral a reason. Re-affirm a suspect decision that still holds with a reaffirm op, and supersede one that does not. Before proposing content removal, call rdy_redact with dry_run true. Show its report and warnings. Record the request, then use rdy_propose and rdy_pending. Only a person can approve it.",
    "Work in your own view; include others' changes, never write into their views.",
    "Conflicts are recorded, not errors: resolve one by writing the block.",
    "When a block comes back as a consequence a second time in a session, do not rewrite it again: confirm it (repin) or ask the person, and show what remains as a change plan.",
    "Before editing a block, read it with format markdown or yaml: compact lines are a summary.",
    "When a change retires only some of a decision's rules, supersede that part (rdy_supersede with part), not the whole decision.",
    "Before a larger change, post an intent (rdy_intent) on its area or blocks; write responses name other sessions' intents on what you wrote, and nothing is ever locked.",
    "Tensions: rdy_tensions lists disagreements between changes and with canon; resolve one with a change, or dismiss it with rdy_resolve and a reason; a blocking one refuses a landing that has a side in it and would put every side into canon.",
    "Read rdy_inbox at session start and when rdy://inbox changes, and ack what you have handled; notifications are best effort, the inbox is reliable.",
    "At session start, also call rdy_pending and tell the person in one line what waits; do not list it unless asked.",
    "When the person states something that should hold, record it at once with rdy_decide as a proposed decision, in their words, and say so in one line.",
    "A decision is worth recording when every agent should keep to it next month; a passing preference is not.",
    "Propose at the point rdy_session's propose.cadence sets, by default at a natural break; under changes, propose when a write reply says propose_due.",
    "Present a batch card from rdy_pending at the end of an iteration, then each exception on its own; show each option label word for word and never change the options, the recommendation or the version.",
    "Relay the person's answer with rdy_answer: pass the card_hash you showed, their words as evidence and your question; interrupt the person only for a blocking item.",
    "While coding, a change within the design is just code; a gap is a proposed decision plus the doc update; a contradiction is a finding plus a superseding decision, and you stop when it is blocking or one_way.",
    "When you start a project or a new area, plan the tree first: folders by area, a doc type for each doc, decisions recorded with rdy_decide and no parent, open questions as question blocks and terms in the glossary; show the person the plan before the first content write.",
    // rf5 (TASK-507 (4)): the Finding line sits in this element, so that the existing sentence
    // guards (their count and their positions) hold
    "Record a decision with rdy_decide and no parent: it goes to the decision home, or to the project root when the project names none; link content to it with decided_by, and never put a decision in a content doc or create a doc to hold decisions. Finding: record what surfaced with rdy_write, type finding, statement and 1 to 20 evidence entries; cite it from a decision with depends_on. Why: to learn why a block says what it says, call rdy_trace.",
    "Search a thing's name, key or alias with rdy_search to get its fact sheet.",
    "Write responses carry suggestions (links to the entities your text names, new entities for literals that recur): accept the right ones with one rdy_accept call; they never block.",
    "Fact sheets, search hits and briefings carry a confidence level: act on high; on medium or low, widen the search, fix the coverage issues you can, or ask the person before you act.",
    "Link every entity a normative statement names, and elsewhere the entities a block is about; dismiss a passing mention of an everyday word with rdy_resolve (mention, dismiss).",
    // O27 (M3c): the agent's landing act
    "When rdy_propose reports policy accepts, land with rdy_land; else show the card to the person.",
];

/// What the server tells a host about rdy: [`INSTRUCTION_SENTENCES`] joined with one space.
pub const INSTRUCTIONS: &str = match core::str::from_utf8(&JOINED) {
    Ok(s) => s,
    Err(_) => panic!("the instruction sentences are not UTF-8"),
};

const fn joined_len(parts: &[&str]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < parts.len() {
        n += parts[i].len();
        if i > 0 {
            n += 1;
        }
        i += 1;
    }
    n
}

const fn join_bytes<const N: usize>(parts: &[&str]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut at = 0;
    let mut i = 0;
    while i < parts.len() {
        if i > 0 {
            out[at] = b' ';
            at += 1;
        }
        let b = parts[i].as_bytes();
        let mut j = 0;
        while j < b.len() {
            out[at] = b[j];
            at += 1;
            j += 1;
        }
        i += 1;
    }
    out
}

const JOINED_LEN: usize = joined_len(INSTRUCTION_SENTENCES);
const JOINED: [u8; JOINED_LEN] = join_bytes::<JOINED_LEN>(INSTRUCTION_SENTENCES);

#[cfg(test)]
mod tests {
    use super::{INSTRUCTION_SENTENCES, INSTRUCTIONS};

    #[test]
    fn isen1_the_sentences_join_to_the_instructions() {
        assert_eq!(INSTRUCTIONS, INSTRUCTION_SENTENCES.join(" "));
    }

    #[test]
    fn ap11_the_instructions_name_rdy_land() {
        // O27: one element at the end
        let s = "When rdy_propose reports policy accepts, land with rdy_land; else show the card to the person.";
        assert!(INSTRUCTIONS.contains(s), "{INSTRUCTIONS}");
        assert_eq!(INSTRUCTION_SENTENCES.last(), Some(&s));
    }

    #[test]
    fn isen1_each_sentence_is_trimmed_and_ends_a_sentence() {
        for s in INSTRUCTION_SENTENCES {
            assert!(!s.is_empty() && s.trim() == *s, "{s:?}");
            assert!(s.ends_with('.'), "{s:?}");
        }
    }
}
