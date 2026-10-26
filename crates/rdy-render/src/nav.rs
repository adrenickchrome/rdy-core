//! The left navigation: the neighbourhood of the page a reader is on, bounded by a cap.
use crate::page::SiteFacts;
use crate::review::{Mark, Review};
use crate::site::Site;
use rdy_model::{Block, BlockId, tree};
use serde::Serialize;
use std::collections::BTreeMap;

/// The most entries a list shows; the rest are one "+N more" entry that leads to the folder page.
const CAP: usize = 50;

/// The most entries the added first-level lists hold in total on one page (D-RQ3X4).
/// The number was measured on the bench on 2026-10-25 (10k docs): the largest page sidebar
/// is 49,628 bytes at 79 and 50,445 at 80, and the bound is under 50,000.
const ADDED_ENTRIES: usize = 79;

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
    // the fill class of each group's own entries, in step with `groups`
    let mut fills = vec![];
    let mut first = vec![plain("Home", "index.html")];
    first.extend(ctx.list(None, relative(from, "index.html")));
    groups.push(NavGroup {
        entries: first,
        ..NavGroup::default()
    });
    fills.push(Fill::Rest);
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
        fills.push(if chain.first() == Some(&folder.id) {
            Fill::Path
        } else {
            Fill::Rest
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
        fills.push(Fill::Rest);
    }
    cap_added(&mut groups, &fills, ADDED_ENTRIES);
    let removed = review
        .map(|r| {
            // a named profile's removed page reads as its old selected side titles it
            let old = facts.old.unwrap_or(site);
            r.removed_pages().iter().map(|b| old.label(b)).collect()
        })
        .unwrap_or_default();
    Nav { groups, removed }
}

/// Where a group's closed folders come in the fill of [`ADDED_ENTRIES`]: the group of the
/// page's top-level folder, or any other group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    Path,
    Rest,
}

/// One closed folder's place in the fill order and the size of its list.
struct Closed {
    /// 0 for a closed folder in a `Path` group, 1 for one in a `Rest` group.
    class: u8,
    /// How many open entries hold it, plus one; 0 in a `Rest` group, where depth is not ranked.
    level: usize,
    /// The entries of its list, a "+N more" entry included.
    size: usize,
}

/// Cuts the added first-level lists of `groups` so that the lists kept hold at most `limit`
/// entries in total (D-RQ3X4). `fills[i]` is the [`Fill`] of group `i`'s own entries.
///
/// A closed folder is a folder entry that is neither the page nor on its path; its list is its
/// children. The lists fill in this order: those in a `Path` group, the deepest level first,
/// then those in a `Rest` group; sidebar order decides within a level. A list that does not fit
/// is cut, and so is every closed folder after it in that order: the folder keeps its label and
/// link and has no list. The open lists of the path and of the top-level groups never count and
/// never change.
fn cap_added(groups: &mut [NavGroup], fills: &[Fill], limit: usize) {
    let mut closed = vec![];
    for (g, &fill) in groups.iter().zip(fills) {
        collect_closed(&g.entries, fill, 1, &mut closed);
    }
    // `closed` is in sidebar order; the sort is stable
    let mut order: Vec<usize> = (0..closed.len()).collect();
    order.sort_by_key(|&i| (closed[i].class, std::cmp::Reverse(closed[i].level)));
    let mut cut = vec![false; closed.len()];
    let mut total = 0usize;
    let mut fits = true;
    for &i in &order {
        fits = fits && total.saturating_add(closed[i].size) <= limit;
        if fits {
            total += closed[i].size;
        } else {
            cut[i] = true;
        }
    }
    if !cut.contains(&true) {
        return;
    }
    let mut seq = 0;
    for g in groups.iter_mut() {
        apply_cut(&mut g.entries, &mut seq, &cut);
    }
}

