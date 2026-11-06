//! What the server tells a host about rdy.

/// The server instructions, one element per entry, in reading order. They stay short: the rules
/// for each kind of work live in the built-in rulesets, which `rdy_instructions` delivers. Do not
/// add guidance here; add it to the guide. `INSTRUCTIONS` joins the elements with one space at
/// compile time.
pub const INSTRUCTION_SENTENCES: &[&str] = &[
    "rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents.",
    "State rdy/actor and rdy/run in _meta, or call rdy_session.",
    "Call rdy_instructions at the start of every task.",
    "Call it before rdy_overview, and load the rulesets your task needs.",
    "Read and write through rdy's tools, not the working-copy files.",
    "Never approve, tag or answer on your own judgment. Relay the person's exact words.",
    "If rdy_instructions refuses, follow its fix and call again before you work.",
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
        // O27: the sentence now lives in the approvals ruleset, not in the server text
        let s = "When `rdy_propose` reports policy accepts, land with `rdy_land`; else show the card to the person.";
        let approvals = rdy_engine::embedded_guide::RULESETS
            .iter()
            .find(|r| r.name == "approvals")
            .expect("the approvals ruleset");
        assert!(
            approvals.rows.iter().any(|r| r.text.contains(s)),
            "the approvals ruleset lacks the rdy_land sentence"
        );
        assert!(!INSTRUCTIONS.contains("rdy_land"), "{INSTRUCTIONS}");
    }

    #[test]
    fn isen1_each_sentence_is_trimmed_and_ends_a_sentence() {
        for s in INSTRUCTION_SENTENCES {
            assert!(!s.is_empty() && s.trim() == *s, "{s:?}");
            assert!(s.ends_with('.'), "{s:?}");
        }
    }
}
