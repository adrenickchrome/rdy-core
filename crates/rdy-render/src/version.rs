//! The site's wording of where a page stands among the versions (rule 2.11). The engine's own
//! label (`VersionPosition.label`, "v1.2.0 + 3 landings") is storage wording and stays with the
//! CLI and MCP.
use crate::escape;
use rdy_engine::VersionPosition;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Label {
    pub text: String,
    pub frozen: bool,
}

fn count(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What a review render shows: a view or a proposal version, and the blocks it changes
/// (D-BMX8P: "draft" is only the lifecycle state, so the site never says it here).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reviewed {
    pub kind: ReviewedKind,
    /// The proposal's title or the view's name.
    pub name: String,
    /// The actor text from the store: a view's owner or a proposal's `provenance.actor`.
    pub author: Option<String>,
    /// The count of changed blocks.
    pub blocks: u32,
}

/// Whether the review render shows a proposal or a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ReviewedKind {
    Proposal,
    View,
}

/// One chip beside the pill. The hover goes only into `title` and `aria-label` (D-ZPAVR).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Chip {
    pub text: String,
    pub hover: String,
}

/// The header's words (D-42YMF): the pill, the neutral landings chip and the accent review chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Header {
    pub pill: String,
    pub frozen: bool,
    pub since: Option<Chip>,
    pub review: Option<Chip>,
}

fn kind_word(kind: ReviewedKind) -> &'static str {
    match kind {
        ReviewedKind::Proposal => "proposal",
        ReviewedKind::View => "view",
    }
}

/// The text of the colophon and the llms files (O10): `unversioned`, `v1.2.0`,
/// `v1.2.0, 3 changes since`, then, for a review render, ` · view: {name}, {n} changes, not
/// landed` (or `proposal:`), or `, no changes yet` with no changed block. A frozen scope reads
/// the same and says `frozen` through the flag.
pub fn text_label(p: &VersionPosition, reviewed: Option<&Reviewed>) -> Label {
    let mut text = match &p.version {
        None => "unversioned".to_string(),
        Some(v) if p.landings_since == 0 => v.name.clone(),
        Some(v) => format!(
            "{}, {} since",
            v.name,
            count(p.landings_since, "change", "changes")
        ),
    };
    if let Some(r) = reviewed {
        let tail = if r.blocks == 0 {
            "no changes yet".to_string()
        } else {
            format!("{}, not landed", count(r.blocks, "change", "changes"))
        };
        text.push_str(&format!(" · {}: {}, {tail}", kind_word(r.kind), r.name));
    }
    Label {
        text,
        frozen: p.frozen,
    }
}

/// The header's pill and chips. The pill shows only the tag. The landings chip needs a tag and
/// at least one landing since it (F3). The review chip needs a review render with a changed block.
pub fn header(p: &VersionPosition, reviewed: Option<&Reviewed>) -> Header {
    let since = match &p.version {
        Some(v) if p.landings_since > 0 => Some(Chip {
            text: format!("+{}", p.landings_since),
            hover: format!(
                "{} since {}",
                count(p.landings_since, "landing", "landings"),
                v.name
            ),
        }),
        _ => None,
    };
    let review = reviewed.filter(|r| r.blocks > 0).map(|r| Chip {
        text: format!("+{}", r.blocks),
        hover: format!("{}, not landed", count(r.blocks, "change", "changes")),
    });
    Header {
        pill: p
            .version
            .as_ref()
            .map_or_else(|| "unversioned".to_string(), |v| v.name.clone()),
        frozen: p.frozen,
        since,
        review,
    }
}

/// Where the header's pill and chips lead (RULING F4). A chip whose link is none is a `<span>`.
/// `menu` marks the pill as the live site's version menu button.
pub struct HeaderLinks<'a> {
    pub versions: &'a str,
    pub since: Option<&'a str>,
    pub changes: Option<&'a str>,
    pub menu: bool,
}

/// The lock a frozen scope's pill shows before its text.
const LOCK: &str = r#"<svg class="lock" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="7" width="10" height="7" rx="1.5"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/></svg>"#;