/// The closed folders of `entries` in sidebar order. `level` is the depth of `entries` below
/// the group.
fn collect_closed(entries: &[NavEntry], fill: Fill, level: usize, out: &mut Vec<Closed>) {
    for e in entries {
        if !e.folder {
            continue;
        }
        if e.current || e.on_path {
            collect_closed(&e.children, fill, level + 1, out);
        } else {
            let (class, level) = match fill {
                Fill::Path => (0, level),
                Fill::Rest => (1, 0),
            };
            out.push(Closed {
                class,
                level,
                size: e.children.len(),
            });
        }
    }
}

/// Turns each closed folder of `entries` that `cut` marks into a page link, walking in the
/// order [`collect_closed`] lists them; `seq` counts the closed folders passed.
fn apply_cut(entries: &mut [NavEntry], seq: &mut usize, cut: &[bool]) {
    for e in entries {
        if !e.folder {
            continue;
        }
        if e.current || e.on_path {
            apply_cut(&mut e.children, seq, cut);
        } else {
            if cut[*seq] {
                e.folder = false;
                e.children = vec![];
            }
            *seq += 1;
        }
    }
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

    /// A closed folder whose list holds `size` entries.
    fn closed(label: &str, size: usize) -> NavEntry {
        NavEntry {
            label: label.into(),
            href: format!("{label}.html"),
            folder: true,
            children: (0..size)
                .map(|i| NavEntry {
                    label: format!("{label}-{i}"),
                    href: format!("{label}-{i}.html"),
                    ..NavEntry::default()
                })
                .collect(),
            ..NavEntry::default()
        }
    }

    /// An open folder entry (`current` or on the path) with `children`.
    fn open(label: &str, current: bool, children: Vec<NavEntry>) -> NavEntry {
        NavEntry {
            label: label.into(),
            href: format!("{label}.html"),
            current,
            on_path: !current,
            folder: true,
            children,
            ..NavEntry::default()
        }
    }

    fn group(entries: Vec<NavEntry>) -> NavGroup {
        NavGroup {
            entries,
            ..NavGroup::default()
        }
    }

    fn find<'a>(groups: &'a [NavGroup], label: &str) -> &'a NavEntry {
        fn walk<'a>(entries: &'a [NavEntry], label: &str) -> Option<&'a NavEntry> {
            entries.iter().find_map(|e| {
                (e.label == label)
                    .then_some(e)
                    .or_else(|| walk(&e.children, label))
            })
        }
        groups
            .iter()
            .find_map(|g| walk(&g.entries, label))
            .unwrap_or_else(|| panic!("no entry {label}"))
    }

    /// The labels of the closed folders that still hold a list, in sidebar order.
    fn listed(groups: &[NavGroup], names: &[&str]) -> Vec<String> {
        names
            .iter()
            .filter(|n| find(groups, n).folder)
            .map(|n| n.to_string())
            .collect()
    }

    #[test]
    fn rdb5_added_lists_fill_beside_the_path_then_rest() {
        let build = || {
            vec![
                group(vec![NavEntry {
                    label: "Home".into(),
                    ..NavEntry::default()
                }]),
                group(vec![closed("R1", 4), closed("R2", 2)]),
                group(vec![closed("A", 3)]),
                group(vec![closed("R3", 5)]),
            ]
        };
        let fills = [Fill::Rest, Fill::Rest, Fill::Path, Fill::Rest];
        let names = ["R1", "R2", "A", "R3"];
        let mut g = build();
        cap_added(&mut g, &fills, 9);
        assert_eq!(listed(&g, &names), ["R1", "R2", "A"]);
        assert!(!find(&g, "R3").folder);
        let mut g = build();
        cap_added(&mut g, &fills, 6);
        assert_eq!(listed(&g, &names), ["A"]);
        let mut g = build();
        cap_added(&mut g, &fills, 14);
        assert_eq!(listed(&g, &names), names);
    }

    #[test]
    fn rdb5_deeper_path_levels_fill_first() {
        let build = || {
            vec![
                group(vec![
                    closed("A", 3),
                    open(
                        "P",
                        false,
                        vec![closed("B", 4), open("Q", true, vec![closed("C", 5)])],
                    ),
                ]),
                group(vec![closed("D", 1)]),
            ]
        };
        let fills = [Fill::Path, Fill::Rest];
        let names = ["A", "B", "C", "D"];
        let mut g = build();
        cap_added(&mut g, &fills, 12);
        assert_eq!(listed(&g, &names), ["A", "B", "C"]);
        let mut g = build();
        cap_added(&mut g, &fills, 9);
        assert_eq!(listed(&g, &names), ["B", "C"]);
        let mut g = build();
        cap_added(&mut g, &fills, 5);
        assert_eq!(listed(&g, &names), ["C"]);
        let mut g = build();
        cap_added(&mut g, &fills, 13);
        assert_eq!(listed(&g, &names), names);
    }

    #[test]
    fn rdb5_a_list_past_the_cap_is_a_page_link() {
        let mut marked = closed("Y", 5);
        marked.change = Some(Mark::Edited);
        marked.change_href = Some("changes.html#y".into());
        let mut g = vec![group(vec![
            closed("X", 3),
            marked.clone(),
            closed("Z", 0),
            closed("W", 1),
        ])];
        cap_added(&mut g, &[Fill::Rest], 6);
        let x = find(&g, "X");
        assert!(x.folder);
        assert_eq!(x.children.len(), 3);
        let y = find(&g, "Y");
        assert!(!y.folder);
        assert!(y.children.is_empty());
        assert_eq!(
            (
                y.label.as_str(),
                y.href.as_str(),
                y.change,
                y.change_href.as_deref()
            ),
            ("Y", "Y.html", Some(Mark::Edited), Some("changes.html#y"))
        );
        assert!(!y.current && !y.on_path && y.more.is_none());
        // every later closed folder is cut, an empty one and one that would fit included
        for later in ["Z", "W"] {
            let e = find(&g, later);
            assert!(!e.folder && e.children.is_empty(), "{later}");
            assert_eq!(e.href, format!("{later}.html"));
        }
    }

    #[test]
    fn rdb5_each_kept_list_keeps_fifty_and_more() {
        let long = || {
            let mut f = closed("L", 50);
            f.children.push(more(53, "L.html".into()));
            f
        };
        assert_eq!(long().children.len(), 51);
        let mut g = vec![group(vec![long(), closed("S", 1)])];
        cap_added(&mut g, &[Fill::Rest], 51);
        let l = find(&g, "L");
        assert!(l.folder);
        assert_eq!(l.children.len(), 51);
        assert_eq!(l.children[50].more, Some((3, "L.html".into())));
        // the next list does not fit
        assert!(!find(&g, "S").folder);
        // a list over the limit is cut whole, never trimmed
        let mut g = vec![group(vec![long()])];
        cap_added(&mut g, &[Fill::Rest], 50);
        let l = find(&g, "L");
        assert!(!l.folder);
        assert!(l.children.is_empty());
    }

    #[test]
    fn rdb5_open_lists_never_count_or_cut() {
        let plain = |n: usize| -> Vec<NavEntry> {
            (0..n)
                .map(|i| NavEntry {
                    label: format!("d{i}"),
                    href: format!("d{i}.html"),
                    ..NavEntry::default()
                })
                .collect()
        };
        let mut g = vec![
            group(vec![
                open("P", false, plain(60)),
                open("Q", true, plain(7)),
                closed("K", 2),
            ]),
            group(vec![plain(1).remove(0), plain(1).remove(0)]),
        ];
        cap_added(&mut g, &[Fill::Path, Fill::Rest], 0);
        assert_eq!(find(&g, "P").children.len(), 60);
        assert!(find(&g, "P").folder);
        assert_eq!(find(&g, "Q").children.len(), 7);
        assert!(find(&g, "Q").folder);
        assert_eq!((g[0].entries.len(), g[1].entries.len()), (3, 2));
        // only a closed folder changes
        let k = find(&g, "K");
        assert!(!k.folder && k.children.is_empty());
    }
}

