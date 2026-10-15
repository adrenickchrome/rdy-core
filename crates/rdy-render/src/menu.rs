//! The version menu of the live site (D-2F29G; RULING F4, F5): the versions, newest first after
//! canon, then each open proposal version, then each open view with a change, as one page at
//! `/~versions/` that the pill opens as a popover. Every word is a reader's: a date, a message, a
//! name, an author and the number of docs a scope changes; never an id, a hash or a ref. Nothing
//! on it acts or waits on a person (D-X61DT, k-BA85X, k-N0EJF): no approval, no readiness, no
//! state word, no queue. "Draft" is never said of a view or a proposal (D-BMX8P).
use crate::live::{LiveBuild, rev};
use crate::site::Site;
use crate::{assets, env, home, render_page, review, version};
use rdy_engine::site_scope::{ScopeRef, tag_name_ok};
use rdy_engine::{RdyError, ReadState, Scope};
use rdy_model::Hash;
use serde::Serialize;
use std::collections::BTreeMap;

/// The page's title, its heading and the name of its navigation (O9).
pub const TITLE: &str = "Versions, proposals and views";

/// One row of the menu: what it opens and the reader's words beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MenuRow {
    pub label: String,
    /// What the row opens; none for a stored tag whose name is longer than the site serves
    /// (ruling final (3)): the row is text only.
    pub href: Option<String>,
    pub meta: Vec<String>,
}

/// The three lists of the menu, each in its order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Menu {
    pub versions: Vec<MenuRow>,
    pub proposals: Vec<MenuRow>,
    pub views: Vec<MenuRow>,
}

/// `1 doc changed` or `N docs changed`.
fn docs_words(n: usize) -> String {
    format!("{n} {} changed", if n == 1 { "doc" } else { "docs" })
}

/// The open views the menu lists: open, with at least one included change, by name.
fn listed_views(st: &ReadState) -> Vec<String> {
    st.view_names()
        .into_iter()
        .filter(|name| {
            st.view_head(name).is_some_and(|h| !h.closed) && !st.included(name).is_empty()
        })
        .collect()
}

