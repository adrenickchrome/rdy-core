//! A publication profile on the live site (D-CWC91, D-DK03K): a profile route under a scope
//! (`<scope prefix>~profile/<key>/`) and the profile-only site of canon. Both read the profile's
//! selected state alone, through the same selected preparation as durable output
//! (`publication::prepare_selected`), with two differences the live site owns: a blocked public
//! profile's check is a warning, not a refusal (durable output still refuses it), and a route
//! shows the existing version header, the menu and the review of its own selected base.
//!
//! The review compares the profile's selected state in each side's own revision: the current
//! profile's id resolved in the review base (absent there, the side is empty). The neutral chip
//! counts the landings since the tag whose selected before and after states differ in what a
//! reader sees; [`ProfileHistoryMemo`] keeps one boolean per landing so a rebuild resolves only
//! the landings it has not seen.
use crate::changes::{self, Sides};
use crate::live::{self, ProfileBuild};
use crate::prepare::{self, FigureBlobs, History, Presentation};
use crate::render_input::capture_inputs;
use crate::site::Site;
use crate::{
    Budget, Mode, NotFoundOut, Slots, build_site_from, inline, publication, review, scope_warnings,
    version,
};
use rdy_engine::diff::diff_states;
use rdy_engine::publication::{
    self as selection, IMPLICIT_ORG, ReadPublicationSource, Resolved, ResolvedProfile,
};
use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, ScopeRef, summary_page_taken};
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{
    E_BAD_INPUT, Materialized, RdyError, ReadState, Scope, VersionPosition, VersionRef,
    history_facts,
};
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// The build warning of an interval past the ceiling (D-42YMF): `<message> <fix>`.
const HISTORY_WARNING: &str = "Profile history exceeds 100000 landings. The landings chip and since link are omitted. Open a tagged profile scope to read that version.";

/// Task 3's refusal of a target no profile build serves.
fn invalid_target() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "invalid live site target",
        "use a source scope and a publication profile key",
    )
}

/// Builds the live site of a profile target: `Profile` (a route under a scope) or `ProfileOnly`
/// (the profile-only site of canon). Any other target is refused with the invalid-target error,
/// and a target is validated before anything is read. `history` keeps the neutral chip's
/// per-landing results between builds of the same target (Task 6 keeps one per cached target).
/// The build is bounded by the publication manifest's limits (`Budget::publication`) while it
/// is made.
pub fn build_profile_live(
    st: &ReadState,
    target: &SiteTarget,
    history: &mut ProfileHistoryMemo,
) -> Result<ProfileBuild, RdyError> {
    build_within(st, target, history, &Budget::publication())
}

/// [`build_profile_live`] within `budget`.
pub(crate) fn build_within(
    st: &ReadState,
    target: &SiteTarget,
    history: &mut ProfileHistoryMemo,
    budget: &Budget,
) -> Result<ProfileBuild, RdyError> {
    let (prepared, profile, changes) = prepare_within(st, target, history, budget)?;
    let root = target.root();
    // the target's not-found page, from the same selected state, kept with its pages
    let built = build_site_from(
        prepared,
        Mode::Live { root: &root },
        Slots::Source,
        Some(budget),
        NotFoundOut::Live {
            root: &root,
            scope: false,
        },
    )?;
    Ok(ProfileBuild {
        live: live::of_site(built.site, built.not_found, st.redaction_epoch()),
        profile,
        changes,
    })
}

