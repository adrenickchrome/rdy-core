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
    /// `text` is the whole colophon sentence, its period included (a live profile's label,
    /// [`profile_label`]): the colophon and the llms files write it as it is, with no site name
    /// or period around it.
    pub complete_colophon: bool,
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
/// `v1.2.0, 3 updates since` (3 landings, F7), then, for a review render, ` · view: {name}, {n} changes, not
/// landed` (or `proposal:`), or `, no changes yet` with no changed block. A frozen scope reads
/// the same and says `frozen` through the flag.
pub fn text_label(p: &VersionPosition, reviewed: Option<&Reviewed>) -> Label {
    let mut text = match &p.version {
        None => "unversioned".to_string(),
        Some(v) if p.landings_since == 0 => v.name.clone(),
        // "updates" is the word of landings only (F7); a review says "changes"
        Some(v) => format!(
            "{}, {} since",
            v.name,
            count(p.landings_since, "update", "updates")
        ),
    };
    if let Some(r) = reviewed {
        text.push_str(&format!(" · {}", review_part(r)));
    }
    Label {
        text,
        frozen: p.frozen,
        complete_colophon: false,
    }
}

/// A review render's part of the label: `view: {name}, {n} changes, not landed` (or
/// `proposal:`), `1 change` at one, or `{kind}: {name}, no changes yet` with no changed block.
fn review_part(r: &Reviewed) -> String {
    let tail = if r.blocks == 0 {
        "no changes yet".to_string()
    } else {
        format!("{}, not landed", count(r.blocks, "change", "changes"))
    };
    format!("{}: {}, {tail}", kind_word(r.kind), r.name)
}

/// A live profile's whole colophon sentence (D-CWC91, p-750KT): `{title}.` for canon, a view, a
/// proposal version and the profile-only site; `A snapshot of {title} at {tag}.` for a chosen
/// tag; then, for a review, ` · ` and the review part (`{title}. · view: work, 2 changes, not
/// landed`). The llms files carry the same words.
pub fn profile_colophon(title: &str, tag: Option<&str>, reviewed: Option<&Reviewed>) -> String {
    let mut text = match tag {
        Some(tag) => format!("A snapshot of {title} at {tag}."),
        None => format!("{title}."),
    };
    if let Some(r) = reviewed {
        text.push_str(&format!(" · {}", review_part(r)));
    }
    text
}

/// A live profile's label: its whole colophon sentence ([`profile_colophon`]), written as it is.
pub fn profile_label(colophon: &str, frozen: bool) -> Label {
    Label {
        text: colophon.to_string(),
        frozen,
        complete_colophon: true,
    }
}

