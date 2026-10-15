//! The card: an item as text (cd-W4XVS for one item, cd-2J1WR for a batch).
//!
//! # Layout
//!
//! Sections stand in this order, one blank line between them. A section with no content is left
//! out.
//!
//! 1. **First line.** The ref, two spaces, the title, and the item kind at the right edge.
//! 2. **Lead.** For an exception, `Beside <batch>, on its own: <words>.` Then, when the item has
//!    no summary, its `why` line. The summary does not repeat `why`.
//! 3. **Summary.** `Summary (<author>)` and the author's words, wrapped. The summary leads the
//!    card and the computed facts stand beside it (D-4PG9Y). Nothing computed can replace it.
//! 4. **In this batch.** One line for each decision of the batch (`members`).
//! 5. **Facts.** `Facts (computed)` and one named line for each fact, in the order the item holds
//!    them. The queue fills them in the closed order of the plan: Changes, Decisions, Supersedes,
//!    Reversibility, Consequences, Tensions, Coverage, Confidence, Base, Exceptions.
//! 6. **Options.** `Options`, then each option numbered, its label, `recommended` at the right
//!    of the recommended one, and what choosing it does. The reason of a recommendation stands
//!    below it unless it is the summary, which the card already shows.
//!
//! The card never shows `raised_by`, `raised_at`, `age`, `blocking`, `card` or `card_hash`.
//!
//! # The cut rule
//!
//! `max_tokens` counts the card's UTF-8 bytes / 4, as briefings do (`brief.rs`). A card over the
//! budget loses lines from the bottom of Facts first, one at a time. When no fact is left it loses
//! the decision lines of the batch from the bottom. The ref line, the lead, the summary, the
//! options and the recommendation are never cut, so a card can stay over a very small budget.
//! A cut card says so: `(<n> facts cut; rdy_pending with a larger max_tokens shows them)`, and
//! the same for decisions.
//!
//! # The hash
//!
//! [`card_hash`] hashes the card as [`render`] gives it with [`CARD_MAX_TOKENS`]. It does not
//! depend on the budget an agent passed.

use super::item::Item;
use rdy_model::Hash;

/// The largest budget `rdy_pending` accepts, and the budget `card_hash` hashes at.
pub const CARD_MAX_TOKENS: usize = 20_000;
/// The budget a card gets when the caller passes none.
pub const CARD_DEFAULT_TOKENS: usize = 2_000;

/// The width of the first line, the item kind at its right edge.
const HEADER_WIDTH: usize = 69;
/// The width a summary line wraps at, indent included.
const SUMMARY_WIDTH: usize = 70;
/// The width an option's text wraps at, indent included.
const OPTION_WIDTH: usize = 64;
/// The column the option labels pad to, so `recommended` lines up.
const LABEL_WIDTH: usize = 47;
/// The width of a fact name, so the values line up.
const NAME_WIDTH: usize = 16;

/// The card of `item`, cut to `max_tokens` by the rule in the module docs.
pub fn render(item: &Item, max_tokens: usize) -> String {
    let mut facts = item.facts.len();
    let mut members = item.members.len();
    let mut card = build(item, facts, members);
    if fits(&card, max_tokens) {
        return card;
    }
    while facts > 0 {
        facts -= 1;
        card = build(item, facts, members);
        if fits(&card, max_tokens) {
            return card;
        }
    }
    while members > 0 {
        members -= 1;
        card = build(item, facts, members);
        if fits(&card, max_tokens) {
            return card;
        }
    }
    card
}

/// The hash of the card at [`CARD_MAX_TOKENS`].
pub fn card_hash(item: &Item) -> Hash {
    Hash::of(render(item, CARD_MAX_TOKENS).as_bytes())
}

fn fits(card: &str, max_tokens: usize) -> bool {
    card.len() / 4 <= max_tokens
}