/// What a build of a profile target reads, as [`build_within`] builds from it: the selected
/// preparation, the profile the target resolved to, and the change summary's page the build
/// will write, if any.
fn prepare_within(
    st: &ReadState,
    target: &SiteTarget,
    history: &mut ProfileHistoryMemo,
    budget: &Budget,
) -> Result<(prepare::Prepared, ResolvedProfile, Option<&'static str>), RdyError> {
    target.validate()?;
    crate::probe::prepared();
    let (route, scope_ref, key) = match target {
        SiteTarget::Profile { scope, key } => (true, scope.clone(), key.as_str()),
        SiteTarget::ProfileOnly { key } => (false, ScopeRef::Canon, key.as_str()),
        SiteTarget::Source(_) | SiteTarget::Menu { .. } => return Err(invalid_target()),
    };
    let scope = read_scope(st, &scope_ref)?;
    // the profile in this scope's own revision; a blocked public check is returned, not refused
    let resolved = selection::resolve_inspection(st, &scope, key)?;
    let warning = resolved.references.live_warning();
    let profile = resolved.profile.clone();
    let inputs = capture_inputs(st, &scope)?;
    let source = ReadPublicationSource::with_settings(
        st,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    publication::check_inputs(&source, &inputs)?;

    // a review route reads the same profile id in its review base (no base: the empty project)
    let reviewing = route && matches!(scope, Scope::View(_) | Scope::Proposal(_));
    let base = if reviewing {
        review::base_of(st, &scope)
    } else {
        None
    };
    let old = match &base {
        Some(b) => selected_in(st, b, &profile)?,
        None => None,
    };

    // the old and new figures together, bounded before any of their bytes is read; then the
    // current side's bytes, with no output gate (D-CWC91)
    holds_figures(budget, &source, &resolved, old.as_ref())?;
    let bytes = selection::load_inspection_assets(&source, &resolved)?;
    let mut removed = publication::removed_blobs(&source, &resolved)?;
    let old_removed = match &old {
        Some(o) => publication::removed_blobs(&source, o)?,
        None => BTreeMap::new(),
    };
    let old_m: Arc<Materialized> = match old {
        Some(o) => {
            FigureBlobs::selected(BTreeMap::new(), old_removed.clone()).project(Arc::new(o.m))
        }
        None => Arc::new(Materialized::default()),
    };
    // an old side's removed figure reads as the notice on its removed line
    for (blob, request) in old_removed {
        removed.entry(blob).or_insert(request);
    }
    let blobs = FigureBlobs::selected(bytes, removed);
    let tag = match &scope_ref {
        ScopeRef::Tag(name) => {
            let (stored, redacted) = publication::chosen_tag(st, name)?;
            stored.map(|s| (s, redacted))
        }
        _ => None,
    };
    let (tag_name, tag_redacted) = tag.clone().unwrap_or_default();
    let mut prepared = publication::prepare_selected(
        resolved,
        blobs,
        warning.clone(),
        &inputs,
        &tag_name,
        tag_redacted,
    )?;
    let title = prepared.site_name.clone();
    let pack = prepared.pack.clone();
    let m = Arc::clone(&prepared.m);
    let site = Site::with_unavailable(&m, &pack, &title, Some(inline::UNAVAILABLE));

    // the review against the selected base
    let mut hist = History::empty();
    if reviewing {
        let listed = review::between_states(&old_m, &m);
        let reviewed = prepare::reviewed_of(st, &scope, listed.changed_blocks())?;
        if let Some(r) = &reviewed {
            let old_site = Site::with_unavailable(&old_m, &pack, &title, Some(inline::UNAVAILABLE));
            let diff = base.as_ref().map(|_| diff_states(&old_m, &m));
            let sides = Sides {
                from: &old_m,
                to: &m,
                text: &site,
                from_text: Some(&old_site),
            };
            let (s, inline) = changes::summary(
                &site,
                CHANGES_PAGE,
                &listed,
                diff.as_ref(),
                &sides,
                &changes::Of::Review(r.kind),
            );
            if !summary_page_taken(&m, CHANGES_PAGE) {
                hist.changes = Some(s);
            }
            // only a page with review marks shows an edited block's diff
            if base.is_some() {
                hist.inline_diffs = inline;
            }
        }
        hist.review_marks = base.is_some();
        hist.listed = Some(listed);
        hist.reviewed = reviewed;
        // removed lines and removed rows read their words through the old selected side
        hist.old = Some(Arc::clone(&old_m));
    }

    // the neutral chip: canon's selected history since its tag, for a route that is not a tag.
    // The walk that counts it also finds the tag, so a rebuild reads only the new landings
    let mut since: Option<(u32, String, Hash)> = None;
    let mut over = false;
    // the version a route's pill names, found by the memo's walks (never a full canon scan)
    let mut route_version: Option<Tagged> = None;
    if route && !matches!(scope, Scope::At(_)) {
        history.begin(Identity {
            profile: profile.id.clone(),
            epoch: st.redaction_epoch(),
            pack: pack_identity(&pack),
        });
        let tags = Tags::of(st)?;
        let prev =
            |h: &Hash| -> Result<Option<Hash>, RdyError> { Ok(st.reader().landing(h)?.prev) };
        let tip = st.canon_head();
        let mut canon_tag: Option<Tagged> = None;
        if let Some(tip) = tip {
            // only the two adjacent states of a walk are held; the shared side carries over.
            // Each side reads as its page would: a figure whose blob lost content is the notice
            let mut carried: Option<(Hash, Arc<Materialized>)> = None;
            let mut eval = |before: &Hash, after: &Hash| -> Result<bool, RdyError> {
                let from = match carried.take() {
                    Some((h, m)) if h == *before => m,
                    _ => selected_projected(st, &source, &Scope::At(*before), &profile)?,
                };
                let to = selected_projected(st, &source, &Scope::At(*after), &profile)?;
                let changed = review::between_states(&from, &to).changed_blocks() > 0;
                carried = Some((*after, to));
                Ok(changed)
            };
            let limit = history.ceiling.unwrap_or_else(ceiling);
            let counted = history.count(tip, &tags, limit, &prev, &mut eval)?;
            match (&counted.tag, counted.since) {
                (Some(t), Since::Count(n)) if n > 0 => {
                    since = Some((n, t.name.clone(), t.landing));
                }
                (_, Since::Over) => over = true,
                _ => {}
            }
            canon_tag = counted.tag;
        }
        // a scope that stands where canon stands names canon's version; a fixed base (a pinned
        // view's, a proposal version's) names its own, found once per tag set
        let base = match &scope {
            Scope::View(name) => st.effective_base(name),
            Scope::Proposal(v) => st.proposal_base(v)?,
            _ => tip,
        };
        route_version = match base {
            Some(b) if Some(b) == tip => canon_tag,
            Some(b) => history.version_at(b, &tags, &prev)?,
            None => None,
        };
    }
    // the summary since the tag: the tag's selected state against canon's
    if let Some((n, name, tag_landing)) = &since
        && !summary_page_taken(&m, SINCE_PAGE)
    {
        let from = selected_projected(st, &source, &Scope::At(*tag_landing), &profile)?;
        let to = match scope {
            Scope::Canon => Arc::clone(&m),
            _ => selected_projected(st, &source, &Scope::Canon, &profile)?,
        };
        let to_site = Site::with_unavailable(&to, &pack, &title, Some(inline::UNAVAILABLE));
        let from_site = Site::with_unavailable(&from, &pack, &title, Some(inline::UNAVAILABLE));
        let rv = review::between_states(&from, &to);
        let diff = diff_states(&from, &to);
        let sides = Sides {
            from: &from,
            to: &to,
            text: &to_site,
            from_text: Some(&from_site),
        };
        let (s, _) = changes::summary(
            &site,
            SINCE_PAGE,
            &rv,
            Some(&diff),
            &sides,
            &changes::Of::Since {
                tag: name.clone(),
                landings: *n,
            },
        );
        hist.since = Some(s);
    }

    // the warnings: the reference warning first, then the scope's, then the history's
    let mut warnings: Vec<String> = warning
        .map(|w| format!("{} {}", w.message, w.fix))
        .into_iter()
        .collect();
    warnings.extend(scope_warnings(
        &m,
        &site,
        hist.reviewed.is_some() && hist.listed.is_some(),
        since.as_ref().map(|(_, name, _)| name.as_str()),
    ));
    if over {
        warnings.push(HISTORY_WARNING.to_string());
    }
    hist.warnings = warnings;
    drop(site);

    let presentation = if route {
        let position = match (&scope, &tag) {
            (Scope::At(landing), Some((name, _))) => VersionPosition {
                version: Some(VersionRef {
                    name: name.clone(),
                    landing: landing.to_string(),
                }),
                landings_since: 0,
                view: None,
                frozen: true,
                label: String::new(),
            },
            // a route's header reads only the version its pill names: its neutral chip counts
            // the selected landings, so no raw landing count is read
            _ => VersionPosition {
                version: route_version.map(|t| VersionRef {
                    name: t.name,
                    landing: t.landing.to_string(),
                }),
                landings_since: 0,
                view: None,
                frozen: matches!(scope, Scope::At(_)),
                label: String::new(),
            },
        };
        let header = version::profile_header(
            &position,
            &title,
            since.as_ref().map(|(n, name, _)| (*n, name.as_str())),
            hist.reviewed.as_ref(),
        );
        let colophon = version::profile_colophon(
            &title,
            tag.as_ref().map(|(name, _)| name.as_str()),
            hist.reviewed.as_ref(),
        );
        prepared.position = position;
        Presentation::LiveRoute { header, colophon }
    } else {
        Presentation::LiveOnly {
            colophon: version::profile_colophon(&title, None, None),
        }
    };
    if let Some(p) = prepared.profile.as_mut() {
        p.presentation = presentation;
    }
    // the summary page the build generated, by the rules a source build's link follows: a review
    // route has `changes.html`, canon `changes-since.html` while it has selected landings since
    // its tag (each is left out when a top-level doc owns its page or the history is over its
    // limit). A tag route and the profile-only site have none
    let changes = match scope_ref {
        ScopeRef::View(_) | ScopeRef::Proposal(_) => hist.changes.is_some().then_some(CHANGES_PAGE),
        ScopeRef::Canon if route => hist.since.is_some().then_some(SINCE_PAGE),
        _ => None,
    };
    prepared.history = hist;
    prepared.menu_url = target.menu_url();
    drop(m);
    Ok((prepared, profile, changes))
}

/// The read scope a route names: a view must exist; `P-x` reads as the proposal's latest
/// version; a tag must exist. The versions page is no read scope (`validate` refuses it nested).
pub(crate) fn read_scope(st: &ReadState, scope: &ScopeRef) -> Result<Scope, RdyError> {
    match scope {
        ScopeRef::Canon => Ok(Scope::Canon),
        ScopeRef::View(name) => st.resolve_read_scope(&format!("view:{name}"), None),
        ScopeRef::Proposal(r) => st.resolve_scope(&format!("proposal:{r}")),
        ScopeRef::Tag(name) => st.resolve_scope(&format!("tag:{name}")),
        ScopeRef::Versions => Err(invalid_target()),
    }
}

/// `profile` resolved in `scope`: the same profile id in that state's own revision (None when the
/// state does not hold it), or the implicit profile through `implicit:org`. Each resolution keeps
/// the publication limits; an invalid or removed profile is the existing error.
fn selected_in(
    st: &ReadState,
    scope: &Scope,
    profile: &ResolvedProfile,
) -> Result<Option<Resolved>, RdyError> {
    match &profile.id {
        Some(id) => selection::resolve_revision(st, scope, id),
        None => selection::resolve_inspection(st, scope, IMPLICIT_ORG).map(Some),
    }
}

/// [`selected_in`]'s selected state, empty when the state does not hold the profile, with each
/// figure whose blob lost content read as removed. No asset byte is read.
fn selected_projected(
    st: &ReadState,
    source: &ReadPublicationSource<'_>,
    scope: &Scope,
    profile: &ResolvedProfile,
) -> Result<Arc<Materialized>, RdyError> {
    Ok(match selected_in(st, scope, profile)? {
        Some(r) => {
            let removed = publication::removed_blobs(source, &r)?;
            FigureBlobs::selected(BTreeMap::new(), removed).project(Arc::new(r.m))
        }
        None => Arc::new(Materialized::default()),
    })
}

/// The available figure blobs of the current selection and of the review's old one, each once,
/// held to `budget` by their manifests' lengths before any of their bytes is read.
fn holds_figures(
    budget: &Budget,
    source: &ReadPublicationSource<'_>,
    current: &Resolved,
    old: Option<&Resolved>,
) -> Result<(), RdyError> {
    use rdy_engine::publication::PublicationSource;
    let blobs: BTreeSet<Hash> = current
        .attachments
        .iter()
        .chain(old.into_iter().flat_map(|o| o.attachments.iter()))
        .filter(|a| a.available)
        .map(|a| a.blob)
        .collect();
    let mut lens = Vec::with_capacity(blobs.len());
    for blob in &blobs {
        if let Some(manifest) = source.manifest(blob)? {
            lens.push(manifest.len);
        }
    }
    budget.holds_blobs(lens)
}

/// The identity of a pack, for the memo: the hash of its JSON form (its maps are ordered).
fn pack_identity(pack: &rdy_schema::Pack) -> Hash {
    Hash::of(&serde_json::to_vec(pack).unwrap_or_default())
}

#[cfg(test)]
thread_local! {
    static CEILING: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The most landings an interval the chip counts may hold: [`history_facts::MAX_LANDINGS`] (a
/// test may lower it for its thread, [`with_ceiling`]).
fn ceiling() -> usize {
    #[cfg(test)]
    if let Some(n) = CEILING.with(std::cell::Cell::get) {
        return n;
    }
    history_facts::MAX_LANDINGS
}

/// Tests only: `f` with the ceiling lowered to `n` on this thread.
#[cfg(test)]
fn with_ceiling<T>(n: usize, f: impl FnOnce() -> T) -> T {
    let old = CEILING.with(|c| c.replace(Some(n)));
    let out = f();
    CEILING.with(|c| c.set(old));
    out
}

/// What a memo's results are valid for: one profile id (None for the implicit profile), one
/// redaction epoch and one captured pack.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    profile: Option<BlockId>,
    epoch: u64,
    pack: Hash,
}

/// A version as a walk finds it: its name and the landing it names.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tagged {
    name: String,
    landing: Hash,
}

/// A state's versions as a walk reads them: each tagged landing with its highest version (as
/// `ReadState::version_position` reads it), and one identity of the whole set.
struct Tags {
    on: HashMap<Hash, String>,
    id: Hash,
}

impl Tags {
    /// The versions `st` was published with. No landing is read.
    fn of(st: &ReadState) -> Result<Tags, RdyError> {
        let mut on = HashMap::new();
        let mut text = String::new();
        // in precedence order: a later version on the same landing is the higher one
        for (name, landing) in st.version_landings()? {
            text.push_str(&format!("{landing} {name}\n"));
            on.insert(landing, name);
        }
        Ok(Tags {
            on,
            id: Hash::of(text.as_bytes()),
        })
    }

    /// The version on landing `h`, if one names it.
    fn at(&self, h: &Hash) -> Option<Tagged> {
        self.on.get(h).map(|name| Tagged {
            name: name.clone(),
            landing: *h,
        })
    }
}

/// The aggregate of the last count, for one tag set: the version canon stands after (none when
/// no version lies on the chain), the canon tip it counted to, how many landings lie between
/// them, how many of those changed what the profile shows, and whether the interval is past the
/// ceiling.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Aggregate {
    tags: Hash,
    tag: Option<Tagged>,
    tip: Hash,
    visited: usize,
    selected: u32,
    over: bool,
}