/// One chip: a link when it has one, else a `<span>`; the hover only in `title` and
/// `aria-label` (D-ZPAVR).
fn chip_html(chip: &Chip, class: &str, href: Option<&str>) -> String {
    let (text, hover) = (escape(&chip.text), escape(&chip.hover));
    match href {
        Some(href) => format!(
            "<a class=\"{class}\" href=\"{}\" title=\"{hover}\" aria-label=\"{hover}\">{text}</a>",
            escape(href)
        ),
        None => format!(
            "<span class=\"{class}\" title=\"{hover}\" aria-label=\"{hover}\">{text}</span>"
        ),
    }
}

/// The header's markup (D-42YMF): the pill, then the landings chip, then the review chip, one
/// space between them. Every value is escaped.
pub fn header_html(h: &Header, links: &HeaderLinks) -> String {
    let mut out = format!("<a class=\"pill\" href=\"{}\"", escape(links.versions));
    if links.menu {
        out.push_str(" data-menu aria-haspopup=\"dialog\" aria-expanded=\"false\"");
    }
    out.push('>');
    if h.frozen {
        out.push_str(LOCK);
    }
    out.push_str(&format!(
        "<span class=\"pill-text\">{}</span>",
        escape(&h.pill)
    ));
    if h.frozen {
        out.push_str(" <span class=\"snapshot\">snapshot</span>");
    }
    out.push_str("</a>");
    if let Some(c) = &h.since {
        out.push(' ');
        out.push_str(&chip_html(c, "chip chip-since", links.since));
    }
    if let Some(c) = &h.review {
        out.push(' ');
        out.push_str(&chip_html(c, "chip chip-review", links.changes));
    }
    out
}

/// The review bar's words (D-BMX8P): `Reviewing view · {name}` or `Reviewing proposal · {title}`,
/// then ` · by {author}` when the store names one.
// Tasks 7 and 8 call this; until then only the tests do.
#[allow(dead_code)]
pub fn review_bar(r: &Reviewed) -> String {
    let mut text = format!("Reviewing {} · {}", kind_word(r.kind), r.name);
    if let Some(a) = &r.author {
        text.push_str(&format!(" · by {}", author_words(a)));
    }
    text
}

/// The heading of the change summary and of the home page's list (O9).
// Tasks 7 and 8 call this; until then only the tests do.
#[allow(dead_code)]
pub fn changes_heading(kind: ReviewedKind) -> &'static str {
    match kind {
        ReviewedKind::View => "Changes in this view",
        ReviewedKind::Proposal => "Changes in this proposal",
    }
}

/// An author as people read it (F1): `human:nick` is `nick`, `agent:claude` is `claude (agent)`,
/// any other text stays as it is, and every `_` shows as a space.
// Tasks 7 and 8 call this; until then only the tests do.
#[allow(dead_code)]
pub fn author_words(actor: &str) -> String {
    let words = if let Some(n) = actor.strip_prefix("human:") {
        n.to_string()
    } else if let Some(n) = actor.strip_prefix("agent:") {
        format!("{n} (agent)")
    } else {
        actor.to_string()
    };
    words.replace('_', " ")
}

#[cfg(test)]
mod sr3 {
    use super::*;
    use rdy_engine::{VersionRef, ViewPosition};

    fn pos(version: Option<&str>, since: u32, frozen: bool) -> VersionPosition {
        VersionPosition {
            version: version.map(|n| VersionRef {
                name: n.into(),
                landing: "0".repeat(64),
            }),
            landings_since: since,
            view: None::<ViewPosition>,
            frozen,
            label: "storage wording".into(),
        }
    }

    fn reviewed(kind: ReviewedKind, name: &str, author: Option<&str>, blocks: u32) -> Reviewed {
        Reviewed {
            kind,
            name: name.into(),
            author: author.map(String::from),
            blocks,
        }
    }

    #[test]
    fn sr3_the_pill_shows_only_the_version() {
        let r = reviewed(ReviewedKind::View, "work", None, 4);
        assert_eq!(
            header(&pos(Some("v1.2.0"), 3, false), Some(&r)).pill,
            "v1.2.0"
        );
        assert_eq!(header(&pos(None, 3, false), None).pill, "unversioned");
    }

    #[test]
    fn sr3_the_since_chip_counts_landings_since_the_tag() {
        let h = header(&pos(Some("v0.4.0"), 14, false), None);
        assert_eq!(
            h.since,
            Some(Chip {
                text: "+14".into(),
                hover: "14 landings since v0.4.0".into()
            })
        );
        let h = header(&pos(Some("v0.4.0"), 1, false), None);
        assert_eq!(h.since.unwrap().hover, "1 landing since v0.4.0");
    }