/// The sidebars of a generated site, built through the one site build.
#[cfg(test)]
mod site_tests {
    use super::*;
    use crate::{Mode, NotFoundOut, Scope, Slots, build_site_from, prepare};

    /// The page's navigation element, from its opening tag to its closing tag.
    fn nav_of<'a>(html: &'a str, page: &str) -> &'a str {
        let from = html
            .find("<nav aria-label=\"Pages\">")
            .unwrap_or_else(|| panic!("{page}: no navigation"));
        let to = html[from..]
            .find("</nav>")
            .unwrap_or_else(|| panic!("{page}: navigation not closed"));
        &html[from..from + to]
    }

    /// The closed folders of `nav`: the text of each from its `<details class="nav-fold">` to
    /// the `</details>` that ends it (a closed folder holds page links only).
    fn closed_folders(nav: &str) -> Vec<&str> {
        const OPEN: &str = "<details class=\"nav-fold\">";
        nav.match_indices(OPEN)
            .map(|(i, _)| {
                let end = nav[i..].find("</details>").expect("details closed");
                &nav[i..i + end]
            })
            .collect()
    }

    /// The top-level groups of `nav`.
    fn groups(nav: &str) -> Vec<&str> {
        nav.split("<li class=\"nav-group\">").skip(1).collect()
    }

    #[test]
    fn rdb5_a_large_site_holds_at_most_the_cap_per_page() {
        // the tree size is fixed (it was ADDED_ENTRIES / 50 + 2 = 10 when the cap was 400); it stays above the cap
        let k = 10;
        let dir = tempfile::tempdir().expect("tempdir");
        rdy_engine::testkit::generate_tree(dir.path(), 10 + 10 * k, 5, 1);
        let r = rdy_engine::Rdy::open(dir.path()).expect("open");
        let read = r.read();
        let prepared = prepare::source(&read, &Scope::View("work".into()), prepare::Purpose::Site)
            .expect("prepare");
        let built = build_site_from(
            prepared,
            Mode::Static,
            Slots::Notices,
            None,
            NotFoundOut::None,
        )
        .expect("build");
        for page in [
            "index.html",
            "area-0/part-10/doc-0.html",
            "area-9/part-19/doc-4.html",
        ] {
            let bytes = built
                .site
                .files
                .get(page)
                .unwrap_or_else(|| panic!("no {page}"));
            let html = std::str::from_utf8(bytes).expect("utf-8");
            let nav = nav_of(html, page);
            assert!(!nav.contains("<script"), "{page}: script in the navigation");
            let closed = closed_folders(nav);
            let entries: usize = closed.iter().map(|c| c.matches("<li").count()).sum();
            assert!(
                entries <= ADDED_ENTRIES,
                "{page}: {entries} added entries over {ADDED_ENTRIES}"
            );
            if page == "index.html" {
                continue;
            }
            // the path's group holds the page link; its closed folders all hold their lists
            let on_page: Vec<&str> = groups(nav)
                .into_iter()
                .filter(|g| g.contains("aria-current=\"page\""))
                .collect();
            assert_eq!(on_page.len(), 1, "{page}");
            let own = closed_folders(on_page[0]);
            assert!(own.len() >= 9, "{page}: {} closed folders", own.len());
            for c in &own {
                assert!(
                    c.contains("<ul class=\"nav-sub\">"),
                    "{page}: a path folder lost its list"
                );
            }
            // another top-level group has a closed folder that the cap cut: a plain link to its folder page
            const LINK: &str = "<li class=\"nav-item\"><a href=\"";
            let plain = groups(nav)
                .into_iter()
                .filter(|g| !g.contains("aria-current=\"page\""))
                .flat_map(|g| g.split(LINK).skip(1))
                .any(|rest| {
                    // a cut folder links to its folder page; a doc link ends in `doc-N.html`
                    rest.split('"')
                        .next()
                        .is_some_and(|h| h.contains("part-") && h.ends_with("/index.html"))
                });
            assert!(
                plain,
                "{page}: no closed folder of another group is a page link"
            );
        }
    }
}