impl Aggregate {
    fn counted(&self) -> Counted {
        Counted {
            tag: self.tag.clone(),
            since: if self.over {
                Since::Over
            } else {
                Since::Count(self.selected)
            },
        }
    }
}

/// What the neutral chip counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Since {
    /// The landings since the tag that change what the profile shows.
    Count(u32),
    /// The interval holds more landings than the ceiling: no chip and no since page.
    Over,
}

/// A count: the version canon stands after, and what the chip counts since it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Counted {
    tag: Option<Tagged>,
    since: Since,
}

/// How a walk from the tip ended.
enum End {
    /// At a tagged landing: the interval's start.
    Tag(Tagged),
    /// At the saved tip: the chain extends it.
    Saved,
    /// At the chain's first landing, with no version on the way.
    Root,
}

/// The most fixed bases (a pinned view's, a proposal version's) a memo keeps the version of.
const MAX_BASES: usize = 64;

/// The neutral chip's memo for one profile target (D-42YMF, D-PX5B0): for each landing, whether
/// its selected before and after states differ in a block a reader sees, keyed by `(landing hash,
/// profile id, redaction epoch)` (one id and one epoch per memo), and the aggregate of the last
/// count, which also holds the version canon stands after. It holds no block body, graph or
/// blob, and at most [`history_facts::MAX_LANDINGS`] results. A new profile id, epoch or pack
/// clears it; a new tag set resets the aggregate and keeps only the results of the new interval;
/// a chain that does not extend the saved tip clears it. A failed read never becomes a result.
/// It also keeps the version of up to [`MAX_BASES`] fixed bases, for one tag set.
#[derive(Default)]
pub struct ProfileHistoryMemo {
    identity: Option<Identity>,
    results: HashMap<Hash, bool>,
    aggregate: Option<Aggregate>,
    bases: HashMap<Hash, Option<Tagged>>,
    bases_tags: Option<Hash>,
    evaluations: u64,
    /// Tests only: the ceiling in place of [`history_facts::MAX_LANDINGS`] for this memo.
    ceiling: Option<usize>,
}