/// The menu of `st`. `docs_changed` counts the docs a view or proposal version changes (the live
/// site caches it per scope; see [`docs_changed`]).
pub fn menu(
    st: &ReadState,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<Menu, RdyError> {
    // canon first, by its text label; then each version, newest first, dated as the home page
    // dates it (the day its landing was approved)
    let canon = version::text_label(&st.version_position(&Scope::Canon)?, None);
    let mut versions = vec![MenuRow {
        label: "Latest".into(),
        href: Some("/".into()),
        meta: vec![canon.text],
    }];
    for v in st.versions()?.iter().rev() {
        let when = v
            .landing
            .parse::<Hash>()
            .ok()
            .and_then(|h| st.reader().landing(&h).ok())
            .map(|rec| home::date(rec.approved_by.at_ms));
        let message = v.message.clone().filter(|m| !m.trim().is_empty());
        versions.push(MenuRow {
            label: v.name.clone(),
            // a stored name over the site's bound has no page: the row is text only
            href: tag_name_ok(&v.name)
                .then(|| format!("/{}", ScopeRef::Tag(v.name.clone()).path_prefix())),
            meta: when.into_iter().chain(message).collect(),
        });
    }

    let mut open = st.open_proposals()?;
    open.sort_by_cached_key(|(r, p)| (p.title.clone(), r.to_string()));
    let mut proposals = vec![];
    for (r, p) in open {
        let n = docs_changed(&Scope::Proposal(r.version))?;
        proposals.push(MenuRow {
            label: p.title.clone(),
            href: Some(format!(
                "/{}",
                ScopeRef::Proposal(r.to_string()).path_prefix()
            )),
            meta: vec![
                format!("by {}", version::author_words(&p.provenance.actor)),
                docs_words(n),
            ],
        });
    }

    let mut views = vec![];
    for name in listed_views(st) {
        let owner = st
            .view_head(&name)
            .and_then(|h| h.owner.as_ref())
            .map(|o| format!("by {}", version::author_words(&o.actor)));
        let n = docs_changed(&Scope::View(name.clone()))?;
        views.push(MenuRow {
            href: Some(format!("/{}", ScopeRef::View(name.clone()).path_prefix())),
            meta: owner.into_iter().chain([docs_words(n)]).collect(),
            label: name,
        });
    }
    Ok(Menu {
        versions,
        proposals,
        views,
    })
}

/// The docs `scope` changes (O12): the rows of the home page's list of changes for that scope,
/// read from the same review ([`review::listed`]: a proposal version made on the empty project
/// counts against it), so the menu, the home page and the change page agree. A scope with no
/// review counts 0.
pub fn docs_changed(st: &ReadState, scope: &Scope) -> Result<usize, RdyError> {
    let Some(review) = review::listed(st, scope)? else {
        return Ok(0);
    };
    let m = st.materialized(scope)?;
    let name = st.project_name();
    let site = Site::new(&m, st.pack(), &name);
    Ok(home::changes(&site, &review).len())
}

/// A hash input: every field after its length, so no two inputs share bytes.
struct Key(Vec<u8>);

impl Key {
    fn num(&mut self, n: u64) -> &mut Self {
        self.0.extend_from_slice(&n.to_le_bytes());
        self
    }
    fn text(&mut self, s: &str) -> &mut Self {
        self.num(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn opt_text(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            Some(s) => self.num(1).text(s),
            None => self.num(0),
        }
    }
    fn hash(&mut self, h: Option<Hash>) -> &mut Self {
        match h {
            Some(h) => {
                self.num(1);
                self.0.extend_from_slice(&h.0);
                self
            }
            None => self.num(0),
        }
    }
}

/// What the menu reads, as one hash: the versions (name, landing, message, time), canon's head,
/// each open proposal version (its hash, title, author and the canon it was written on), each
/// listed view (name, owner, head op and effective base), the pack generation, and the project's
/// name (the page's title and brand). The same key means the same page.
pub fn menu_key(st: &ReadState, pack_generation: u64) -> Hash {
    let mut k = Key(Vec::new());
    // a store that cannot be read keys as empty; the build reports the error
    let versions = st.versions().unwrap_or_default();
    k.num(versions.len() as u64);
    for v in &versions {
        k.text(&v.name)
            .text(&v.landing)
            .opt_text(v.message.as_deref())
            .num(v.at_ms as u64);
    }
    k.hash(st.canon_head());
    let open = st.open_proposals().unwrap_or_default();
    k.num(open.len() as u64);
    for (r, p) in &open {
        k.hash(Some(r.version))
            .text(&p.title)
            .text(&p.provenance.actor)
            .hash(p.canon_at);
    }
    let views = listed_views(st);
    k.num(views.len() as u64);
    for name in &views {
        let head = st.view_head(name);
        k.text(name)
            .opt_text(
                head.and_then(|h| h.owner.as_ref())
                    .map(|o| o.actor.as_str()),
            )
            .hash(head.map(|h| h.head))
            .hash(st.effective_base(name));
    }
    k.num(pack_generation).text(&st.project_name());
    Hash::of(&k.0)
}

#[derive(Serialize)]
struct Section<'a> {
    title: &'static str,
    rows: &'a [MenuRow],
}

#[derive(Serialize)]
struct Page<'a> {
    site_name: String,
    title: &'static str,
    init_js: &'static str,
    sections: [Section<'a>; 3],
}

/// The versions page as the live site serves it: one file, `index.html`, with no header slot and
/// no label. Its revision is the hash of its bytes.
pub fn build_versions(
    st: &ReadState,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<LiveBuild, RdyError> {
    let m = menu(st, docs_changed)?;
    let page = Page {
        site_name: st.project_name(),
        title: TITLE,
        init_js: assets::INIT_JS,
        sections: [
            Section {
                title: "Versions",
                rows: &m.versions,
            },
            Section {
                title: "Proposals",
                rows: &m.proposals,
            },
            Section {
                title: "Views",
                rows: &m.views,
            },
        ],
    };
    let (bytes, slots) = render_page(&env()?, "versions.html", page)?;
    debug_assert!(slots.is_empty(), "the versions page has no slot");
    let mut revs = BTreeMap::new();
    revs.insert("index.html".to_string(), rev(&bytes));
    let mut files = BTreeMap::new();
    files.insert("index.html".to_string(), bytes);
    Ok(LiveBuild {
        files,
        revs,
        label: String::new(),
        header: String::new(),
        // the revision of an empty label and an empty header, as a page build spells it
        label_rev: rev(&[0u8; 16]),
        label_at: BTreeMap::new(),
        warnings: vec![],
    })
}
