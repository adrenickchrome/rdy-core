//! The left navigation: the neighbourhood of the page a reader is on, bounded by a cap.
use crate::page::SiteFacts;
use crate::review::{Mark, Review};
use crate::site::Site;
use rdy_model::{Block, BlockId, tree};
use serde::Serialize;
use std::collections::BTreeMap;

/// The most entries a list shows; the rest are one "+N more" entry that leads to the folder page.
const CAP: usize = 50;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Nav {
    pub groups: Vec<NavGroup>,
    /// The docs and folders a draft removed, by the label they had; empty for canon and tags.
    pub removed: Vec<String>,
}

/// A labelled run of entries: the top-level docs, one top-level folder, or the collected pages.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NavGroup {
    /// The group's heading; the first group has none.
    pub label: Option<String>,
    /// Where the heading leads (a folder's page), when it leads anywhere.
    pub href: Option<String>,
    /// The page the reader is on is the one the heading leads to.
    pub current: bool,
    /// The word a draft gives the folder the group is for.
    pub change: Option<Mark>,
    pub entries: Vec<NavEntry>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct NavEntry {
    pub label: String,
    pub href: String,
    /// The page the reader is on.
    pub current: bool,
    /// A folder above the page the reader is on.
    pub on_path: bool,
    /// The entries of a folder on the path, one level down.
    pub children: Vec<NavEntry>,
    /// Instead of a page: how many entries the cap hid, and the folder page that lists them.
    pub more: Option<(usize, String)>,
    /// The word a draft gives the page.
    pub change: Option<Mark>,
    /// A folder the reader can open and close in place: one of the folders of the neighbourhood
    /// of the page (see [`Ctx::entry`]). Its children are one level down; the folders among them
    /// are ordinary page links. It opens by default when it is on the path to the page.
    pub folder: bool,
    /// Where an Edited doc's word leads: its group on the change summary.
    pub change_href: Option<String>,
}

/// The navigation as page `from` shows it, `current` being the block the page is for.
/// `review_anchors` holds the change summary's anchor of each doc, by page path.
pub fn build(
    site: &Site,
    current: Option<&BlockId>,
    from: &str,
    facts: SiteFacts,
    review_anchors: &BTreeMap<String, String>,
) -> Nav {
    let review = facts.review;
    let chain = current.map(|c| site.chain(c)).unwrap_or_default();
    let ctx = Ctx {
        site,
        from,
        current,
        chain: &chain,
        review,
        review_anchors,
    };
    let plain = |label: &str, to: &str| NavEntry {
        label: label.to_string(),
        href: relative(from, to),
        current: from == to,
        ..NavEntry::default()
    };
    let mut groups = vec![];
    let mut first = vec![plain("Home", "index.html")];
    first.extend(ctx.list(None, relative(from, "index.html")));
    groups.push(NavGroup {
        entries: first,
        ..NavGroup::default()
    });
    for folder in site.top_folders() {
        let Some(href) = site.page(&folder.id).map(|p| relative(from, &p)) else {
            continue;
        };
        groups.push(NavGroup {
            label: Some(site.label(folder)),
            current: current == Some(&folder.id),
            change: review.and_then(|r| r.page_mark(&folder.id)),
            entries: ctx.list(Some(&folder.id), href.clone()),
            href: Some(href),
        });
    }
    let mut reference = vec![];
    if facts.has_terms {
        reference.push(plain("Glossary", "glossary.html"));
    }
    if facts.has_questions {
        reference.push(plain("Open questions", "questions.html"));
    }
    if facts.has_disagreements {
        reference.push(plain("Where the docs disagree", "disagreements.html"));
    }
    if !reference.is_empty() {
        groups.push(NavGroup {
            label: Some("Reference".into()),
            entries: reference,
            ..NavGroup::default()
        });
    }
    let removed = review
        .map(|r| {
            // a named profile's removed page reads as its old selected side titles it
            let old = facts.old.unwrap_or(site);
            r.removed_pages().iter().map(|b| old.label(b)).collect()
        })
        .unwrap_or_default();
    Nav { groups, removed }
}