impl ProfileHistoryMemo {
    /// Tests only: how many landing pairs this memo has evaluated (resolved and compared).
    #[doc(hidden)]
    pub fn evaluations_for_tests(&self) -> u64 {
        self.evaluations
    }

    /// Tests only: the builds with this memo take `n` as the ceiling of the interval they count
    /// (`None`: [`history_facts::MAX_LANDINGS`]), so a test reaches the over-limit path with a
    /// short chain. The live site's cache sets it from its own test hook.
    #[doc(hidden)]
    pub fn set_ceiling_for_tests(&mut self, n: Option<usize>) {
        self.ceiling = n;
    }

    /// Readies the memo for `identity`: one of another profile, epoch or pack is cleared before
    /// any lookup.
    fn begin(&mut self, identity: Identity) {
        if self.identity.as_ref() != Some(&identity) {
            self.results.clear();
            self.aggregate = None;
            self.identity = Some(identity);
        }
    }

    /// The version canon's tip `tip` stands after, and the landings since it (exclusive) to the
    /// tip that change what the profile shows. The walk goes from the tip through `prev` only to
    /// the first tagged landing of `tags` or to the saved tip, so a rebuild reads only the new
    /// landings. `eval` says whether the pair (before, after) changes a block a reader sees; it
    /// runs only for a landing with no result yet, oldest first. More than `limit` landings in
    /// the interval is [`Since::Over`], found before any pair is evaluated and remembered for the
    /// tip, so an unchanged or extended over-limit chain is not scanned again. Past the ceiling
    /// the walk holds no more hashes; it reads on only to learn whether a version lies beyond.
    fn count(
        &mut self,
        tip: Hash,
        tags: &Tags,
        limit: usize,
        prev: &dyn Fn(&Hash) -> Result<Option<Hash>, RdyError>,
        eval: &mut dyn FnMut(&Hash, &Hash) -> Result<bool, RdyError>,
    ) -> Result<Counted, RdyError> {
        // a changed tag set resets the aggregate; the results stay usable for the new interval
        if self.aggregate.as_ref().is_some_and(|a| a.tags != tags.id) {
            self.aggregate = None;
        }
        if let Some(a) = &self.aggregate
            && a.tip == tip
        {
            return Ok(a.counted());
        }
        let saved = self.aggregate.clone();
        // the new landings, newest first: to the tag, to the saved tip or to the first landing
        let mut fresh: Vec<Hash> = vec![];
        let mut cur = tip;
        let end = loop {
            if let Some(t) = tags.at(&cur) {
                break End::Tag(t);
            }
            if saved.as_ref().is_some_and(|a| a.tip == cur) {
                break End::Saved;
            }
            if fresh.len() >= limit {
                return self.beyond(tip, cur, fresh.len(), tags, saved.as_ref(), prev);
            }
            fresh.push(cur);
            match prev(&cur)? {
                Some(p) => cur = p,
                None => break End::Root,
            }
        };
        let (tag, visited, mut selected, before_oldest) = match end {
            // no version on this chain: nothing to count, and nothing kept
            End::Root => {
                self.results.clear();
                self.aggregate = Some(Aggregate {
                    tags: tags.id,
                    tag: None,
                    tip,
                    visited: fresh.len(),
                    selected: 0,
                    over: false,
                });
                return Ok(Counted {
                    tag: None,
                    since: Since::Count(0),
                });
            }
            End::Saved => {
                let a = saved.expect("the saved tip ended the walk");
                let total = a.visited.saturating_add(fresh.len());
                if a.tag.is_none() || a.over || total > limit {
                    let over = a.tag.is_some();
                    self.aggregate = Some(Aggregate {
                        tip,
                        visited: total,
                        selected: 0,
                        over,
                        ..a
                    });
                    return Ok(self.aggregate.as_ref().map(Aggregate::counted).unwrap());
                }
                (a.tag, a.visited, a.selected, a.tip)
            }
            End::Tag(t) => {
                // a chain that does not extend the saved tip: never reuse its results
                if saved.is_some() {
                    self.results.clear();
                }
                // a walk from the tag holds the whole interval: results before the tag go
                let keep: HashSet<&Hash> = fresh.iter().collect();
                self.results.retain(|h, _| keep.contains(h));
                let landing = t.landing;
                (Some(t), 0, 0, landing)
            }
        };
        let total = visited.saturating_add(fresh.len());
        for i in (0..fresh.len()).rev() {
            let after = fresh[i];
            let changed = match self.results.get(&after) {
                Some(b) => *b,
                None => {
                    if self.results.len() >= limit {
                        return Ok(Counted {
                            tag,
                            since: Since::Over,
                        });
                    }
                    let before = fresh.get(i + 1).copied().unwrap_or(before_oldest);
                    let b = eval(&before, &after)?;
                    self.evaluations += 1;
                    self.results.insert(after, b);
                    b
                }
            };
            if changed {
                selected = selected.saturating_add(1);
            }
        }
        self.aggregate = Some(Aggregate {
            tags: tags.id,
            tag: tag.clone(),
            tip,
            visited: total,
            selected,
            over: false,
        });
        Ok(Counted {
            tag,
            since: Since::Count(selected),
        })
    }

