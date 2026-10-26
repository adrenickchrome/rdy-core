//! The version menu of the live site (D-2F29G; RULING F4, F5; D-CWC91): the versions, newest first
//! after canon, then each open proposal version, then each open view with a change, then the
//! publication profiles of the scope the menu is seen from, as one page at `/~versions/` that
//! the pill opens as a popover. Every word is a reader's: a date, a message, a name, an author and
//! the number of docs a scope changes; never an id, a hash or a ref. Nothing on it acts or waits
//! on a person (D-X61DT, k-BA85X, k-N0EJF): no approval, no readiness, no state word, no queue.
//! "Draft" is never said of a view or a proposal (D-BMX8P).
//!
//! The menu is seen from a context: a displayed scope and, optionally, the profile selected in
//! it. The source entry points ([`menu`], [`menu_key`], [`build_versions`]) are the menu of canon
//! with no profile; the `_for` ones take any context. All of them share the private functions
//! below.
use crate::live::{LiveBuild, rev};
use crate::page::Crumb;
use crate::site::Site;
use crate::{assets, env, home, render_page, review, version};
use rdy_engine::publication::{self, IMPLICIT_ORG, ResolvedProfile};
use rdy_engine::site_scope::{ScopeRef, tag_name_ok};
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{E_BAD_INPUT, E_NOT_FOUND, RdyError, ReadState, Scope};
use rdy_model::{BlockId, Hash, key};
use rdy_schema::publication::PROFILE_TYPE;
use serde::Serialize;
use std::collections::BTreeMap;

/// The page's title, its heading and the name of its navigation (O9).
pub const TITLE: &str = "Versions, proposals, views and profiles";

/// The most stored profiles the Profiles list shows. The others are counted, not listed.
const PROFILE_ROWS: usize = 100;

/// One row of the menu: what it opens and the reader's words beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MenuRow {
    pub label: String,
    /// What the row opens; none for a stored tag whose name is longer than the site serves
    /// (ruling final (3)): the row is text only.
    pub href: Option<String>,
    pub meta: Vec<String>,
    /// The profile the context selects (the Profiles list only): the row says `(current)` and
    /// carries `aria-current="page"`.
    pub current: bool,
}

/// The four lists of the menu, each in its order. `profiles` is empty when the displayed scope
/// stores no profile; else it holds "All docs" and at most 100 stored profiles, and
/// `profiles_omitted` counts the stored profiles it leaves out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Menu {
    pub versions: Vec<MenuRow>,
    pub proposals: Vec<MenuRow>,
    pub views: Vec<MenuRow>,
    pub profiles: Vec<MenuRow>,
    pub profiles_omitted: usize,
}

/// `1 doc changed` or `N docs changed`.
fn docs_words(n: usize) -> String {
    format!("{n} {} changed", if n == 1 { "doc" } else { "docs" })
}

/// `1 doc` or `N docs`: the docs a profile selects.
fn count_words(n: usize) -> String {
    format!("{n} {}", if n == 1 { "doc" } else { "docs" })
}