    #[test]
    fn sr3_the_review_chip_counts_changed_blocks() {
        let r = reviewed(
            ReviewedKind::Proposal,
            "Guest mode",
            Some("agent:claude"),
            10,
        );
        let h = header(&pos(Some("v0.4.0"), 0, false), Some(&r));
        assert_eq!(
            h.review,
            Some(Chip {
                text: "+10".into(),
                hover: "10 changes, not landed".into()
            })
        );
        let one = reviewed(ReviewedKind::View, "work", None, 1);
        assert_eq!(
            header(&pos(None, 0, false), Some(&one))
                .review
                .unwrap()
                .hover,
            "1 change, not landed"
        );
    }

    #[test]
    fn sr3_no_chip_at_zero_and_none_when_unversioned() {
        let empty = reviewed(ReviewedKind::View, "work", None, 0);
        let h = header(&pos(Some("v1.0.0"), 0, false), Some(&empty));
        assert_eq!((h.since, h.review), (None, None));
        assert_eq!(
            header(&pos(None, 7, false), None).since,
            None,
            "F3: no tag, no since chip"
        );
        let frozen = header(&pos(Some("v1.0.0"), 0, true), None);
        assert!(frozen.frozen && frozen.since.is_none());
    }

    #[test]
    fn sr3_the_label_text_for_the_colophon_and_llms() {
        assert_eq!(text_label(&pos(None, 0, false), None).text, "unversioned");
        assert_eq!(
            text_label(&pos(Some("v1.2.0"), 3, false), None).text,
            "v1.2.0, 3 changes since"
        );
        let p = reviewed(ReviewedKind::Proposal, "Guest mode", None, 3);
        assert_eq!(
            text_label(&pos(Some("v1.2.0"), 0, false), Some(&p)).text,
            "v1.2.0 · proposal: Guest mode, 3 changes, not landed"
        );
        let v = reviewed(ReviewedKind::View, "work", None, 1);
        assert_eq!(
            text_label(&pos(Some("v1.2.0"), 3, false), Some(&v)).text,
            "v1.2.0, 3 changes since · view: work, 1 change, not landed"
        );
        let none = reviewed(ReviewedKind::View, "work", None, 0);
        assert_eq!(
            text_label(&pos(None, 0, false), Some(&none)).text,
            "unversioned · view: work, no changes yet"
        );
    }

    #[test]
    fn sr3_the_review_bar_names_the_view_or_proposal_and_its_author() {
        let p = reviewed(
            ReviewedKind::Proposal,
            "Reviewing on the live site",
            Some("human:nick"),
            2,
        );
        assert_eq!(
            review_bar(&p),
            "Reviewing proposal · Reviewing on the live site · by nick"
        );
        let v = reviewed(
            ReviewedKind::View,
            "editing",
            Some("agent:design-thread"),
            2,
        );
        assert_eq!(
            review_bar(&v),
            "Reviewing view · editing · by design-thread (agent)"
        );
        let lone = reviewed(ReviewedKind::View, "editing", None, 2);
        assert_eq!(review_bar(&lone), "Reviewing view · editing");
        assert_eq!(changes_heading(ReviewedKind::View), "Changes in this view");
        assert_eq!(
            changes_heading(ReviewedKind::Proposal),
            "Changes in this proposal"
        );
    }

    #[test]
    fn sr3_no_review_word_says_draft() {
        // D-BMX8P: "draft" is only the lifecycle state
        for kind in [ReviewedKind::View, ReviewedKind::Proposal] {
            let r = reviewed(kind, "work", Some("human:nick"), 2);
            let h = header(&pos(Some("v1.0.0"), 2, false), Some(&r));
            let words = [
                text_label(&pos(Some("v1.0.0"), 2, false), Some(&r)).text,
                review_bar(&r),
                changes_heading(kind).to_string(),
                h.review.unwrap().hover,
            ];
            assert!(
                words.iter().all(|w| !w.to_lowercase().contains("draft")),
                "{words:?}"
            );
        }
    }

    #[test]
    fn sr3_author_words() {
        assert_eq!(author_words("human:nick"), "nick");
        assert_eq!(author_words("agent:claude"), "claude (agent)");
        assert_eq!(author_words("agent:design_thread"), "design thread (agent)");
        assert_eq!(author_words("ci-bot"), "ci-bot");
    }
}