    /// [`ProfileHistoryMemo::count`] past the ceiling: `held` landings from `tip` are counted and
    /// `cur` is the next, neither tagged nor the saved tip. The walk reads on, holding no hash,
    /// to the first tagged landing (the interval is over the limit: no pair is evaluated), to the
    /// saved tip (its version) or to the chain's first landing (no version: nothing to count).
    fn beyond(
        &mut self,
        tip: Hash,
        mut cur: Hash,
        held: usize,
        tags: &Tags,
        saved: Option<&Aggregate>,
        prev: &dyn Fn(&Hash) -> Result<Option<Hash>, RdyError>,
    ) -> Result<Counted, RdyError> {
        let mut walked = held;
        let tag = loop {
            if let Some(t) = tags.at(&cur) {
                break Some(t);
            }
            if let Some(a) = saved.filter(|a| a.tip == cur) {
                walked = walked.saturating_add(a.visited);
                break a.tag.clone();
            }
            walked = walked.saturating_add(1);
            match prev(&cur)? {
                Some(p) => cur = p,
                None => break None,
            }
        };
        if tag.is_none() {
            self.results.clear();
        }
        let a = Aggregate {
            tags: tags.id,
            over: tag.is_some(),
            tag,
            tip,
            visited: walked,
            selected: 0,
        };
        let counted = a.counted();
        self.aggregate = Some(a);
        Ok(counted)
    }

    /// The version a fixed landing `base` stands after: the first tagged landing walking back
    /// from it. A landing's version changes only with the tag set, so each base is walked once
    /// per tag set (at most [`MAX_BASES`] are kept).
    fn version_at(
        &mut self,
        base: Hash,
        tags: &Tags,
        prev: &dyn Fn(&Hash) -> Result<Option<Hash>, RdyError>,
    ) -> Result<Option<Tagged>, RdyError> {
        if self.bases_tags != Some(tags.id) {
            self.bases.clear();
            self.bases_tags = Some(tags.id);
        }
        if let Some(v) = self.bases.get(&base) {
            return Ok(v.clone());
        }
        let mut cur = Some(base);
        let found = loop {
            let Some(h) = cur else { break None };
            if let Some(t) = tags.at(&h) {
                break Some(t);
            }
            cur = prev(&h)?;
        };
        if self.bases.len() >= MAX_BASES {
            self.bases.clear();
        }
        self.bases.insert(base, found.clone());
        Ok(found)
    }
}