/// The card with the first `keep_facts` facts and the first `keep_members` decision lines.
fn build(item: &Item, keep_facts: usize, keep_members: usize) -> String {
    let mut sections: Vec<Vec<String>> = vec![vec![header(item)]];
    let lead = lead(item);
    if !lead.is_empty() {
        sections.push(lead);
    }
    if let Some(summary) = item.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        let mut lines = vec![match item.summary_by.as_deref() {
            Some(by) => format!("Summary ({by})"),
            None => "Summary".to_string(),
        }];
        lines.extend(wrap(summary, "  ", SUMMARY_WIDTH));
        sections.push(lines);
    }
    if !item.members.is_empty() {
        let mut lines = vec!["In this batch".to_string()];
        lines.extend(
            item.members
                .iter()
                .take(keep_members)
                .map(|m| format!("  {}", m.trim().replace('\n', " "))),
        );
        if keep_members < item.members.len() {
            lines.push(cut_line(item.members.len() - keep_members, "decisions"));
        }
        sections.push(lines);
    }
    if !item.facts.is_empty() {
        let mut lines = vec!["Facts (computed)".to_string()];
        for f in item.facts.iter().take(keep_facts) {
            lines.push(format!("  {:<NAME_WIDTH$}{}", f.name, one_line(&f.value)));
        }
        if keep_facts < item.facts.len() {
            lines.push(cut_line(item.facts.len() - keep_facts, "facts"));
        }
        sections.push(lines);
    }
    if !item.options.is_empty() {
        sections.push(options(item));
    }
    sections
        .into_iter()
        .map(|s| s.join("\n"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The first line: `<ref>  <title>`, then the kind at the right edge (two spaces apart at least).
fn header(item: &Item) -> String {
    let left = format!("{}  {}", one_line(&item.r#ref), one_line(&item.title));
    let kind = item.kind.word();
    let used = left.chars().count() + kind.len();
    let gap = HEADER_WIDTH.saturating_sub(used).max(2);
    format!("{left}{}{kind}", " ".repeat(gap))
}

/// What the card says before the summary: where an exception stands, and `why` when no summary
/// says it.
fn lead(item: &Item) -> Vec<String> {
    let mut lines = Vec::new();
    match (item.batch.as_deref(), item.exception) {
        (Some(batch), Some(kind)) => {
            lines.push(format!(
                "Beside {batch}, on its own: {}.",
                exception_words(kind)
            ));
        }
        (None, Some(kind)) => lines.push(format!("On its own: {}.", exception_words(kind))),
        (Some(batch), None) => lines.push(format!("Beside {batch}.")),
        (None, None) => {}
    }
    let has_summary = item
        .summary
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    if !has_summary && !item.why.trim().is_empty() {
        lines.extend(wrap(&item.why, "", SUMMARY_WIDTH));
    }
    lines
}

/// The plain words of an exception kind. A kind outside the closed list shows as it is.
fn exception_words(kind: &str) -> String {
    match kind {
        "against" => "it goes against an earlier decision".to_string(),
        "one_way" => "it is one_way".to_string(),
        "unapplied" => "no content applies it".to_string(),
        "not_recommended" => "it is not recommended".to_string(),
        "blocking_tension" => "a blocking tension involves it".to_string(),
        other => other.to_string(),
    }
}

fn options(item: &Item) -> Vec<String> {
    let summary = item.summary.as_deref().map(str::trim);
    let mut lines = vec!["Options".to_string()];
    for (i, o) in item.options.iter().enumerate() {
        let label = one_line(&o.label);
        let prefix = format!("  {}. ", i + 1);
        match &o.recommended {
            Some(_) => {
                let pad = LABEL_WIDTH.saturating_sub(label.chars().count()).max(2);
                lines.push(format!("{prefix}{label}{}recommended", " ".repeat(pad)));
            }
            None => lines.push(format!("{prefix}{label}")),
        }
        for c in &o.consequences {
            lines.extend(wrap(c, "     ", OPTION_WIDTH));
        }
        if let Some(reason) = o.recommended.as_deref() {
            let reason = reason.trim();
            if !reason.is_empty() && Some(reason) != summary {
                lines.extend(wrap(
                    &format!("Why recommended: {reason}"),
                    "     ",
                    OPTION_WIDTH,
                ));
            }
        }
    }
    lines
}

fn cut_line(n: usize, what: &str) -> String {
    format!("  ({n} {what} cut; rdy_pending with a larger max_tokens shows them)")
}

/// `text` with every run of white space as one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` as lines of at most `width` characters, indent included. A paragraph break stays as an
/// empty line. A word longer than the line stands alone.
fn wrap(text: &str, indent: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.trim().split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if line.is_empty() {
                line = format!("{indent}{word}");
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(std::mem::take(&mut line));
                line = format!("{indent}{word}");
            }
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::item::{Fact, ItemKind, ItemOption, age};
    use super::*;

    const SUMMARY: &str = "Lets a visitor read a shared doc without an account. Adds one decision on access and updates Getting started and Sharing.";

    fn option(id: &str, label: &str, consequences: &[&str], rec: Option<&str>) -> ItemOption {
        ItemOption {
            id: id.to_string(),
            label: label.to_string(),
            consequences: consequences.iter().map(|c| c.to_string()).collect(),
            recommended: rec.map(str::to_string),
        }
    }

    fn fact(name: &'static str, value: &str) -> Fact {
        Fact {
            name,
            value: value.to_string(),
        }
    }

    fn item(kind: ItemKind, r#ref: &str, title: &str) -> Item {
        Item {
            r#ref: r#ref.to_string(),
            kind,
            title: title.to_string(),
            why: String::new(),
            blocking: false,
            summary: None,
            summary_by: None,
            facts: Vec::new(),
            members: Vec::new(),
            options: Vec::new(),
            raised_by: "agent:planner".to_string(),
            raised_at: "2026-10-12T03:04:20Z".to_string(),
            age: "today".to_string(),
            batch: None,
            exception: None,
            card: String::new(),
            card_hash: String::new(),
        }
    }

    /// The approve item of cd-W4XVS, with the facts of the plan's closed list (O25) and the
    /// wording of C7.
    fn guest_mode() -> Item {
        let mut i = item(ItemKind::Approve, "P-7f3a@2", "Guest mode");
        i.why = "ready to land: 3 changes".to_string();
        i.summary = Some(SUMMARY.to_string());
        i.summary_by = Some("agent:planner".to_string());
        i.facts = vec![
            fact("Changes", "2 docs, 3 blocks"),
            fact(
                "Decisions",
                "1 new: D-0931 \"Guest readers need no account\"",
            ),
            fact("Supersedes", "none"),
            fact("Reversibility", "two_way, declared by the proposer"),
            fact("Consequences", "4 returned"),
            fact(
                "Tensions",
                "0 blocking, 1 advisory, answers not recorded yet",
            ),
            fact("Coverage", "81% \u{2192} 84%"),
            fact("Confidence", "high"),
            fact("Base", "canon at L-2210, current"),
            fact("Exceptions", "none"),
        ];
        i.options = vec![
            option(
                "approve",
                "Approve P-7f3a@2 and confirm D-0931",
                &["Lands the proposal in canon."],
                Some(SUMMARY),
            ),
            option(
                "reject",
                "Reject",
                &["Returns the proposal to its view. Canon does not change."],
                None,
            ),
            option(
                "later",
                "Later",
                &["Hides this card until the chat session changes."],
                None,
            ),
        ];
        i
    }

    fn batch() -> Item {
        let mut i = guest_mode();
        i.members = vec![
            "D-0931  Guest readers need no account".to_string(),
            "D-0932  Link holders may tick items, not edit them".to_string(),
            "D-0933  Guest links expire after 30 days".to_string(),
        ];
        i
    }

    fn exception() -> Item {
        let mut i = item(
            ItemKind::Confirm,
            "D-0934@3c1f9a",
            "Sync needs an email address",
        );
        i.why = "Goes against your decision \"No accounts\". Not recommended without a look."
            .to_string();
        i.batch = Some("P-7f3a@2".to_string());
        i.exception = Some("against");
        i.options = vec![
            option(
                "confirm",
                "Confirm",
                &["Lets the batch land with this decision."],
                None,
            ),
            option(
                "reject",
                "Reject",
                &["The batch waits until its author drops or revises this decision."],
                Some("it goes against D-0210"),
            ),
            option(
                "later",
                "Later",
                &["Hides this card until the chat session changes."],
                None,
            ),
        ];
        i
    }

    const GUEST_MODE_CARD: &str = "\
P-7f3a@2  Guest mode                                          approve

Summary (agent:planner)
  Lets a visitor read a shared doc without an account. Adds one
  decision on access and updates Getting started and Sharing.

Facts (computed)
  Changes         2 docs, 3 blocks
  Decisions       1 new: D-0931 \"Guest readers need no account\"
  Supersedes      none
  Reversibility   two_way, declared by the proposer
  Consequences    4 returned
  Tensions        0 blocking, 1 advisory, answers not recorded yet
  Coverage        81% \u{2192} 84%
  Confidence      high
  Base            canon at L-2210, current
  Exceptions      none

Options
  1. Approve P-7f3a@2 and confirm D-0931            recommended
     Lands the proposal in canon.
  2. Reject
     Returns the proposal to its view. Canon does not change.
  3. Later
     Hides this card until the chat session changes.";

    #[test]
    fn di4_an_approve_card_matches_the_documented_layout() {
        assert_eq!(render(&guest_mode(), CARD_DEFAULT_TOKENS), GUEST_MODE_CARD);
    }

    #[test]
    fn di4_a_batch_lists_its_decisions_and_names_its_exceptions() {
        let card = render(&batch(), CARD_DEFAULT_TOKENS);
        let want = "\
In this batch
  D-0931  Guest readers need no account
  D-0932  Link holders may tick items, not edit them
  D-0933  Guest links expire after 30 days";
        assert!(card.contains(want), "{card}");
        let ex = render(&exception(), CARD_DEFAULT_TOKENS);
        let want = "\
D-0934@3c1f9a  Sync needs an email address                    confirm

Beside P-7f3a@2, on its own: it goes against an earlier decision.
Goes against your decision \"No accounts\". Not recommended without a
look.

Options
  1. Confirm
     Lets the batch land with this decision.
  2. Reject                                         recommended
     The batch waits until its author drops or revises this
     decision.
     Why recommended: it goes against D-0210
  3. Later
     Hides this card until the chat session changes.";
        assert_eq!(ex, want);
    }

    #[test]
    fn di4_the_summary_comes_first_and_facts_beside_it() {
        let card = render(&batch(), CARD_DEFAULT_TOKENS);
        let at = |needle: &str| {
            card.find(needle)
                .unwrap_or_else(|| panic!("{needle}\n{card}"))
        };
        assert!(card.starts_with("P-7f3a@2  Guest mode"));
        assert!(at("P-7f3a@2  Guest mode") < at("Summary (agent:planner)"));
        assert!(at("Summary (agent:planner)") < at("In this batch"));
        assert!(at("Summary (agent:planner)") < at("Facts (computed)"));
        assert!(at("Facts (computed)") < at("Options"));
        // The author's words stand as written, not replaced by a computed line.
        assert!(card.contains("Lets a visitor read a shared doc without an account."));
        // With no summary, `why` stands where it would be.
        let mut no_summary = guest_mode();
        no_summary.summary = None;
        let card = render(&no_summary, CARD_DEFAULT_TOKENS);
        assert!(!card.contains("Summary"), "{card}");
        assert!(card.contains("ready to land: 3 changes"), "{card}");
        assert!(!render(&guest_mode(), CARD_DEFAULT_TOKENS).contains("ready to land"));
    }

    #[test]
    fn di4_a_small_budget_cuts_facts_first_and_says_so() {
        let full = render(&batch(), CARD_MAX_TOKENS);
        let budget = full.len() / 4 - 20;
        let card = render(&batch(), budget);
        assert!(card.len() / 4 <= budget, "{card}");
        assert!(
            card.contains("  Changes         2 docs, 3 blocks"),
            "{card}"
        );
        assert!(!card.contains("Exceptions      none"), "{card}");
        let line = card
            .lines()
            .find(|l| l.contains("facts cut"))
            .expect("a cut line");
        assert!(line.ends_with("facts cut; rdy_pending with a larger max_tokens shows them)"));
        // The facts that stay are the first ones; the cut ones are the bottom ones.
        let cut: usize = line
            .trim()
            .trim_start_matches('(')
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let shown = card
            .lines()
            .filter(|l| {
                l.starts_with("  ")
                    && l.contains("  ")
                    && [
                        "Changes",
                        "Decisions",
                        "Supersedes",
                        "Reversibility",
                        "Consequences",
                        "Tensions",
                        "Coverage",
                        "Confidence",
                        "Base",
                        "Exceptions",
                    ]
                    .iter()
                    .any(|n| l.trim_start().starts_with(n))
            })
            .count();
        assert_eq!(shown + cut, 10, "{card}");
        // The decision lines stay while facts are enough to cut.
        assert!(
            card.contains("  D-0933  Guest links expire after 30 days"),
            "{card}"
        );
        assert!(!card.contains("decisions cut"), "{card}");
    }

    #[test]
    fn di4_the_options_and_recommendation_are_never_cut() {
        for max in [0, 1, 50] {
            let card = render(&batch(), max);
            assert!(card.starts_with("P-7f3a@2  Guest mode"), "{card}");
            assert!(card.contains("Summary (agent:planner)"), "{card}");
            assert!(
                card.contains("  1. Approve P-7f3a@2 and confirm D-0931            recommended"),
                "{card}"
            );
            assert!(card.contains("  2. Reject"), "{card}");
            assert!(card.contains("  3. Later"), "{card}");
            assert!(card.contains("Lands the proposal in canon."), "{card}");
            assert!(
                card.contains("(10 facts cut; rdy_pending with a larger max_tokens shows them)"),
                "{card}"
            );
            assert!(
                card.contains("(3 decisions cut; rdy_pending with a larger max_tokens shows them)"),
                "{card}"
            );
        }
        // An exception keeps its place and its reason.
        let card = render(&exception(), 0);
        assert!(card.contains("Beside P-7f3a@2, on its own"), "{card}");
        assert!(
            card.contains("Why recommended: it goes against D-0210"),
            "{card}"
        );
    }

    #[test]
    fn di4_the_card_hash_ignores_the_budget() {
        let i = batch();
        let h = card_hash(&i);
        assert_eq!(h, Hash::of(render(&i, CARD_MAX_TOKENS).as_bytes()));
        // A small budget changes the card the agent sees, never the hash it binds to.
        assert_ne!(render(&i, 10), render(&i, CARD_MAX_TOKENS));
        assert_eq!(h, card_hash(&i));
        // The stored `card` and `card_hash` fields are not inputs.
        let mut stored = i.clone();
        stored.card = render(&i, 10);
        stored.card_hash = "anything".to_string();
        assert_eq!(card_hash(&stored), h);
        // Any shown word is.
        let mut changed = i.clone();
        changed.facts[0].value = "3 docs, 3 blocks".to_string();
        assert_ne!(card_hash(&changed), h);
    }

    #[test]
    fn di4_the_same_item_gives_the_same_card() {
        let a = render(&batch(), CARD_DEFAULT_TOKENS);
        let b = render(&batch().clone(), CARD_DEFAULT_TOKENS);
        assert_eq!(a, b);
        assert_eq!(card_hash(&batch()), card_hash(&batch()));
    }

    #[test]
    fn di4_age_reads_in_plain_words() {
        const DAY: i64 = 86_400_000;
        let now = 100 * DAY;
        assert_eq!(age(now, now), "today");
        assert_eq!(age(now - DAY + 1, now), "today");
        assert_eq!(age(now - DAY, now), "1 day");
        assert_eq!(age(now - 2 * DAY, now), "2 days");
        assert_eq!(age(now - 13 * DAY, now), "13 days");
        assert_eq!(age(now - 14 * DAY, now), "2 weeks");
        assert_eq!(age(now - 29 * DAY, now), "4 weeks");
        // A time ahead of now is today.
        assert_eq!(age(now + 5 * DAY, now), "today");
    }
}