/// The words after the Profiles list when stored profiles are left out of it.
fn omitted_words(n: usize) -> String {
    if n == 1 {
        "1 more profile: ask your agent for its link".to_string()
    } else {
        format!("{n} more profiles: ask your agent for their links")
    }
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

/// A menu context after its checks: the displayed scope and the key of the profile selected in
/// it, if any. `Source(Versions)` is the menu of canon with no profile.
struct Context {
    scope: ScopeRef,
    profile: Option<String>,
}

impl Context {
    fn of(target: &SiteTarget) -> Result<Context, RdyError> {
        target.validate()?;
        match target {
            SiteTarget::Menu { scope, profile } => Ok(Context {
                scope: scope.clone(),
                profile: profile.clone(),
            }),
            SiteTarget::Source(ScopeRef::Versions) => Ok(Context::source()),
            _ => Err(RdyError::new(
                E_BAD_INPUT,
                "invalid live site target",
                "use a menu target",
            )),
        }
    }

    /// The menu of canon, with no profile selected.
    fn source() -> Context {
        Context {
            scope: ScopeRef::Canon,
            profile: None,
        }
    }

    /// The target the context's own pages start at: the displayed scope's source, or the
    /// selected profile in it.
    fn home(&self) -> SiteTarget {
        match &self.profile {
            None => SiteTarget::Source(self.scope.clone()),
            Some(key) => SiteTarget::Profile {
                scope: self.scope.clone(),
                key: key.clone(),
            },
        }
    }

    /// The link to the menu of this context.
    fn menu_url(&self) -> String {
        SiteTarget::Menu {
            scope: self.scope.clone(),
            profile: self.profile.clone(),
        }
        .menu_url()
        .unwrap_or_else(|| "/~versions/".to_string())
    }
}

/// The scope a displayed `ScopeRef` reads, as the live site resolves it.
fn read_scope(st: &ReadState, scope: &ScopeRef) -> Result<Scope, RdyError> {
    match scope {
        ScopeRef::Canon => Ok(Scope::Canon),
        ScopeRef::View(name) => st.resolve_read_scope(&format!("view:{name}"), None),
        ScopeRef::Proposal(r) => st.resolve_scope(&format!("proposal:{r}")),
        ScopeRef::Tag(name) => st.resolve_scope(&format!("tag:{name}")),
        ScopeRef::Versions => Err(RdyError::new(
            E_BAD_INPUT,
            "the versions page is not a read scope",
            "read canon, a version, a view or a proposal version",
        )),
    }
}

/// The stored profiles of one scope that the list shows, and how many it leaves out.
struct Listing {
    /// The smallest [`PROFILE_ROWS`] by (title, key, id).
    shown: Vec<ResolvedProfile>,
    omitted: usize,
}

/// Reads every stored `publication_profile` of `scope` once, by the metadata check a build
/// makes and no selection: an invalid profile fails the read, shown or not. Keeps only the
/// smallest [`PROFILE_ROWS`] rows and counts the rest.
fn list_profiles(st: &ReadState, scope: &Scope) -> Result<Listing, RdyError> {
    let m = st.materialized(scope)?;
    let mut kept: BTreeMap<(String, String, String), ResolvedProfile> = BTreeMap::new();
    let mut omitted = 0;
    for b in m.iter().filter(|b| b.block_type == PROFILE_TYPE) {
        let p = publication::profile_at(st, scope, b.id.as_str())?;
        let order = (
            p.title.clone(),
            p.key.clone().unwrap_or_default(),
            b.id.to_string(),
        );
        kept.insert(order, p);
        if kept.len() > PROFILE_ROWS {
            kept.pop_last();
            omitted += 1;
        }
    }
    Ok(Listing {
        shown: kept.into_values().collect(),
        omitted,
    })
}

/// The profile a context selects, in the displayed scope.
enum Selected {
    /// Full source.
    Nothing,
    /// `implicit:org`: no block, so nothing to follow by id.
    Implicit,
    /// A stored profile, by its id.
    Stored(BlockId),
}

fn selected(st: &ReadState, scope: &Scope, profile: Option<&str>) -> Result<Selected, RdyError> {
    match profile {
        None => Ok(Selected::Nothing),
        Some(IMPLICIT_ORG) => Ok(Selected::Implicit),
        Some(name) => Ok(match publication::profile_at(st, scope, name)?.id {
            Some(id) => Selected::Stored(id),
            None => Selected::Implicit,
        }),
    }
}

/// Where a scope row leads: the scope's source, or the selected profile in it when that scope
/// holds it. A stored profile is followed by its id, so a renamed key follows and a reused key
/// does not; an absent id leads to the full source. `scope` is the row's read scope; none when
/// the row has no scope to read (a tag whose landing is unreadable).
fn row_root(
    st: &ReadState,
    selected: &Selected,
    row: &ScopeRef,
    scope: Option<&Scope>,
) -> Result<String, RdyError> {
    let target = match selected {
        Selected::Nothing => SiteTarget::Source(row.clone()),
        Selected::Implicit => SiteTarget::Profile {
            scope: row.clone(),
            key: IMPLICIT_ORG.to_string(),
        },
        Selected::Stored(id) => {
            let held = match scope {
                Some(scope) => match publication::profile_at(st, scope, id.as_str()) {
                    Ok(p) => p.key,
                    Err(e) if e.code == E_NOT_FOUND => None,
                    Err(e) => return Err(e),
                },
                None => None,
            };
            match held {
                Some(k) => SiteTarget::Profile {
                    scope: row.clone(),
                    key: key::fold(&k),
                },
                None => SiteTarget::Source(row.clone()),
            }
        }
    };
    Ok(target.root())
}

/// The Profiles list: empty when the scope stores no profile, else "All docs" first and the
/// shown profiles. Only these rows resolve their selection, for the docs they select.
fn profile_rows(
    st: &ReadState,
    displayed: &ScopeRef,
    scope: &Scope,
    listing: &Listing,
    selected: &Selected,
) -> Result<Vec<MenuRow>, RdyError> {
    if listing.shown.is_empty() {
        return Ok(vec![]);
    }
    let mut rows = vec![MenuRow {
        label: "All docs".into(),
        href: Some(SiteTarget::Source(displayed.clone()).root()),
        meta: vec![],
        current: matches!(selected, Selected::Nothing),
    }];
    for p in &listing.shown {
        let name = p.id.as_ref().map(BlockId::as_str).unwrap_or_default();
        let docs = publication::resolve_inspection(st, scope, name)?.docs.len();
        let key = p.key.as_deref().map(key::fold).unwrap_or_default();
        rows.push(MenuRow {
            label: p.title.clone(),
            href: Some(
                SiteTarget::Profile {
                    scope: displayed.clone(),
                    key,
                }
                .root(),
            ),
            meta: vec![p.audience.as_str().to_string(), count_words(docs)],
            current: matches!(selected, Selected::Stored(id) if Some(id) == p.id.as_ref()),
        });
    }
    Ok(rows)
}

/// The menu of the source site: canon, with no profile selected.
pub fn menu(
    st: &ReadState,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<Menu, RdyError> {
    menu_in(st, &Context::source(), docs_changed)
}

/// The menu as seen from `context`, a menu target: the lists of the displayed scope, with the
/// selected profile kept by each row whose scope holds it. `docs_changed` is the callback of
/// [`menu`]: proposal and view counts are always the full-source counts.
pub fn menu_for(
    st: &ReadState,
    context: &SiteTarget,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<Menu, RdyError> {
    menu_in(st, &Context::of(context)?, docs_changed)
}

fn menu_in(
    st: &ReadState,
    cx: &Context,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<Menu, RdyError> {
    let displayed = read_scope(st, &cx.scope)?;
    let listing = list_profiles(st, &displayed)?;
    let selected = selected(st, &displayed, cx.profile.as_deref())?;

    // canon first, by its text label; then each version, newest first, dated as the home page
    // dates it (the day its landing was approved)
    let canon = version::text_label(&st.version_position(&Scope::Canon)?, None);
    let mut versions = vec![MenuRow {
        label: "Latest".into(),
        href: Some(row_root(
            st,
            &selected,
            &ScopeRef::Canon,
            Some(&Scope::Canon),
        )?),
        meta: vec![canon.text],
        current: false,
    }];
    for v in st.versions()?.iter().rev() {
        let landing = v.landing.parse::<Hash>().ok();
        let when = landing
            .and_then(|h| st.reader().landing(&h).ok())
            .map(|rec| home::date(rec.approved_by.at_ms));
        let message = v.message.clone().filter(|m| !m.trim().is_empty());
        let row = ScopeRef::Tag(v.name.clone());
        versions.push(MenuRow {
            label: v.name.clone(),
            // a stored name over the site's bound has no page: the row is text only
            href: if tag_name_ok(&v.name) {
                Some(row_root(
                    st,
                    &selected,
                    &row,
                    landing.map(Scope::At).as_ref(),
                )?)
            } else {
                None
            },
            meta: when
                .into_iter()
                .chain(message)
                .chain(v.redacted.then(|| "redacted".to_string()))
                .collect(),
            current: false,
        });
    }

    let mut open = st.open_proposals()?;
    open.sort_by_cached_key(|(r, p)| (p.title.clone(), r.to_string()));
    let mut proposals = vec![];
    for (r, p) in open {
        let scope = Scope::Proposal(r.version);
        let n = docs_changed(&scope)?;
        proposals.push(MenuRow {
            label: p.title.clone(),
            href: Some(row_root(
                st,
                &selected,
                &ScopeRef::Proposal(r.to_string()),
                Some(&scope),
            )?),
            meta: vec![
                format!("by {}", version::author_words(&p.provenance.actor)),
                docs_words(n),
            ],
            current: false,
        });
    }

    let mut views = vec![];
    for name in listed_views(st) {
        let owner = st
            .view_head(&name)
            .and_then(|h| h.owner.as_ref())
            .map(|o| format!("by {}", version::author_words(&o.actor)));
        let scope = Scope::View(name.clone());
        let n = docs_changed(&scope)?;
        views.push(MenuRow {
            href: Some(row_root(
                st,
                &selected,
                &ScopeRef::View(name.clone()),
                Some(&scope),
            )?),
            meta: owner.into_iter().chain([docs_words(n)]).collect(),
            current: false,
            label: name,
        });
    }
    let profiles = profile_rows(st, &cx.scope, &displayed, &listing, &selected)?;
    Ok(Menu {
        versions,
        proposals,
        views,
        profiles,
        profiles_omitted: listing.omitted,
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

/// What the menu seen from `context` reads, as one hash: [`menu_key`], the normalized context,
/// the displayed scope's head or version, then each shown profile's id and version and the
/// number left out. It resolves no selection: the scope's head already covers a change inside a
/// selected doc. An invalid profile gives the error a build gives.
pub fn menu_key_for(
    st: &ReadState,
    context: &SiteTarget,
    generation: u64,
) -> Result<Hash, RdyError> {
    let cx = Context::of(context)?;
    let displayed = read_scope(st, &cx.scope)?;
    let listing = list_profiles(st, &displayed)?;
    selected(st, &displayed, cx.profile.as_deref())?;
    let mut k = Key(Vec::new());
    k.hash(Some(menu_key(st, generation)))
        .text(&cx.scope.text())
        .opt_text(cx.profile.as_deref());
    match &displayed {
        Scope::Canon => {
            k.hash(st.canon_head());
        }
        Scope::View(name) => {
            let head = st.view_head(name);
            k.hash(head.map(|h| h.head)).hash(st.effective_base(name));
        }
        Scope::At(h) | Scope::Proposal(h) => {
            k.hash(Some(*h));
        }
        Scope::Inflight => {}
    }
    k.num(listing.shown.len() as u64);
    for p in &listing.shown {
        k.opt_text(p.id.as_ref().map(BlockId::as_str))
            .hash(p.version);
    }
    k.num(listing.omitted as u64);
    Ok(Hash::of(&k.0))
}

#[derive(Serialize)]
struct Section<'a> {
    title: &'static str,
    rows: &'a [MenuRow],
    /// Plain words after the list: the profiles it leaves out. Not a link or a row.
    note: Option<String>,
}

/// What the site's layout (`layout.html`) reads, for the versions page: no scope, so no pill, no
/// search, no review bar and no label slot (F1, visual review).
#[derive(Serialize)]
struct Page<'a> {
    site_name: String,
    title: &'static str,
    init_js: &'static str,
    lang: &'static str,
    kind: &'static str,
    root: &'static str,
    live: bool,
    crumbs: [Crumb; 2],
    /// Where the page's "Home" leads: the displayed scope's source, or the selected profile.
    home_href: String,
    /// This page's own link, in the page list.
    menu_href: String,
    /// Canon's text label, as the Latest row shows it: the footer's words.
    latest: String,
    sections: Vec<Section<'a>>,
}

/// The versions page of the source site, as the live site serves it.
pub fn build_versions(
    st: &ReadState,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<LiveBuild, RdyError> {
    build_in(st, &Context::source(), docs_changed)
}

/// The versions page as seen from `context`, a menu target: [`menu_for`]'s lists, with Home and
/// the page's own link in that context.
pub fn build_versions_for(
    st: &ReadState,
    context: &SiteTarget,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<LiveBuild, RdyError> {
    build_in(st, &Context::of(context)?, docs_changed)
}

/// The versions page in the site's layout (the header with the brand, the theme toggle and the
/// Menu button, the breadcrumbs, the drawer and the footer), with no header slot and no label.
/// One file, `index.html`; its revision is the hash of its bytes.
fn build_in(
    st: &ReadState,
    cx: &Context,
    docs_changed: &mut dyn FnMut(&Scope) -> Result<usize, RdyError>,
) -> Result<LiveBuild, RdyError> {
    let m = menu_in(st, cx, docs_changed)?;
    let latest = m.versions[0].meta.join(" · ");
    let home_href = cx.home().root();
    let list = |title, rows| Section {
        title,
        rows,
        note: None,
    };
    let mut sections = vec![
        list("Versions", &m.versions),
        list("Proposals", &m.proposals),
        list("Views", &m.views),
    ];
    if !m.profiles.is_empty() {
        sections.push(Section {
            title: "Profiles",
            rows: &m.profiles,
            note: (m.profiles_omitted > 0).then(|| omitted_words(m.profiles_omitted)),
        });
    }
    let page = Page {
        site_name: st.project_name(),
        title: TITLE,
        init_js: assets::INIT_JS,
        lang: "en",
        kind: "versions",
        root: "/",
        live: true,
        crumbs: [
            Crumb {
                label: "Home".into(),
                href: Some(home_href.clone()),
            },
            Crumb {
                label: TITLE.into(),
                href: None,
            },
        ],
        home_href,
        menu_href: cx.menu_url(),
        latest,
        sections,
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
        epoch: st.redaction_epoch(),
        // the menu has no page to miss: a miss under it is canon's (the server's choice)
        not_found: Default::default(),
    })
}