#[cfg(test)]
mod spv4 {
    use super::*;
    use crate::{Budget, live};
    use rdy_engine::publication_manifest::{MAX_ARTIFACT_BYTES, MAX_FILE_BYTES, MAX_FILES};
    use rdy_engine::site_scope::ScopeRef;
    use rdy_engine::site_target::SiteTarget;
    use rdy_engine::{E_LIMIT, Rdy, Session, WriteRequest, history_facts};
    use rdy_model::{Hash, Role};
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, HashMap};

    fn session(role: Role) -> Session {
        Session::human("human:qa".into(), role).unwrap()
    }

    fn write(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: view.into(),
                ops: serde_json::from_value(ops).expect("ops"),
                justification: None,
                message: String::new(),
            },
        )
        .expect("write")
        .created
        .into_iter()
        .collect()
    }

    fn land(r: &mut Rdy, view: &str) {
        let p = r
            .propose(&session(Role::Writer), view, view, Some("test summary"))
            .expect("propose");
        r.land(&session(Role::Approver), &p.id).expect("land");
    }

    /// Lands an edit of block `id`'s body, in a view of its own.
    fn edit(r: &mut Rdy, n: &mut u32, id: &str, body: &str) {
        *n += 1;
        let view = format!("e{n}");
        write(r, &view, json!([{"op": "update", "id": id, "body": body}]));
        land(r, &view);
    }

    struct Project {
        _dir: tempfile::TempDir,
        r: Rdy,
        ids: BTreeMap<String, String>,
        n: u32,
    }

    /// Two docs (`intro`, `other`) with a prose each, and profiles `guide` (intro) and `both`
    /// (both docs), landed and tagged `v1.0.0`.
    fn project() -> Project {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
        let mut ids = write(
            &mut r,
            "w1",
            json!([
                {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "Intro words."},
                {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$op", "parent": "$other", "body": "Other words."},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
            ]),
        );
        land(&mut r, "w1");
        let p = write(
            &mut r,
            "w2",
            json!([
                {"op": "create", "type": "publication_profile", "id": "$guide", "parent": ids["$admin"],
                 "fields": {"key": "guide", "title": "Guide", "audience": "org", "select": [ids["$intro"]]}},
                {"op": "create", "type": "publication_profile", "id": "$both", "parent": ids["$admin"],
                 "fields": {"key": "both", "title": "Both", "audience": "org", "select": [ids["$intro"], ids["$other"]]}}
            ]),
        );
        land(&mut r, "w2");
        ids.extend(p);
        r.tag(&session(Role::Approver), "v1.0.0", None, None)
            .expect("tag");
        Project {
            _dir: dir,
            r,
            ids,
            n: 0,
        }
    }

    impl Project {
        fn edit(&mut self, key: &str, body: &str) {
            let id = self.ids[key].clone();
            edit(&mut self.r, &mut self.n, &id, body);
        }
    }

    fn canon(key: &str) -> SiteTarget {
        SiteTarget::Profile {
            scope: ScopeRef::Canon,
            key: key.into(),
        }
    }

    /// The since chip's text in a build's header, if it has one.
    fn chip(b: &live::ProfileBuild) -> Option<String> {
        let h = &b.live.header;
        let at = h.find("class=\"chip chip-since\"")?;
        let rest = &h[at..];
        let gt = rest.find('>').unwrap() + 1;
        Some(rest[gt..gt + rest[gt..].find('<').unwrap()].to_string())
    }

    #[track_caller]
    fn over(e: &RdyError, limit: String) {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                format!("publication manifest exceeds {limit}").as_str(),
                "reduce the artifact to the stated limit"
            ),
            "{e}"
        );
    }

    #[test]
    fn spv4_selected_history_memo_visits_only_new_landings() {
        let mut p = project();
        p.edit("$ip", "Intro words, one.");
        p.edit("$op", "Other words, two.");
        p.edit("$ip", "Intro words, three.");
        let mut memo = ProfileHistoryMemo::default();
        let run = |p: &Project, memo: &mut ProfileHistoryMemo, key: &str| {
            build_profile_live(&p.r.read(), &canon(key), memo).expect("build")
        };
        // cold: each landing since the tag once
        let b = run(&p, &mut memo, "guide");
        assert_eq!(memo.evaluations_for_tests(), 3);
        assert_eq!(chip(&b).as_deref(), Some("+2"));
        // unchanged: no pair
        run(&p, &mut memo, "guide");
        assert_eq!(memo.evaluations_for_tests(), 3);
        // one new landing: one pair
        p.edit("$op", "Other words, four.");
        let b = run(&p, &mut memo, "guide");
        assert_eq!(memo.evaluations_for_tests(), 4);
        assert_eq!(chip(&b).as_deref(), Some("+2"));
        // another profile id: the memo starts again
        let b = run(&p, &mut memo, "both");
        assert_eq!(memo.evaluations_for_tests(), 8);
        assert_eq!(chip(&b).as_deref(), Some("+4"));
        let b = run(&p, &mut memo, "guide");
        assert_eq!(memo.evaluations_for_tests(), 12);
        assert_eq!(chip(&b).as_deref(), Some("+2"));
        // a later tag: a new interval, whose booleans the memo already holds
        let chain: Vec<Hash> =
            p.r.read()
                .landings()
                .unwrap()
                .into_iter()
                .map(|(h, _)| h)
                .collect();
        p.r.tag(
            &session(Role::Approver),
            "v1.1.0",
            Some(&chain[2].to_string()),
            None,
        )
        .expect("tag");
        let b = run(&p, &mut memo, "guide");
        assert_eq!(memo.evaluations_for_tests(), 12, "the new interval is held");
        assert_eq!(chip(&b).as_deref(), Some("+1"));
        // and only the new interval's booleans stay
        assert_eq!(memo.results.len(), 2);
        // an evicted target builds cold
        let mut fresh = ProfileHistoryMemo::default();
        let b = run(&p, &mut fresh, "guide");
        assert_eq!(fresh.evaluations_for_tests(), 2);
        assert_eq!(chip(&b).as_deref(), Some("+1"));

        // a new redaction epoch, pack or profile discards the memo before any lookup
        let held = memo.identity.clone().expect("an identity");
        for changed in [
            Identity {
                epoch: held.epoch + 1,
                ..held.clone()
            },
            Identity {
                pack: Hash::of(b"another pack"),
                ..held.clone()
            },
            Identity {
                profile: None,
                ..held.clone()
            },
        ] {
            let mut m = ProfileHistoryMemo::default();
            run(&p, &mut m, "guide");
            assert!(!m.results.is_empty() && m.aggregate.is_some());
            m.begin(changed);
            assert!(m.results.is_empty() && m.aggregate.is_none());
        }
        // the build's own identity keeps it
        let before = memo.results.len();
        memo.begin(held);
        assert_eq!(memo.results.len(), before);

        // a chain that does not extend the saved tip: the memo starts again, never reusing the
        // aggregate
        if let Some(a) = memo.aggregate.as_mut() {
            a.tip = Hash::of(b"a landing of another chain");
        }
        let b = run(&p, &mut memo, "guide");
        assert_eq!(memo.evaluations_for_tests(), 14);
        assert_eq!(chip(&b).as_deref(), Some("+1"));
    }

    /// The object frames a whole build of `key`'s canon route reads, with `memo`.
    fn frames(p: &Project, memo: &mut ProfileHistoryMemo, key: &str) -> (u64, live::ProfileBuild) {
        let st = p.r.read();
        let before = st.reader().frames_read();
        let b = build_profile_live(&st, &canon(key), memo).expect("build");
        (st.reader().frames_read() - before, b)
    }

    /// The frames of a cold build, of an unchanged one and of one after one more landing, for a
    /// tag followed by `n` landings that change what `guide` shows.
    fn reads_after(n: usize) -> (u64, u64, live::ProfileBuild, u64) {
        let mut p = project();
        for i in 0..n {
            p.edit("$ip", &format!("Intro words, {i}."));
        }
        let mut memo = ProfileHistoryMemo::default();
        frames(&p, &mut memo, "guide");
        let (unchanged, _) = frames(&p, &mut memo, "guide");
        p.edit("$ip", "Intro words, appended.");
        let (appended, b) = frames(&p, &mut memo, "guide");
        (unchanged, appended, b, memo.evaluations_for_tests())
    }

    #[test]
    fn spv4_selected_history_memo_reads_only_new_landings_in_the_whole_build() {
        // every read of a whole build, not only its pairs: a rebuild of an unchanged or appended
        // canon reads the same whatever the interval's length (the tag is found incrementally)
        let (u2, a2, b2, e2) = reads_after(2);
        let (u6, a6, b6, e6) = reads_after(6);
        assert_eq!(chip(&b2).as_deref(), Some("+3"));
        assert_eq!(chip(&b6).as_deref(), Some("+7"));
        assert_eq!((e2, e6), (3, 7), "each pair once");
        assert_eq!(u2, u6, "an unchanged rebuild");
        assert_eq!(a2, a6, "an appended rebuild");
        // past the ceiling: an extending chain stays over the limit and is not scanned again
        let over = |n: usize| with_ceiling(2, || reads_after(n));
        let (u4, a4, b4, e4) = over(4);
        let (u8, a8, b8, e8) = over(8);
        assert_eq!((e4, e8), (0, 0), "no pair past the ceiling");
        for b in [&b4, &b8] {
            assert_eq!(chip(b), None);
            assert_eq!(b.live.warnings, vec![HISTORY_WARNING.to_string()]);
        }
        assert_eq!(u4, u8, "an unchanged over-limit rebuild");
        assert_eq!(a4, a8, "an appended over-limit rebuild");
    }

    #[test]
    fn spv4_selected_history_counts_what_redaction_leaves_visible() {
        use rdy_engine::redaction::RedactInput;
        let mut p = project();
        let png = b"\x89PNG\r\n\x1a\nhistory figure bytes".to_vec();
        let blob = p.r.store_mut().put_blob(&png).unwrap();
        let fig = write(
            &mut p.r,
            "f1",
            json!([{"op": "create", "type": "figure", "id": "$fig", "parent": p.ids["$intro"],
                "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "Old alt"}}]),
        )["$fig"]
            .clone();
        land(&mut p.r, "f1");
        p.r.tag(&session(Role::Approver), "v1.1.0", None, None)
            .expect("tag");
        // a landing that edits only the figure's alt text: a reader sees it
        write(
            &mut p.r,
            "f2",
            json!([{"op": "update", "id": fig, "fields": {"alt": "New alt"}}]),
        );
        land(&mut p.r, "f2");
        let mut memo = ProfileHistoryMemo::default();
        let run = |p: &Project, memo: &mut ProfileHistoryMemo| {
            build_profile_live(&p.r.read(), &canon("guide"), memo).expect("build")
        };
        let b = run(&p, &mut memo);
        assert_eq!(chip(&b).as_deref(), Some("+1"));
        assert_eq!(memo.evaluations_for_tests(), 1);
        let epoch = p.r.read().redaction_epoch();

        // a removal of the figure's blob: both sides of the alt edit read as the same notice
        p.r.redact_in(
            &session(Role::Writer),
            "rx",
            RedactInput {
                targets: vec![format!("blob:{blob}")],
                reason_class: "secret".into(),
                reason: "a leaked figure".into(),
                dry_run: false,
            },
        )
        .expect("redact");
        land(&mut p.r, "rx");
        assert!(p.r.read().redaction_epoch() > epoch, "a real epoch change");
        let b = run(&p, &mut memo);
        // the retained memo is discarded for the new epoch, and the alt edit no longer counts
        assert_eq!(chip(&b), None, "{}", b.live.header);
        assert!(!b.live.files.contains_key("changes-since.html"));
        assert_eq!(
            memo.results.get(&p.r.read().canon_head().unwrap()),
            Some(&false)
        );
        let intro = String::from_utf8(b.live.files["intro.html"].clone()).unwrap();
        assert!(intro.contains("Content removed."), "{intro}");
    }

    /// A synthetic landing: `h(i)`'s parent is `h(i - 1)`; `h(0)` is the tag.
    fn h(i: u64) -> Hash {
        Hash::of(&i.to_le_bytes())
    }

    #[test]
    fn spv4_history_ceiling_omits_only_the_chip_and_since_link() {
        let limit = history_facts::MAX_LANDINGS;
        assert_eq!(limit, 100_000);
        let n = limit as u64 + 1;
        let parents: HashMap<Hash, Hash> = (1..=n + 1).map(|i| (h(i), h(i - 1))).collect();
        let prevs = std::cell::Cell::new(0u64);
        let prev = |x: &Hash| -> Result<Option<Hash>, RdyError> {
            prevs.set(prevs.get() + 1);
            Ok(parents.get(x).copied())
        };
        let evals = std::cell::Cell::new(0u64);
        let mut eval = |_: &Hash, after: &Hash| -> Result<bool, RdyError> {
            evals.set(evals.get() + 1);
            Ok(*after != h(7))
        };
        let identity = Identity {
            profile: None,
            epoch: 0,
            pack: Hash::of(b"pack"),
        };
        let tagged = Tags {
            on: HashMap::from([(h(0), "v1.0.0".to_string())]),
            id: Hash::of(b"one tag"),
        };
        let tag = Some(Tagged {
            name: "v1.0.0".into(),
            landing: h(0),
        });
        let counted = |since| Counted {
            tag: tag.clone(),
            since,
        };
        // exactly the ceiling: every pair once
        let mut memo = ProfileHistoryMemo::default();
        memo.begin(identity.clone());
        let got = memo
            .count(h(limit as u64), &tagged, limit, &prev, &mut eval)
            .unwrap();
        assert_eq!(got, counted(Since::Count(limit as u32 - 1)));
        assert_eq!(evals.get(), limit as u64);
        // one more: over the ceiling before any pair is evaluated
        let mut memo = ProfileHistoryMemo::default();
        memo.begin(identity.clone());
        evals.set(0);
        prevs.set(0);
        assert_eq!(
            memo.count(h(n), &tagged, limit, &prev, &mut eval).unwrap(),
            counted(Since::Over)
        );
        assert_eq!(evals.get(), 0);
        assert!(
            prevs.get() <= limit as u64 + 1,
            "{} parents read",
            prevs.get()
        );
        assert!(memo.results.is_empty());
        // an unchanged over-limit tip is not scanned again
        prevs.set(0);
        assert_eq!(
            memo.count(h(n), &tagged, limit, &prev, &mut eval).unwrap(),
            counted(Since::Over)
        );
        assert_eq!((prevs.get(), evals.get()), (0, 0));
        // an extending tip walks only to the saved tip, and stays over
        assert_eq!(
            memo.count(h(n + 1), &tagged, limit, &prev, &mut eval)
                .unwrap(),
            counted(Since::Over)
        );
        assert_eq!((prevs.get(), evals.get()), (1, 0));
        // no version on a chain past the ceiling: nothing to count and no warning, found once;
        // an extending chain is not scanned again
        let untagged = Tags {
            on: HashMap::new(),
            id: Hash::of(b"no tag"),
        };
        let none = Counted {
            tag: None,
            since: Since::Count(0),
        };
        let mut memo = ProfileHistoryMemo::default();
        memo.begin(identity.clone());
        prevs.set(0);
        assert_eq!(
            memo.count(h(n), &untagged, limit, &prev, &mut eval)
                .unwrap(),
            none
        );
        assert_eq!((prevs.get(), evals.get()), (n + 1, 0));
        prevs.set(0);
        assert_eq!(
            memo.count(h(n + 1), &untagged, limit, &prev, &mut eval)
                .unwrap(),
            none
        );
        assert_eq!((prevs.get(), evals.get()), (1, 0));
        // a read failure never becomes a cached boolean
        let mut memo = ProfileHistoryMemo::default();
        memo.begin(identity);
        let mut failing = |_: &Hash, _: &Hash| -> Result<bool, RdyError> {
            Err(RdyError::new(E_LIMIT, "boom", "fix"))
        };
        memo.count(h(3), &tagged, limit, &prev, &mut failing)
            .unwrap_err();
        assert!(memo.results.is_empty() && memo.aggregate.is_none());

        // a real build under a lowered ceiling: the chip, the since page and its link go; the
        // warning is the exact one, and the page builds
        let mut p = project();
        p.edit("$ip", "Intro words, one.");
        p.edit("$ip", "Intro words, two.");
        let at = |p: &Project| {
            with_ceiling(2, || {
                build_profile_live(
                    &p.r.read(),
                    &canon("guide"),
                    &mut ProfileHistoryMemo::default(),
                )
            })
            .expect("build")
        };
        let b = at(&p);
        assert_eq!(chip(&b).as_deref(), Some("+2"));
        assert!(b.live.files.contains_key("changes-since.html"));
        assert_eq!(b.changes, Some(SINCE_PAGE));
        assert!(b.live.warnings.is_empty(), "{:?}", b.live.warnings);
        p.edit("$ip", "Intro words, three.");
        let b = at(&p);
        assert_eq!(chip(&b), None);
        assert!(!b.live.files.contains_key("changes-since.html"));
        assert_eq!(b.changes, None);
        assert!(
            !b.live.header.contains("changes-since"),
            "{}",
            b.live.header
        );
        assert_eq!(
            b.live.warnings,
            vec![
                "Profile history exceeds 100000 landings. The landings chip and since link are omitted. Open a tagged profile scope to read that version."
                    .to_string()
            ]
        );
        assert!(
            b.live
                .header
                .contains("<span class=\"pill-text\">v1.0.0 · Guide</span>")
        );
        let intro = String::from_utf8(b.live.files["intro.html"].clone()).unwrap();
        assert!(intro.contains("Intro words, three."));
    }

    #[test]
    fn spv4_profile_live_budget_fails_before_growth() {
        let mut p = project();
        let png_a = b"\x89PNG\r\n\x1a\nold side figure bytes".to_vec();
        let png_b = b"\x89PNG\r\n\x1a\nthe new side figure, a little longer".to_vec();
        let blob_a = p.r.store_mut().put_blob(&png_a).unwrap();
        let blob_b = p.r.store_mut().put_blob(&png_b).unwrap();
        let fig = write(
            &mut p.r,
            "f1",
            json!([{"op": "create", "type": "figure", "id": "$fa", "parent": p.ids["$intro"],
                "fields": {"blob": blob_a.to_string(), "media_type": "image/png", "alt": "Old figure"}}]),
        );
        land(&mut p.r, "f1");
        // the review removes the old figure and adds the new one
        write(
            &mut p.r,
            "work",
            json!([
                {"op": "delete", "id": fig["$fa"]},
                {"op": "create", "type": "figure", "parent": p.ids["$intro"],
                 "fields": {"blob": blob_b.to_string(), "media_type": "image/png", "alt": "New figure"}}
            ]),
        );
        let st = p.r.read();
        let target = SiteTarget::Profile {
            scope: ScopeRef::View("work".into()),
            key: "guide".into(),
        };
        let within = |b: &Budget| build_within(&st, &target, &mut ProfileHistoryMemo::default(), b);
        let full = within(&Budget::publication()).expect("build");
        let files = full.live.files.len();
        let total: u64 = full.live.files.values().map(|f| f.len() as u64).sum();
        assert!(files > 5, "{files}");
        // the exact file count and total bytes build; one fewer refuses
        within(&Budget::new(files, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES)).expect("exact files");
        let e = within(&Budget::new(files - 1, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES))
            .err()
            .expect("refused");
        over(&e, format!("{} files", files - 1));
        within(&Budget::new(MAX_FILES, MAX_FILE_BYTES, total)).expect("exact bytes");
        let b = Budget::new(MAX_FILES, MAX_FILE_BYTES, total - 1);
        let e = within(&b).err().expect("refused");
        over(&e, format!("{} artifact bytes", total - 1));
        assert!(b.admitted() < total);
        // the old and new figures together are bounded before any of their bytes is read
        let both = (png_a.len() + png_b.len()) as u64;
        let reads = rdy_engine::publication::blob_reads_for_tests();
        let e = within(&Budget::new(MAX_FILES, MAX_FILE_BYTES, both - 1))
            .err()
            .expect("refused");
        over(&e, format!("{} artifact bytes", both - 1));
        let largest = png_b.len() as u64;
        let e = within(&Budget::new(MAX_FILES, largest - 1, MAX_ARTIFACT_BYTES))
            .err()
            .expect("refused");
        over(&e, format!("{} file bytes", largest - 1));
        assert_eq!(rdy_engine::publication::blob_reads_for_tests(), reads);
    }
}