/// The header's pill and chips. The pill shows only the tag. The landings chip needs a tag and
/// at least one landing since it (F3). The review chip needs a review render with a changed block.
pub fn header(p: &VersionPosition, reviewed: Option<&Reviewed>) -> Header {
    let since = match &p.version {
        Some(v) if p.landings_since > 0 => Some(since_chip(p.landings_since, &v.name)),
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

/// The neutral landings chip (D-42YMF): `+{n}`, hovering `{n} landings since {tag}` (`1 landing`
/// at one).
pub fn since_chip(n: u32, tag: &str) -> Chip {
    Chip {
        text: format!("+{n}"),
        hover: format!("{} since {tag}", count(n, "landing", "landings")),
    }
}

/// A live profile's header words (D-CWC91, p-FTDH3): the pill as [`header`] writes it, then
/// ` · {title}` (`unversioned · User guide`, `v1.2.0 · User guide`); the neutral chip counts the
/// `since` landings that changed what the profile shows, since the tag it names; the accent chip
/// counts the selected blocks of a review.
pub fn profile_header(
    p: &VersionPosition,
    title: &str,
    since: Option<(u32, &str)>,
    reviewed: Option<&Reviewed>,
) -> Header {
    let mut h = header(p, reviewed);
    h.pill = format!("{} · {title}", h.pill);
    h.since = since
        .filter(|(n, _)| *n > 0)
        .map(|(n, tag)| since_chip(n, tag));
    h
}

/// A named profile's label (RULING F3): the explicitly chosen tag's name, or empty for canon and
/// a bare landing. It never falls back to `unversioned` and never counts landings.
pub fn chosen_label(tag: Option<&str>, frozen: bool) -> Label {
    Label {
        text: tag.unwrap_or_default().to_string(),
        frozen,
        complete_colophon: false,
    }
}

/// A named profile's header words (RULING F3): the chosen tag's name as the pill, or an empty
/// pill for canon and a bare landing; no landings chip and no review chip.
pub fn chosen_header(tag: Option<&str>, frozen: bool) -> Header {
    Header {
        pill: tag.unwrap_or_default().to_string(),
        frozen,
        since: None,
        review: None,
    }
}

/// A named profile's header markup (RULING F3): the chosen tag's pill as a `<span>`, with no
/// link, menu, snapshot suffix or count, then the `redacted` badge when the tag's history holds
/// removed content. Empty when no tag was chosen: the page then writes no version wrapper.
pub fn chosen_header_html(h: &Header, redacted: bool) -> String {
    if h.pill.is_empty() {
        return String::new();
    }
    let mut out = String::from("<span class=\"pill\">");
    if h.frozen {
        out.push_str(LOCK);
    }
    out.push_str(&format!(
        "<span class=\"pill-text\">{}</span></span>",
        escape(&h.pill)
    ));
    if redacted {
        out.push_str(" <span class=\"badge badge-redacted\">redacted</span>");
    }
    out
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
pub fn review_bar(r: &Reviewed) -> String {
    let mut text = format!("Reviewing {} · {}", kind_word(r.kind), r.name);
    if let Some(a) = &r.author {
        text.push_str(&format!(" · by {}", author_words(a)));
    }
    text
}

/// The heading of the change summary and of the home page's list (O9).
pub fn changes_heading(kind: ReviewedKind) -> &'static str {
    match kind {
        ReviewedKind::View => "Changes in this view",
        ReviewedKind::Proposal => "Changes in this proposal",
    }
}

/// An author as people read it (F1): `human:nick` is `nick`, `agent:claude` is `claude (agent)`,
/// any other text stays as it is, and every `_` shows as a space.
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
            "v1.2.0, 3 updates since"
        );
        assert_eq!(
            text_label(&pos(Some("v1.2.0"), 1, false), None).text,
            "v1.2.0, 1 update since"
        );
        let p = reviewed(ReviewedKind::Proposal, "Guest mode", None, 3);
        assert_eq!(
            text_label(&pos(Some("v1.2.0"), 0, false), Some(&p)).text,
            "v1.2.0 · proposal: Guest mode, 3 changes, not landed"
        );
        let v = reviewed(ReviewedKind::View, "work", None, 1);
        assert_eq!(
            text_label(&pos(Some("v1.2.0"), 3, false), Some(&v)).text,
            "v1.2.0, 3 updates since · view: work, 1 change, not landed"
        );
        let none = reviewed(ReviewedKind::View, "work", None, 0);
        assert_eq!(
            text_label(&pos(None, 0, false), Some(&none)).text,
            "unversioned · view: work, no changes yet"
        );
    }

    /// F7: "updates" is the word of landings only; a view or proposal says "changes".
    #[test]
    fn sr17_updates_names_landings_and_never_a_review() {
        for kind in [ReviewedKind::View, ReviewedKind::Proposal] {
            let r = reviewed(kind, "work", Some("human:nick"), 2);
            let p = pos(Some("v1.0.0"), 2, false);
            let text = text_label(&p, Some(&r)).text;
            let (version, review) = text.split_once(" · ").expect("two parts");
            assert_eq!(version, "v1.0.0, 2 updates since");
            assert!(!review.contains("update"), "{review}");
            assert!(review.ends_with("2 changes, not landed"), "{review}");
            let h = header(&p, Some(&r));
            assert!(!h.review.unwrap().hover.contains("update"));
            // the chip keeps its words: +N, counting landings
            let since = h.since.unwrap();
            assert_eq!(
                (since.text.as_str(), since.hover.as_str()),
                ("+2", "2 landings since v1.0.0")
            );
        }
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