/// The shortest link from page `from` to page `to`, both site-relative: a sibling is its bare
/// file name, a page in another folder goes up only as far as the folders differ.
pub fn relative(from: &str, to: &str) -> String {
    let from_dirs: Vec<&str> = from.split('/').collect();
    let from_dirs = &from_dirs[..from_dirs.len() - 1];
    let to_parts: Vec<&str> = to.split('/').collect();
    let common = from_dirs
        .iter()
        .zip(&to_parts[..to_parts.len() - 1])
        .take_while(|(a, b)| a == b)
        .count();
    format!(
        "{}{}",
        "../".repeat(from_dirs.len() - common),
        to_parts[common..].join("/")
    )
}

/// What building one page's navigation reads.
struct Ctx<'a> {
    site: &'a Site<'a>,
    from: &'a str,
    current: Option<&'a BlockId>,
    /// The folders and doc from the top level down to the current page.
    chain: &'a [BlockId],
    /// What a draft changes; none for canon and tags.
    review: Option<&'a Review>,
    /// The change summary's anchor of each doc, by page path; empty without that summary.
    review_anchors: &'a BTreeMap<String, String>,
}

impl Ctx<'_> {
    /// The entry of a folder or doc. A folder on the path is open down to the page. Any other
    /// folder of the neighbourhood is closed and holds its first level only: its children are
    /// plain page links, and a folder among them is not extended.
    fn entry(&self, b: &Block) -> NavEntry {
        let current = self.current == Some(&b.id);
        let on_path = !current && self.chain.contains(&b.id);
        let mut e = self.plain(b, current, on_path);
        if b.block_type == tree::FOLDER {
            e.folder = true;
            e.children = if current || on_path {
                self.list(Some(&b.id), e.href.clone())
            } else {
                self.first_level(&b.id, e.href.clone())
            };
        }
        e
    }

    /// The entry of a folder or doc, without children.
    fn plain(&self, b: &Block, current: bool, on_path: bool) -> NavEntry {
        let href = self
            .site
            .page(&b.id)
            .map(|p| relative(self.from, &p))
            .unwrap_or_default();
        let change = self.review.and_then(|r| r.page_mark(&b.id));
        NavEntry {
            label: self.site.label(b),
            href,
            current,
            on_path,
            change,
            change_href: self.change_href(b, change),
            ..NavEntry::default()
        }
    }

    /// Where the Edited word of a doc leads: its group on the change summary, when the build
    /// writes that summary and the doc has a group.
    fn change_href(&self, b: &Block, change: Option<Mark>) -> Option<String> {
        if change != Some(Mark::Edited) || b.block_type == tree::FOLDER {
            return None;
        }
        let anchor = self.review_anchors.get(&self.site.page(&b.id)?)?;
        Some(format!("{}#{anchor}", relative(self.from, "changes.html")))
    }

    /// The first level of a folder off the path, cut to [`CAP`] from its start: page links only.
    fn first_level(&self, folder: &BlockId, folder_href: String) -> Vec<NavEntry> {
        let list = self.site.nav_list(Some(folder));
        // no entry is on the path here: a capped list shows its first entries
        let (start, capped) = match window(list.len(), None) {
            None => (0, false),
            Some(start) => (start, true),
        };
        let end = if capped { start + CAP } else { list.len() };
        let mut out: Vec<NavEntry> = list[start..end]
            .iter()
            .map(|b| self.plain(b, false, false))
            .collect();
        if capped {
            out.push(more(list.len(), folder_href));
        }
        out
    }

    /// The entries of the navigation list under `parent` (see [`Site::nav_list`]), cut to
    /// [`CAP`] as [`cap`] cuts them; only the entries shown are built.
    fn list(&self, parent: Option<&BlockId>, folder_href: String) -> Vec<NavEntry> {
        let list = self.site.nav_list(parent);
        // the entry on the path is the one of the chain that sits in this list
        let at = self
            .chain
            .iter()
            .find(|c| {
                self.site
                    .m
                    .get(c)
                    .is_some_and(|b| b.parent.as_ref() == parent)
            })
            .and_then(|c| self.site.nav_pos(c))
            .filter(|&i| list.get(i).is_some_and(|b| self.chain.contains(&b.id)));
        match window(list.len(), at) {
            None => list.iter().map(|b| self.entry(b)).collect(),
            Some(start) => {
                let mut shown: Vec<NavEntry> = list[start..start + CAP]
                    .iter()
                    .map(|b| self.entry(b))
                    .collect();
                shown.push(more(list.len(), folder_href));
                shown
            }
        }
    }
}

/// Where the [`CAP`] entries shown of a list of `n` start, around the one at `at` (the first
/// ones when none is); none when the whole list is shown.
fn window(n: usize, at: Option<usize>) -> Option<usize> {
    (n > CAP).then(|| at.map_or(0, |i| i.saturating_sub(CAP / 2).min(n - CAP)))
}

/// The closing "+N more" entry of a list of `n` cut to [`CAP`].
fn more(n: usize, folder_href: String) -> NavEntry {
    NavEntry {
        more: Some((n - CAP, folder_href)),
        ..NavEntry::default()
    }
}

/// `entries` cut to [`CAP`] around the one on the path (the first ones when none is), with a
/// closing "+N more" entry that leads to `folder_href`.
#[cfg(test)]
fn cap(mut entries: Vec<NavEntry>, folder_href: String) -> Vec<NavEntry> {
    let n = entries.len();
    let at = entries.iter().position(|e| e.current || e.on_path);
    let Some(start) = window(n, at) else {
        return entries;
    };
    let mut shown: Vec<NavEntry> = entries.drain(start..start + CAP).collect();
    shown.push(more(n, folder_href));
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_goes_up_only_as_far_as_the_folders_differ() {
        assert_eq!(relative("a/two.html", "a/one.html"), "one.html");
        assert_eq!(
            relative("a/two.html", "b/c/three.html"),
            "../b/c/three.html"
        );
        assert_eq!(relative("b/c/three.html", "index.html"), "../../index.html");
        assert_eq!(relative("b/c/three.html", "b/index.html"), "../index.html");
        assert_eq!(relative("index.html", "a/one.html"), "a/one.html");
        assert_eq!(relative("a/one.html", "a/one.html"), "one.html");
        assert_eq!(relative("go/x.html", "a/one.html"), "../a/one.html");
    }

    fn entries(n: usize, on: Option<usize>) -> Vec<NavEntry> {
        (0..n)
            .map(|i| NavEntry {
                label: format!("e{i}"),
                current: on == Some(i),
                ..NavEntry::default()
            })
            .collect()
    }

    #[test]
    fn a_list_within_the_cap_is_whole() {
        assert_eq!(cap(entries(50, None), "x".into()).len(), 50);
    }

    #[test]
    fn a_long_list_is_cut_around_the_entry_on_the_path() {
        let l = cap(entries(120, Some(59)), "f.html".into());
        assert_eq!(l.len(), 51);
        assert_eq!(l[0].label, "e34");
        assert_eq!(l[49].label, "e83");
        assert_eq!(l[50].more, Some((70, "f.html".into())));
    }

    #[test]
    fn the_window_stays_inside_the_list_at_both_ends() {
        let l = cap(entries(120, Some(0)), "f".into());
        assert_eq!((l[0].label.as_str(), l[49].label.as_str()), ("e0", "e49"));
        let l = cap(entries(120, Some(119)), "f".into());
        assert_eq!((l[0].label.as_str(), l[49].label.as_str()), ("e70", "e119"));
        let l = cap(entries(120, None), "f".into());
        assert_eq!(l[0].label, "e0");
    }
}
