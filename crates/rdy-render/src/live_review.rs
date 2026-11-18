//! Review targets on the live site, rendered on request (D-F44G6, refined by D-WYZ97 and
//! D-SMP5A): a view, a proposal version, or a profile route under one. Nothing here touches the
//! file system or keeps a store reader.
//!
//! Three operations, each separate:
//! - [`prepare_review`] and [`prepare_profile_review`] read one captured target state into a
//!   [`PreparedReview`]: its detached render inputs and the facts every file shares.
//! - [`PreparedReview::render`] renders one requested file of that state, by the path rules and
//!   to the bytes of the target's whole build.
//! - [`PreparedReview::check`] runs the complete warnings pass of that state.
//!
//! A review target has the manifest's file count and per-file limits and no artifact limit
//! (D-SMP5A): its preparation counts the files it can serve, and each requested file is held to
//! the per-file limit on its own. Nothing adds up the target's files.
use crate::live::{self, Facts, ProfileHistoryMemo};
use crate::page::{self, Kind};
use crate::prepare::{self, Detached};
use crate::retained::{self, ARC_HEADER, RetainedAllocations};
use crate::{
    Budget, Mode, NotFoundKind, Slot, SlotKind, Slots, assets, build_page, keep_for_llms,
    live_not_found, llms, one_page, page_kinds, profile_live, render_ctx, search, with_inputs,
};
use rdy_engine::publication::ResolvedProfile;
use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, ScopeRef};
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{RdyError, ReadState, Scope};
use rdy_model::{Block, Hash};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::Arc;

/// The detached render preparation of one captured review target state. Clones share it: the
/// site's ledger counts it once per recount ([`PreparedReview::retained_bytes`]).
#[derive(Clone)]
pub struct PreparedReview {
    inner: Arc<Inner>,
}

struct Inner {
    d: Detached,
    /// The target's root, where every link leads.
    root: String,
    /// Where the pill opens the menu.
    menu_url: Option<String>,
    /// The redaction epoch the state captured.
    epoch: u64,
    label: String,
    header: String,
    label_rev: String,
    /// The files the target can serve, as counted when it was prepared.
    files: usize,
    profile: Option<ResolvedProfile>,
    changes: Option<&'static str>,
    /// The retained estimate of what this preparation alone holds, and of its shared parts (the
    /// selected blocks, the graph, a profile's old side), computed once when it was prepared.
    own: u64,
    m_bytes: u64,
    g_bytes: u64,
    old_bytes: u64,
}

/// One requested file of a review target, as its whole build would write it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewFile {
    /// The file's bytes, the label and header slots unfilled.
    pub bytes: Vec<u8>,
    /// Where the file holds the label and the header.
    pub label_at: Vec<Slot>,
    /// The page's revision: none for a permalink page and any file but a page.
    pub rev: Option<String>,
    /// The warnings of the diagrams this page could not draw.
    pub warnings: Vec<String>,
    /// The redaction epoch the target's state captured.
    pub epoch: u64,
}

/// The complete warnings pass of a review target state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewCheck {
    /// The warnings of the target's whole build, in its order.
    pub warnings: Vec<String>,
    pub epoch: u64,
}

/// Prepares the review target of a source view or proposal version (`target`, whose scope
/// resolved to `scope`): what [`live::build_live`] reads, with each shown figure measured by its
/// manifest and never read, and the count of the files it can serve. Any other target is
/// refused with the invalid-target error.
pub fn prepare_review(
    st: &ReadState,
    scope: &Scope,
    target: &SiteTarget,
) -> Result<PreparedReview, RdyError> {
    rdy_engine::refuse_inflight_export(scope)?;
    target.validate()?;
    if !matches!(
        target,
        SiteTarget::Source(ScopeRef::View(_) | ScopeRef::Proposal(_))
    ) {
        return Err(profile_live::invalid_target());
    }
    let budget = Budget::per_file();
    let prepared = prepare::source_review(st, scope)?;
    let changes = prepared.history.changes.as_ref().map(|_| CHANGES_PAGE);
    detach(st, prepared, target, None, changes, &budget)
}

/// Prepares the review target of a profile route under a view or a proposal version: what
/// [`live::build_profile_live`] reads, with its selection, attachment and path bounds and its
/// errors, each figure held to the per-file limit alone and none kept. `history` is the target's
/// memo of the neutral chip. Any other target is refused with the invalid-target error.
pub fn prepare_profile_review(
    st: &ReadState,
    target: &SiteTarget,
    history: &mut ProfileHistoryMemo,
) -> Result<PreparedReview, RdyError> {
    target.validate()?;
    if !matches!(
        target,
        SiteTarget::Profile {
            scope: ScopeRef::View(_) | ScopeRef::Proposal(_),
            ..
        }
    ) {
        return Err(profile_live::invalid_target());
    }
    let budget = Budget::per_file();
    let (prepared, profile, changes) = profile_live::prepare_review(st, target, history, &budget)?;
    detach(st, prepared, target, Some(profile), changes, &budget)
}

/// The prepared review of `prepared`: its detached inputs, its label and header facts, its file
/// count held to `budget`, and its retained estimate.
fn detach(
    st: &ReadState,
    mut prepared: prepare::Prepared,
    target: &SiteTarget,
    profile: Option<ResolvedProfile>,
    changes: Option<&'static str>,
    budget: &Budget,
) -> Result<PreparedReview, RdyError> {
    prepared.menu_url = target.menu_url();
    let root = target.root();
    let (d, scoped) = prepared.split();
    let menu_url = scoped.menu_url;
    let (label, header, files) = with_inputs(
        &d,
        Mode::Live { root: &root },
        menu_url.as_deref(),
        None,
        |sh, _, extras| Ok((sh.label().text, sh.header_html(""), files_of(sh, &extras))),
    )?;
    budget.count(files)?;
    let label_rev = live::label_rev(&label, &header);
    let m_bytes = retained::materialized(&d.m);
    let g_bytes = retained::graph(&d.g, &d.m);
    let old_bytes = d.history.old.as_deref().map_or(0, retained::materialized);
    let mut inner = Inner {
        d,
        root,
        menu_url,
        epoch: st.redaction_epoch(),
        label,
        header,
        label_rev,
        files,
        profile,
        changes,
        own: 0,
        m_bytes,
        g_bytes,
        old_bytes,
    };
    inner.own = own_bytes(&inner);
    Ok(PreparedReview {
        inner: Arc::new(inner),
    })
}

/// How many files the whole build of the target can write, counted from its inputs without a
/// page body: its pages and change summaries, its permalink pages, the agent and search files,
/// the theme, each figure file a shown figure may name and each diagram source that may draw.
/// The count is never below the whole build's.
fn files_of(sh: &page::Shared<'_>, extras: &crate::Extras) -> usize {
    let (site, m) = (sh.site, sh.m);
    let pages = page_kinds(site, m).len()
        + usize::from(extras.changes.is_some())
        + usize::from(extras.since.is_some());
    let permalinks = m
        .iter()
        .filter(|b| !crate::site::is_profile(b) && site.url_of(&b.id).is_some())
        .count();
    let theme = assets::names().count();
    // a figure file for each shown blob and media type a figure names with it, on a page or on
    // a removed line
    let mut figures: BTreeSet<String> = BTreeSet::new();
    let mut figure = |b: &Block| {
        let media: Vec<&str> = rdy_schema::ASSET_MEDIA_TYPE_FIELDS
            .iter()
            .map(|f| b.field_str(f).unwrap_or(""))
            .collect();
        for h in rdy_engine::reader::review::figure_blobs(b) {
            if sh.blob_lens.contains_key(&h) && !sh.removed_blobs.contains_key(&h) {
                for media in &media {
                    figures.insert(assets::blob_path(&h.to_string(), media));
                }
            }
        }
    };
    m.iter().for_each(&mut figure);
    if let Some(old) = sh.old_site {
        old.m.iter().for_each(&mut figure);
    }
    if let Some(r) = sh.listed {
        for id in m.ids() {
            for line in r.removed_in(id) {
                line.blocks.iter().for_each(&mut figure);
            }
        }
        r.removed_pages().iter().for_each(&mut figure);
    }
    // a diagram file for each distinct source
    let diagrams: BTreeSet<&str> = m
        .iter()
        .filter(|b| b.block_type == "diagram")
        .map(|b| b.body.as_str())
        .collect();
    pages + permalinks + 3 + theme + figures.len() + diagrams.len()
}

/// The retained estimate of what `i` alone holds: its own facts and the inputs no other target
/// shares (D-WYZ97, the F3 accounting ruling).
fn own_bytes(i: &Inner) -> u64 {
    use retained::{btree, btree_set, data, id, opt_string, string, vec};
    let d = &i.d;
    let h = &d.history;
    let mut n = size_of::<Inner>() as u64;
    n += string(&i.root) + opt_string(&i.menu_url);
    n += string(&i.label) + string(&i.header) + string(&i.label_rev);
    n += data(&d.pack);
    n += btree(&d.blobs.lens, |_, _| 0) + btree(&d.blobs.removed, |_, _| 0);
    n += string(&d.site_name) + opt_string(&d.description);
    n += data(&d.position) + vec(&d.versions, data);
    n += btree(&d.landed_ms, |k, _| string(k));
    n += vec(&d.disagreements, |x| {
        vec(&x.blocks, id) + x.lead.as_ref().map_or(0, id)
    });
    n += h.listed.as_ref().map_or(0, |r| retained::review(r, &d.m));
    n += h.reviewed.as_ref().map_or(0, data);
    n += btree(&h.inline_diffs, |k, v| id(k) + string(v));
    n += h.changes.as_ref().map_or(0, data) + h.since.as_ref().map_or(0, data);
    n += vec(&h.warnings, string);
    if let Some(p) = &d.profile {
        n += opt_string(&p.tag_name) + btree_set(&p.dropped_decisions, id);
        n += match &p.presentation {
            prepare::Presentation::LiveRoute { header, colophon } => {
                data(header) + string(colophon)
            }
            prepare::Presentation::LiveOnly { colophon } => string(colophon),
            prepare::Presentation::Output => 0,
        };
    }
    n += btree(&d.status, |k, v| id(k) + opt_string(&v.since));
    if let Some(p) = &i.profile {
        n += size_of::<ResolvedProfile>() as u64
            + opt_string(&p.key)
            + string(&p.title)
            + opt_string(&p.description)
            + p.id.as_ref().map_or(0, id);
    }
    n
}

/// The change summary page `path` names in `extras`, if the target has it.
fn summary_at<'p>(extras: &crate::Extras<'p>, path: &str) -> Option<Kind<'p>> {
    match path {
        CHANGES_PAGE => extras.changes.map(Kind::Changes),
        SINCE_PAGE => extras.since.map(Kind::ChangesSince),
        _ => None,
    }
}

/// The pages of a whole build in the order it builds them: its pages, then its change
/// summaries.
fn all_kinds<'p>(sh: &page::Shared<'p>, extras: &crate::Extras<'p>) -> Vec<Kind<'p>> {
    let mut kinds = page_kinds(sh.site, sh.m);
    kinds.extend(extras.changes.map(Kind::Changes));
    kinds.extend(extras.since.map(Kind::ChangesSince));
    kinds
}

/// What an asset path under `assets/` names: a diagram's drawing, or a figure's blob.
enum Asset {
    Diagram(Hash),
    Figure(Hash),
}

fn asset_of(path: &str) -> Option<Asset> {
    let name = path.strip_prefix("assets/")?;
    if let Some(svg) = name.strip_prefix("diagrams/") {
        return svg.strip_suffix(".svg")?.parse().ok().map(Asset::Diagram);
    }
    let (hex, ext) = name.split_once('.')?;
    if ext.is_empty() || ext.contains(['.', '/']) {
        return None;
    }
    hex.parse().ok().map(Asset::Figure)
}

impl PreparedReview {
    /// The bytes this preparation keeps, by the retained estimators ([`crate::retained`]). An
    /// allocation `seen` already counted in this recount (this target through a clone, or the
    /// blocks and graph another target of the same state shares) is not counted again.
    pub fn retained_bytes(&self, seen: &mut RetainedAllocations) -> u64 {
        if !seen.first(&self.inner) {
            return 0;
        }
        let i = &*self.inner;
        let mut n = ARC_HEADER + i.own;
        if seen.first(&i.d.m) {
            n += i.m_bytes;
        }
        if seen.first(&i.d.g) {
            n += i.g_bytes;
        }
        if let Some(old) = &i.d.history.old
            && seen.first(old)
        {
            n += i.old_bytes;
        }
        n
    }

    /// The redaction epoch the target's state captured.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// The version label's text, as the files write it.
    pub fn label(&self) -> &str {
        &self.inner.label
    }

    /// The header's version markup.
    pub fn header(&self) -> &str {
        &self.inner.header
    }

    /// The revision of the label and the header together.
    pub fn label_rev(&self) -> &str {
        &self.inner.label_rev
    }

    /// The warnings the preparation found: the whole build's, before any diagram's.
    pub fn warnings(&self) -> &[String] {
        &self.inner.d.history.warnings
    }

    /// The profile a profile route resolved to; none for a source review.
    pub fn profile(&self) -> Option<&ResolvedProfile> {
        self.inner.profile.as_ref()
    }

    /// The change summary's page the target has, if any.
    pub fn changes(&self) -> Option<&'static str> {
        self.inner.changes
    }

    /// How many files the target can serve, as counted when it was prepared.
    pub fn files(&self) -> usize {
        self.inner.files
    }

    /// The budget of one request: the manifest's file count and per-file limits, no artifact
    /// limit.
    fn budget(&self) -> Budget {
        Budget::per_file()
    }

    /// `f` with the shared setup of this target over `st`'s diagram cache.
    fn with<R>(
        &self,
        st: Option<&ReadState>,
        f: impl FnOnce(
            &page::Shared<'_>,
            &minijinja::Environment<'static>,
            crate::Extras,
        ) -> Result<R, RdyError>,
    ) -> Result<R, RdyError> {
        let i = &*self.inner;
        with_inputs(
            &i.d,
            Mode::Live { root: &i.root },
            i.menu_url.as_deref(),
            st.map(|st| st.reader().diagram_cache()),
            f,
        )
    }

    fn file(&self, bytes: Vec<u8>, label_at: Vec<Slot>, rev: Option<String>) -> ReviewFile {
        ReviewFile {
            bytes,
            label_at,
            rev,
            warnings: vec![],
            epoch: self.inner.epoch,
        }
    }

    /// The file the target's whole build writes at `path`, rendered alone from `st` (the state
    /// the target was prepared from): a theme file, a figure or diagram file, the search index,
    /// an llms file, a permalink page, a change summary or a page. Only that file is rendered;
    /// a page builds its own context alone, and an asset or an agent file renders no page. The
    /// file is held to the per-file limit. None for a path the whole build has no file at.
    pub fn render(&self, st: &ReadState, path: &str) -> Result<Option<ReviewFile>, RdyError> {
        let budget = self.budget();
        if let Some(bytes) = assets::file(path) {
            budget.admit(0, bytes.len() as u64)?;
            return Ok(Some(self.file(bytes, vec![], None)));
        }
        if let Some(asset) = asset_of(path) {
            return self.asset(st, path, asset, &budget);
        }
        if matches!(path, "search-index.json" | "llms.txt" | "llms-full.txt") {
            return self.aux(st, path, &budget).map(Some);
        }
        if !path.ends_with(".html") {
            return Ok(None);
        }
        self.with(Some(st), |sh, env, extras| {
            // a permalink page replaces a page at its path; a change summary replaces a page
            if let Some((id, url)) = one_page::permalink(sh, path) {
                let ctx = build_page(sh, Kind::Redirect(&id, &url));
                let t = crate::RedirectTemplate::new(env, ctx, Slots::Source)?;
                let (_, label, href) = page::redirect_target(sh, &id, &url);
                budget.admit(0, t.filled_len(&label, &href) as u64)?;
                let (html, _) = t.fill_at(&label, &href);
                return Ok(Some(self.file(html.into_bytes(), vec![], None)));
            }
            let kind = match summary_at(&extras, path) {
                Some(kind) => kind,
                None => match one_page::page_at(sh, path) {
                    Some(kind) => kind,
                    None => return Ok(None),
                },
            };
            let ctx = build_page(sh, kind);
            let (_, bytes, slots, _) = render_ctx(env, Slots::Source, Some(&budget), &ctx)?;
            let rev = (!live::is_redirect(&bytes)).then(|| live::page_rev(&bytes, &slots));
            let mut file = self.file(bytes, slots, rev);
            file.warnings = sh.diagrams.take().warnings;
            Ok(Some(file))
        })
    }

    /// A figure or diagram file: found among the files the pages name, page by page, with no
    /// page rendered. A figure's bytes are read from `st` once its length is within the limit.
    fn asset(
        &self,
        st: &ReadState,
        path: &str,
        asset: Asset,
        budget: &Budget,
    ) -> Result<Option<ReviewFile>, RdyError> {
        let lens = &self.inner.d.blobs;
        if let Asset::Figure(h) = &asset
            && (!lens.lens.contains_key(h) || lens.removed.contains_key(h))
        {
            return Ok(None);
        }
        let found = self.with(Some(st), |sh, _, extras| {
            for kind in all_kinds(sh, &extras) {
                drop(build_page(sh, kind));
                match &asset {
                    Asset::Diagram(h) => {
                        if let Some(bytes) = sh.diagrams.borrow().file(h) {
                            return Ok(Some(bytes));
                        }
                    }
                    Asset::Figure(h) => {
                        if sh
                            .used_blobs
                            .borrow()
                            .get(path)
                            .is_some_and(|(u, _)| u == h)
                        {
                            return Ok(Some(vec![]));
                        }
                    }
                }
            }
            Ok(None)
        })?;
        let Some(bytes) = found else { return Ok(None) };
        let bytes = match asset {
            Asset::Diagram(_) => bytes,
            Asset::Figure(h) => {
                budget.holds_file(lens.lens.get(&h).copied().unwrap_or(0))?;
                st.reader().get_blob(&h).map_err(RdyError::from)?
            }
        };
        budget.admit(0, bytes.len() as u64)?;
        Ok(Some(self.file(bytes, vec![], None)))
    }

    /// The search index or an llms file: every page's context made, and no page rendered. The
    /// search index collects each page's entries, held to the per-file limit while they are
    /// collected; an llms file reads the contexts as the whole build keeps them for it, and is
    /// held to that limit while it is written.
    fn aux(&self, st: &ReadState, path: &str, budget: &Budget) -> Result<ReviewFile, RdyError> {
        let index_file = path == "search-index.json";
        let (bytes, at) =
            self.with(Some(st), |sh, _, _| {
                let (cap, limit) = budget.file_cap();
                let mut index =
                    search::Index::new(sh.site_name, if index_file { cap } else { u64::MAX });
                let mut contexts = vec![];
                for kind in page_kinds(sh.site, sh.m) {
                    let mut ctx = build_page(sh, kind);
                    if index_file {
                        page::finish(sh, &ctx, &mut index);
                        if index.over() {
                            return Err(budget.over(limit.clone()));
                        }
                    } else {
                        keep_for_llms(&mut ctx);
                        contexts.push(ctx);
                    }
                }
                let label = sh.label();
                let none = BTreeMap::new();
                match path {
                    "search-index.json" => crate::aux_file(Some(budget), &none, path, |room| {
                        match index.len() <= u64::try_from(room).unwrap_or(u64::MAX) {
                            true => Ok((search::bytes(&index), 0..0)),
                            false => Err(llms::Over { held: 0 }),
                        }
                    }),
                    "llms.txt" => crate::aux_file(Some(budget), &none, path, |room| {
                        llms::index_bytes(
                            sh.site,
                            &contexts,
                            sh.site_name,
                            sh.description,
                            &label,
                            room,
                        )
                    }),
                    _ => crate::aux_file(Some(budget), &none, path, |room| {
                        llms::full_bytes(&contexts, sh.site_name, sh.description, &label, room)
                    }),
                }
            })?;
        let slots = match at.is_empty() {
            true => vec![],
            false => vec![(SlotKind::Label, at)],
        };
        Ok(self.file(bytes, slots, None))
    }

    /// The complete warnings pass of the target's state: the preparation's warnings, then those
    /// of every diagram the pages could not draw, in page and block order, as the whole build
    /// gives them. Every page's context is made; no page is rendered and none is kept.
    pub fn check(&self, st: &ReadState) -> Result<ReviewCheck, RdyError> {
        let warnings = self.with(Some(st), |sh, _, extras| {
            let mut warnings = extras.warnings.clone();
            for kind in all_kinds(sh, &extras) {
                drop(build_page(sh, kind));
            }
            warnings.extend(sh.diagrams.take().warnings);
            Ok(warnings)
        })?;
        Ok(ReviewCheck {
            warnings,
            epoch: self.inner.epoch,
        })
    }

    /// The target's own not-found page of `kind`, built from its prepared state: the Page
    /// variant, rooted at the target's root. A review target has no Scope variant, and a page
    /// that cannot be built is none.
    pub fn not_found(&self, kind: NotFoundKind) -> Option<Vec<u8>> {
        match kind {
            NotFoundKind::Page => self
                .with(None, |sh, env, _| {
                    Ok(live_not_found(sh, env, &self.inner.root, kind))
                })
                .ok()
                .flatten(),
            NotFoundKind::Scope => None,
        }
    }

    /// The bytes the server sends for `file` at `path`: its label and header filled in, and a
    /// page's live script with `target`'s event query ([`live::page`]).
    pub fn serve(&self, file: &ReviewFile, path: &str, target: &SiteTarget) -> Vec<u8> {
        let i = &*self.inner;
        let facts = Facts {
            label: &i.label,
            header: &i.header,
            label_rev: &i.label_rev,
        };
        live::fill(
            &file.bytes,
            path,
            &file.label_at,
            file.rev.as_ref(),
            &facts,
            target,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget_hooks::{self, Limits};
    use crate::live::{self, LiveBuild};
    use crate::{
        Budget, Mode, NotFoundOut, Slots, build_site_from, page_builds, page_renders, prepare,
    };
    use rdy_engine::publication::ReadPublicationSource;
    use rdy_engine::publication_manifest::{MAX_ARTIFACT_BYTES, MAX_FILE_BYTES, MAX_FILES};
    use rdy_engine::render_inputs::RenderInputs;
    use rdy_engine::site_scope::{ScopeRef, proposal_ref};
    use rdy_engine::{E_LIMIT, Rdy, Session, WriteRequest};
    use rdy_model::{Hash, Role};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

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

    /// A PNG's bytes of `n` bytes, starting with `tag`.
    fn png(tag: &str, n: usize) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend_from_slice(tag.as_bytes());
        b.resize(n, b'.');
        b
    }

    struct Fx {
        _dir: tempfile::TempDir,
        r: Rdy,
        ids: BTreeMap<String, String>,
        /// The open proposal's version, `P-<hex>@1`.
        proposal: String,
        proposal_scope: Scope,
        /// The old figure (canon) and the new one (the view), each `FIG` bytes.
        old_fig: Vec<u8>,
        new_fig: Vec<u8>,
    }

    /// Each figure's bytes.
    const FIG: usize = 3000;

    /// A project with a folder, docs, a term, an open question, a drawn diagram, a diagram that
    /// does not draw, a light and dark figure, a top-level doc named glossary (a scope warning)
    /// and the profile `guide` (overview and setup, not the broken doc), landed and tagged
    /// `v1.0.0`; then the view `work`, which edits, adds, removes and changes the figure, and
    /// the proposal of the view `prop`.
    fn fixture() -> Fx {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
        let old_fig = png("old side figure", FIG);
        let new_fig = png("new side figure", FIG);
        let dark = png("dark figure", 200);
        let old_blob = r.store_mut().put_blob(&old_fig).unwrap();
        let new_blob = r.store_mut().put_blob(&new_fig).unwrap();
        let dark_blob = r.store_mut().put_blob(&dark).unwrap();
        let mut ids = write(
            &mut r,
            "w1",
            json!([
                {"op": "create", "type": "doc", "id": "$ov", "fields": {"name": "overview", "title": "Overview", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$op", "parent": "$ov", "body": "Overview words."},
                {"op": "create", "type": "prose", "id": "$gone", "parent": "$ov", "body": "GONEWORDS to remove."},
                {"op": "create", "type": "section", "id": "$os", "parent": "$ov", "fields": {"title": "Words"}},
                {"op": "create", "type": "term", "id": "$t", "parent": "$os", "fields": {"name": "Ledger"}, "body": "Balances."},
                {"op": "create", "type": "question", "id": "$q", "parent": "$os", "fields": {"prompt": "Which ledger?"}},
                {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "guides", "title": "Guides"}},
                {"op": "create", "type": "doc", "id": "$setup", "parent": "$g", "fields": {"name": "setup", "title": "Setup", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$sp", "parent": "$setup", "body": "Setup words."},
                {"op": "create", "type": "diagram", "id": "$dg", "parent": "$setup", "fields": {"lang": "mermaid", "alt": "flow"}, "body": "graph TD\n  A --> B"},
                {"op": "create", "type": "figure", "id": "$fig", "parent": "$setup",
                 "fields": {"blob": old_blob.to_string(), "media_type": "image/png", "alt": "Old figure",
                            "dark_blob": dark_blob.to_string(), "dark_media_type": "image/png"}},
                {"op": "create", "type": "doc", "id": "$broken", "fields": {"name": "broken", "title": "Broken", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$bp", "parent": "$broken", "body": "BROKENWORDS here."},
                {"op": "create", "type": "diagram", "id": "$bd", "parent": "$broken", "fields": {"lang": "mermaid", "alt": "broken"}, "body": "flowchart LR\n H[broken"},
                {"op": "create", "type": "doc", "id": "$gl", "fields": {"name": "glossary", "title": "Glossary doc", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
            ]),
        );
        land(&mut r, "w1");
        let p = write(
            &mut r,
            "w2",
            json!([
                {"op": "create", "type": "publication_profile", "id": "$guide", "parent": ids["$admin"],
                 "fields": {"key": "guide", "title": "Guide", "audience": "org", "select": [ids["$ov"], ids["$g"]]}}
            ]),
        );
        ids.extend(p);
        land(&mut r, "w2");
        r.tag(&session(Role::Approver), "v1.0.0", None, None)
            .expect("tag");
        // the view: an edit with a reference outside the profile, a new doc, a removed line, the
        // old figure replaced by the new one
        let w = write(
            &mut r,
            "work",
            json!([
                {"op": "update", "id": ids["$op"], "body": format!("Overview words, NEWWORDS. See {{{{{}}}}}.", ids["$bp"])},
                {"op": "delete", "id": ids["$gone"]},
                {"op": "create", "type": "doc", "id": "$added", "parent": ids["$g"], "fields": {"name": "added", "title": "Added", "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$added", "body": "Added words."},
                {"op": "delete", "id": ids["$fig"]},
                {"op": "create", "type": "figure", "parent": ids["$setup"],
                 "fields": {"blob": new_blob.to_string(), "media_type": "image/png", "alt": "New figure",
                            "dark_blob": dark_blob.to_string(), "dark_media_type": "image/png"}}
            ]),
        );
        ids.extend(w);
        write(
            &mut r,
            "prop",
            json!([{"op": "update", "id": ids["$sp"], "body": "Setup words, PROPWORDS."}]),
        );
        let p = r
            .propose(&session(Role::Writer), "prop", "Prop", Some("test summary"))
            .expect("propose");
        let proposal = proposal_ref(&p.r#ref).expect("ref");
        let proposal_scope = Scope::Proposal(p.id.parse::<Hash>().expect("hash"));
        Fx {
            _dir: dir,
            r,
            ids,
            proposal,
            proposal_scope,
            old_fig,
            new_fig,
        }
    }

    fn view() -> (Scope, SiteTarget) {
        (
            Scope::View("work".into()),
            SiteTarget::Source(ScopeRef::View("work".into())),
        )
    }

    fn profile(scope: ScopeRef) -> SiteTarget {
        SiteTarget::Profile {
            scope,
            key: "guide".into(),
        }
    }

    fn whole_profile(st: &ReadState, target: &SiteTarget) -> LiveBuild {
        live::build_profile_live(st, target, &mut ProfileHistoryMemo::default())
            .expect("whole profile build")
            .live
    }

    fn prep_profile(st: &ReadState, target: &SiteTarget) -> PreparedReview {
        prepare_profile_review(st, target, &mut ProfileHistoryMemo::default())
            .expect("profile preparation")
    }

    /// The HTML pages of a whole build.
    fn html(b: &LiveBuild) -> Vec<String> {
        b.files
            .keys()
            .filter(|p| p.ends_with(".html"))
            .cloned()
            .collect()
    }

    fn text(f: &ReviewFile) -> String {
        String::from_utf8(f.bytes.clone()).expect("utf-8")
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

    /// `f` with the publication and review limits at `limits` on this thread.
    fn within<T>(limits: Limits, f: impl FnOnce() -> T) -> T {
        budget_hooks::with_limits(limits, f)
    }

    /// Checks that `file` is the whole build's file at `path`: its bytes, slots and revision,
    /// and the bytes the server sends for it.
    #[track_caller]
    fn same_as_whole(
        prep: &PreparedReview,
        file: &ReviewFile,
        whole: &LiveBuild,
        path: &str,
        target: &SiteTarget,
    ) {
        assert_eq!(
            file.bytes, whole.files[path],
            "{path}: the bytes the whole build writes"
        );
        assert_eq!(
            file.label_at,
            whole.label_at.get(path).cloned().unwrap_or_default(),
            "{path}: slots"
        );
        assert_eq!(file.rev.as_ref(), whole.revs.get(path), "{path}: revision");
        assert_eq!(
            Some(prep.serve(file, path, target)),
            live::page(whole, path, target),
            "{path}: served bytes"
        );
    }

    /// A source view and a proposal version render one requested body each: every page of the
    /// whole build (home, folder, doc, glossary, questions, permalinks, change summary) by the
    /// same path rules and to the same bytes, each with one page context and one template.
    #[test]
    fn sod1_one_requested_file_builds_only_that_output() {
        let fx = fixture();
        let st = fx.r.read();
        let (scope, target) = view();
        let proposal = SiteTarget::Source(ScopeRef::Proposal(fx.proposal.clone()));
        for (scope, target) in [(scope, target), (fx.proposal_scope.clone(), proposal)] {
            let whole = live::build_live(&st, &scope, &target).expect("whole");
            let prep = prepare_review(&st, &scope, &target).expect("prepare");
            let pages = html(&whole);
            for must in [
                "index.html",
                "guides/index.html",
                "guides/setup.html",
                "glossary.html",
                "questions.html",
                "changes.html",
            ] {
                assert!(pages.iter().any(|p| p == must), "{must} in {pages:?}");
            }
            assert!(pages.iter().any(|p| p.starts_with("go/")), "{pages:?}");
            assert_eq!(prep.epoch(), whole.epoch);
            for path in &pages {
                let (builds, renders) = (page_builds(), page_renders());
                let file = prep.render(&st, path).expect("render").expect("a file");
                assert_eq!(page_builds() - builds, 1, "{path}: one page context");
                assert_eq!(page_renders() - renders, 1, "{path}: one template");
                assert_eq!(file.epoch, whole.epoch);
                same_as_whole(&prep, &file, &whole, path, &target);
            }
            // a path the target has no file at builds nothing
            let (builds, renders) = (page_builds(), page_renders());
            for path in [
                "nope.html",
                "go/nope.html",
                "guides/nope.html",
                "assets/nope.css",
            ] {
                assert_eq!(prep.render(&st, path).expect("render"), None, "{path}");
            }
            assert_eq!((page_builds(), page_renders()), (builds, renders));
        }
    }

    /// Repeated requests reuse one preparation: no new preparation, the same label and header
    /// facts and change marks, and an unchanged retained estimate. A new state gets a new
    /// preparation. Two preparations of one state share its selected blocks and graph, which a
    /// recount counts once.
    #[test]
    fn sod1_shared_parts_are_prepared_once_per_state() {
        let mut fx = fixture();
        let st = fx.r.read();
        let (scope, target) = view();
        let whole = live::build_live(&st, &scope, &target).expect("whole");
        let prepared = crate::probe::preparations();
        let prep = prepare_review(&st, &scope, &target).expect("prepare");
        assert_eq!(crate::probe::preparations() - prepared, 1);
        let before = prep.retained_bytes(&mut RetainedAllocations::new());
        assert_eq!(
            (prep.label(), prep.header(), prep.label_rev()),
            (
                whole.label.as_str(),
                whole.header.as_str(),
                whole.label_rev.as_str()
            )
        );
        assert_eq!(prep.changes(), Some("changes.html"));
        for _ in 0..2 {
            for path in [
                "index.html",
                "guides/setup.html",
                "overview.html",
                "changes.html",
            ] {
                let file = prep.render(&st, path).expect("render").expect("file");
                same_as_whole(&prep, &file, &whole, path, &target);
            }
        }
        assert_eq!(
            crate::probe::preparations() - prepared,
            1,
            "the requests prepared nothing"
        );
        assert_eq!(
            prep.retained_bytes(&mut RetainedAllocations::new()),
            before,
            "no retained part grew"
        );
        // the estimate covers the retained text, beyond any wrapper's size
        let words: u64 = st
            .materialized(&scope)
            .unwrap()
            .iter()
            .map(|b| b.body.len() as u64)
            .sum();
        assert!(before > words + std::mem::size_of::<PreparedReview>() as u64);

        // one recount: the same target counts once; a second preparation of the same state
        // shares its blocks and graph, which count once
        let again = prepare_review(&st, &scope, &target).expect("prepare");
        let alone = again.retained_bytes(&mut RetainedAllocations::new());
        let mut seen = RetainedAllocations::new();
        assert_eq!(prep.retained_bytes(&mut seen), before);
        assert_eq!(
            prep.clone().retained_bytes(&mut seen),
            0,
            "a clone is the same target"
        );
        let shared = again.retained_bytes(&mut seen);
        assert!(
            shared < alone,
            "{shared} < {alone}: the shared parts count once"
        );
        assert!(shared > 0, "its own parts count");
        drop(seen);

        // a new state: a new preparation, with the new words
        write(
            &mut fx.r,
            "work",
            json!([{"op": "update", "id": fx.ids["$sp"], "body": "Setup words, LATERWORDS."}]),
        );
        let st2 = fx.r.read();
        let next = prepare_review(&st2, &scope, &target).expect("prepare");
        assert_eq!(crate::probe::preparations() - prepared, 3);
        let file = next.render(&st2, "guides/setup.html").unwrap().unwrap();
        assert!(text(&file).contains("LATERWORDS"));
        let old = prep.render(&st, "guides/setup.html").unwrap().unwrap();
        assert!(!text(&old).contains("LATERWORDS"), "the kept state stays");
    }

    /// The search index and both llms files render as three separate requests, each to the
    /// whole build's bytes, with no HTML page rendered and nothing kept.
    #[test]
    fn sod1_auxiliary_files_are_independent_requests() {
        let fx = fixture();
        let st = fx.r.read();
        let (scope, target) = view();
        let whole = live::build_live(&st, &scope, &target).expect("whole");
        let prep = prepare_review(&st, &scope, &target).expect("prepare");
        let kept = prep.retained_bytes(&mut RetainedAllocations::new());
        for path in ["search-index.json", "llms.txt", "llms-full.txt"] {
            let renders = page_renders();
            let file = prep.render(&st, path).expect("render").expect("file");
            assert_eq!(page_renders() - renders, 0, "{path}: no HTML page rendered");
            assert!(file.warnings.is_empty(), "{path}");
            same_as_whole(&prep, &file, &whole, path, &target);
        }
        assert_eq!(prep.retained_bytes(&mut RetainedAllocations::new()), kept);
    }

    /// A profile route under the view reads each side's own selection: its selected content
    /// with the excluded reference masked, the removed line and the new figure, links under the
    /// route's root, and no excluded doc. The source review of the same view keeps them all.
    #[test]
    fn sod1_profile_review_uses_each_sides_selection() {
        let fx = fixture();
        let st = fx.r.read();
        let target = profile(ScopeRef::View("work".into()));
        let whole = whole_profile(&st, &target);
        let prep = prep_profile(&st, &target);
        assert_eq!(prep.profile().map(|p| p.title.as_str()), Some("Guide"));
        assert_eq!(prep.changes(), Some("changes.html"));
        assert!(!whole.files.contains_key("broken.html"));
        for path in html(&whole) {
            let file = prep.render(&st, &path).expect("render").expect("file");
            same_as_whole(&prep, &file, &whole, &path, &target);
        }
        assert_eq!(prep.render(&st, "broken.html").unwrap(), None);
        let ov = text(&prep.render(&st, "overview.html").unwrap().unwrap());
        assert!(ov.contains("NEWWORDS"), "{ov}");
        assert!(ov.contains("unavailable reference"), "{ov}");
        assert!(!ov.contains("BROKENWORDS"), "{ov}");
        assert!(ov.contains("GONEWORDS"), "the removed line: {ov}");
        assert!(
            prep.header().contains("/~view/work/~profile/guide/"),
            "{}",
            prep.header()
        );
        // the source review of the same view has the excluded doc, and its reference resolves
        let (scope, src) = view();
        let source = prepare_review(&st, &scope, &src).expect("prepare");
        let ov = text(&source.render(&st, "overview.html").unwrap().unwrap());
        assert!(!ov.contains("unavailable reference"), "{ov}");
        assert!(source.render(&st, "broken.html").unwrap().is_some());
        assert!(source.profile().is_none());
    }

    /// Direct figure files (light and dark), a diagram file and a theme file come from the
    /// captured target with no page requested first and no whole build; so does the target's
    /// own not-found page.
    #[test]
    fn sod1_assets_and_missing_pages_need_no_whole_build() {
        let fx = fixture();
        let st = fx.r.read();
        let (scope, src) = view();
        let route = profile(ScopeRef::View("work".into()));
        let source_whole = live::build_live(&st, &scope, &src).expect("whole");
        let route_whole = whole_profile(&st, &route);
        for (whole, target) in [(&source_whole, &src), (&route_whole, &route)] {
            let assets: Vec<String> = whole
                .files
                .keys()
                .filter(|p| p.starts_with("assets/"))
                .cloned()
                .collect();
            let figures = assets.iter().filter(|p| p.ends_with(".png")).count();
            assert_eq!(figures, 2, "light and dark: {assets:?}");
            assert!(assets.iter().any(|p| p.starts_with("assets/diagrams/")));
            assert!(assets.iter().any(|p| p == "assets/site.css"));
            budget_hooks::take_dispatched();
            // a fresh preparation each: no page was requested, as after the pages' eviction
            for path in &assets {
                let prep = match target {
                    SiteTarget::Profile { .. } => prep_profile(&st, target),
                    _ => prepare_review(&st, &scope, target).expect("prepare"),
                };
                let renders = page_renders();
                let file = prep.render(&st, path).expect("render").expect("file");
                assert_eq!(page_renders() - renders, 0, "{path}: no page rendered");
                same_as_whole(&prep, &file, whole, path, target);
            }
            assert_eq!(budget_hooks::take_dispatched(), vec![], "no whole build");
            let prep = prepare_review(&st, &scope, &src).expect("prepare");
            assert_eq!(
                prep.render(&st, &format!("assets/{}.png", Hash::of(b"none")))
                    .unwrap(),
                None
            );
        }
        // the old figure is in no page of the view: no file
        let prep = prepare_review(&st, &scope, &src).expect("prepare");
        let old = format!("assets/{}.png", Hash::of(&fx.old_fig));
        assert!(!source_whole.files.contains_key(&old));
        assert_eq!(prep.render(&st, &old).unwrap(), None);
        // the themed miss, from the captured target, with no page built first
        assert_eq!(
            prep.not_found(NotFoundKind::Page),
            source_whole.not_found.page
        );
        assert_eq!(prep.not_found(NotFoundKind::Scope), None);
        let route_prep = prep_profile(&st, &route);
        assert_eq!(
            route_prep.not_found(NotFoundKind::Page),
            route_whole.not_found.page
        );
        assert_eq!(budget_hooks::take_dispatched(), vec![], "no whole build");
    }

    /// The complete pass gives the whole build's warnings in its order: the scope's and the
    /// profile's, then the diagram warning of a page nobody requested. It renders and keeps no
    /// page file.
    #[test]
    fn sod1_checks_collect_all_warnings_without_pages() {
        let fx = fixture();
        let st = fx.r.read();
        let (scope, src) = view();
        let source_whole = live::build_live(&st, &scope, &src).expect("whole");
        let route = profile(ScopeRef::View("work".into()));
        let route_whole = whole_profile(&st, &route);
        let source = prepare_review(&st, &scope, &src).expect("prepare");
        let route_prep = prep_profile(&st, &route);
        assert!(
            source_whole.warnings.iter().any(|w| w.contains("Broken")),
            "{:?}",
            source_whole.warnings
        );
        assert!(
            source_whole.warnings.iter().any(|w| w.contains("glossary")),
            "{:?}",
            source_whole.warnings
        );
        // the route's selection leaves out the glossary doc and the diagram that does not draw:
        // its reference warning alone
        assert!(
            !route_whole.warnings.is_empty(),
            "{:?}",
            route_whole.warnings
        );
        for (prep, whole) in [(&source, &source_whole), (&route_prep, &route_whole)] {
            let kept = prep.retained_bytes(&mut RetainedAllocations::new());
            let renders = page_renders();
            let check = prep.check(&st).expect("check");
            assert_eq!(page_renders() - renders, 0, "no page file rendered");
            assert_eq!(check.warnings, whole.warnings);
            assert_eq!(check.epoch, whole.epoch);
            assert!(whole.warnings.starts_with(prep.warnings()));
            assert_eq!(
                prep.retained_bytes(&mut RetainedAllocations::new()),
                kept,
                "nothing kept"
            );
        }
        // a requested page's own diagram warning is its contribution
        let broken = source.render(&st, "broken.html").unwrap().unwrap();
        assert_eq!(broken.warnings.len(), 1, "{:?}", broken.warnings);
        assert!(source_whole.warnings.contains(&broken.warnings[0]));
    }

    /// Selection, attachment and path bounds refuse a profile review as they refuse its whole
    /// build, with the same errors; the file count bounds source and profile reviews, counted
    /// without rendering a page body.
    #[test]
    fn sod1_review_requests_keep_selection_attachment_path_and_file_count_bounds() {
        use rdy_engine::publication::{Limits as Bounds, limits, override_limits_for_tests};
        assert_eq!(MAX_FILES, 32000);
        let fx = fixture();
        let st = fx.r.read();
        let route = profile(ScopeRef::View("work".into()));
        let bounds = [
            Bounds {
                selection: 1,
                ..limits()
            },
            Bounds {
                assets: (FIG as u64) - 1,
                ..limits()
            },
            Bounds {
                path: 3,
                ..limits()
            },
        ];
        for b in bounds {
            let _g = override_limits_for_tests(b);
            let whole = live::build_profile_live(&st, &route, &mut ProfileHistoryMemo::default())
                .err()
                .expect("the whole build refuses");
            let review = prepare_profile_review(&st, &route, &mut ProfileHistoryMemo::default())
                .err()
                .expect("the review refuses");
            assert_eq!(
                (review.code, &review.message, &review.fix),
                (whole.code, &whole.message, &whole.fix)
            );
        }
        let (scope, src) = view();
        let source_whole = live::build_live(&st, &scope, &src).expect("whole");
        for target in [src.clone(), route.clone()] {
            let prepare = || match &target {
                SiteTarget::Profile { .. } => {
                    prepare_profile_review(&st, &target, &mut ProfileHistoryMemo::default())
                }
                _ => prepare_review(&st, &scope, &target),
            };
            let (builds, renders) = (page_builds(), page_renders());
            let files = prepare().expect("prepare").files();
            assert_eq!(
                (page_builds(), page_renders()),
                (builds, renders),
                "counted without a page body"
            );
            if matches!(target, SiteTarget::Source(_)) {
                // every file of the whole build is counted; a diagram that does not draw counts
                // as a file too
                assert!(files > source_whole.files.len(), "{files}");
            }
            within((files, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES), || {
                prepare().expect("the exact count")
            });
            let e = within((files - 1, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES), || {
                prepare().err().expect("one file over")
            });
            over(&e, format!("{} files", files - 1));
        }
    }

    /// Each requested review file is held to the per-file bound: exactly the limit renders, one
    /// byte over refuses, for an HTML page and each auxiliary file, source and profile alike.
    #[test]
    fn sod1_per_file_bound_applies_to_each_requested_review_page() {
        assert_eq!(MAX_FILE_BYTES, 2_147_483_648);
        let fx = fixture();
        let st = fx.r.read();
        let (scope, src) = view();
        let route = profile(ScopeRef::View("work".into()));
        for target in [src, route] {
            // the figures are held when the target is prepared; each requested file when it is
            // rendered
            let free = match &target {
                SiteTarget::Profile { .. } => prep_profile(&st, &target),
                _ => prepare_review(&st, &scope, &target).expect("prepare"),
            };
            for path in [
                "overview.html",
                "changes.html",
                "search-index.json",
                "llms.txt",
                "llms-full.txt",
            ] {
                let file = free.render(&st, path).unwrap().expect("file");
                let len = file.bytes.len() as u64;
                let exact = within((MAX_FILES, len, 1), || free.render(&st, path));
                assert_eq!(
                    exact
                        .unwrap_or_else(|e| panic!("{path} exact: {e}"))
                        .expect("file")
                        .bytes,
                    file.bytes
                );
                let e = within((MAX_FILES, len - 1, MAX_ARTIFACT_BYTES), || {
                    free.render(&st, path).err().expect("one byte over")
                });
                over(&e, format!("{} file bytes", len - 1));
            }
        }
    }

    /// A review target whose files together pass the artifact limit still serves every page,
    /// source and profile alike, with no aggregate check and no whole build; the profile's old
    /// and new figures together pass that limit too. A bounded whole build of the same state
    /// refuses, which shows the fixture passes it.
    #[test]
    fn sod1_live_review_has_no_whole_artifact_byte_check() {
        let fx = fixture();
        let st = fx.r.read();
        let (scope, src) = view();
        let route = profile(ScopeRef::View("work".into()));
        // under the old and new figures together, above each
        let limit = (fx.old_fig.len() + fx.new_fig.len()) as u64 - 1;
        assert!(limit > fx.new_fig.len() as u64);
        let source_whole = live::build_live(&st, &scope, &src).expect("whole");
        let total: u64 = source_whole.files.values().map(|f| f.len() as u64).sum();
        assert!(total > limit);
        let limits = (MAX_FILES, MAX_FILE_BYTES, limit);
        within(limits, || {
            // the bounded whole builds refuse at the artifact limit
            let bounded = build_site_from(
                prepare::source(&st, &scope, prepare::Purpose::Site).unwrap(),
                Mode::Live { root: "/" },
                Slots::Source,
                Some(&Budget::publication()),
                NotFoundOut::None,
            )
            .err()
            .expect("the bounded source build refuses");
            over(&bounded, format!("{limit} artifact bytes"));
            let e = live::build_profile_live(&st, &route, &mut ProfileHistoryMemo::default())
                .err()
                .expect("the bounded profile build refuses");
            over(&e, format!("{limit} artifact bytes"));
            budget_hooks::take_dispatched();
            let aggregates = budget_hooks::aggregates();
            for target in [&src, &route] {
                let prep = match target {
                    SiteTarget::Profile { .. } => prep_profile(&st, target),
                    _ => prepare_review(&st, &scope, target).expect("prepare"),
                };
                let mut served = 0u64;
                let renders = page_renders();
                let paths = html(&source_whole);
                let mut requested = 0;
                for path in &paths {
                    if let Some(f) = prep.render(&st, path).expect("render") {
                        served += f.bytes.len() as u64;
                        requested += 1;
                    }
                }
                assert!(served > limit, "{served} bytes served");
                assert_eq!(page_renders() - renders, requested, "only requested pages");
            }
            assert_eq!(budget_hooks::aggregates(), aggregates, "no aggregate check");
            assert_eq!(budget_hooks::take_dispatched(), vec![], "no whole build");
        });
    }

    /// The paths that keep a budget still dispatch with one, and refuse at each of its limits:
    /// a publication artifact, a one-page publication artifact, canon's and a tag's live profile
    /// routes, and the profile-only site.
    #[test]
    fn sod1_existing_budget_paths_keep_artifact_and_file_limits() {
        assert_eq!(
            (MAX_FILES, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES),
            (32000, 2_147_483_648, 2_147_483_648)
        );
        let fx = fixture();
        let st = fx.r.read();
        let canon = Scope::Canon;
        let inputs: RenderInputs = crate::render_input::capture_inputs(&st, &canon).unwrap();
        let source = ReadPublicationSource::with_settings(
            &st,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = st.materialized(&canon).unwrap();
        // each path, and the files it writes
        type Files = BTreeMap<String, Vec<u8>>;
        let paths: Vec<(&str, Box<dyn Fn() -> Result<Files, RdyError> + '_>)> = vec![
            (
                "publication",
                Box::new(|| {
                    crate::build_publication(&source, &m, "v1.0.0", false, "guide", &inputs)
                        .map(|a| a.files)
                }),
            ),
            (
                "one page",
                Box::new(|| {
                    crate::render_publication_page(
                        &source,
                        &m,
                        "v1.0.0",
                        false,
                        "guide",
                        &inputs,
                        "overview.html",
                    )
                    .map(|p| {
                        let mut files = p.assets;
                        files.insert(p.path, p.html);
                        files
                    })
                }),
            ),
            (
                "canon route",
                Box::new(|| {
                    live::build_profile_live(
                        &st,
                        &profile(ScopeRef::Canon),
                        &mut ProfileHistoryMemo::default(),
                    )
                    .map(|b| b.live.files)
                }),
            ),
            (
                "tag route",
                Box::new(|| {
                    live::build_profile_live(
                        &st,
                        &profile(ScopeRef::Tag("v1.0.0".into())),
                        &mut ProfileHistoryMemo::default(),
                    )
                    .map(|b| b.live.files)
                }),
            ),
            (
                "profile-only",
                Box::new(|| {
                    live::build_profile_live(
                        &st,
                        &SiteTarget::ProfileOnly {
                            key: "guide".into(),
                        },
                        &mut ProfileHistoryMemo::default(),
                    )
                    .map(|b| b.live.files)
                }),
            ),
        ];
        for (name, build) in paths {
            budget_hooks::take_dispatched();
            let files = build().unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                budget_hooks::take_dispatched(),
                vec![Some((MAX_FILES, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES))],
                "{name} dispatches with the publication budget"
            );
            let n = files.len();
            let total: u64 = files.values().map(|f| f.len() as u64).sum();
            let largest = files.values().map(Vec::len).max().unwrap() as u64;
            within((n, MAX_FILE_BYTES, total), || {
                build().unwrap_or_else(|e| panic!("{name} at its exact limits: {e}"))
            });
            let e = within((n - 1, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES), || {
                build().err().expect("files")
            });
            over(&e, format!("{} files", n - 1));
            let e = within((MAX_FILES, largest - 1, MAX_ARTIFACT_BYTES), || {
                build().err().expect("file bytes")
            });
            over(&e, format!("{} file bytes", largest - 1));
            let e = within((MAX_FILES, MAX_FILE_BYTES, total - 1), || {
                build().err().expect("artifact bytes")
            });
            over(&e, format!("{} artifact bytes", total - 1));
            assert!(
                budget_hooks::take_dispatched().iter().all(|d| d.is_some()),
                "{name}"
            );
        }
    }

    /// Static source and profile builds and the live source builds of canon and a tag dispatch
    /// with no budget: they build above a tiny injected limit, and no budget is asked.
    #[test]
    fn sod1_static_and_source_live_builds_gain_no_artifact_budget() {
        let fx = fixture();
        let st = fx.r.read();
        let tag = st.resolve_scope("tag:v1.0.0").expect("tag");
        within((1, 1, 1), || {
            budget_hooks::take_dispatched();
            let admits = budget_hooks::admits();
            let files = [
                crate::build_site(&st, &Scope::Canon, Mode::Static)
                    .expect("static source")
                    .files,
                crate::build_profile_site(&st, "canon", "guide")
                    .expect("static profile")
                    .files,
                live::build_live(&st, &Scope::Canon, &SiteTarget::Source(ScopeRef::Canon))
                    .expect("live canon")
                    .files,
                live::build_live(
                    &st,
                    &tag,
                    &SiteTarget::Source(ScopeRef::Tag("v1.0.0".into())),
                )
                .expect("live tag")
                .files,
            ];
            for f in &files {
                assert!(f.len() > 1);
                assert!(f.values().map(|b| b.len() as u64).sum::<u64>() > 1);
            }
            assert_eq!(budget_hooks::take_dispatched(), vec![None; 4]);
            assert_eq!(budget_hooks::admits(), admits, "no budget was asked");
        });
    }

    /// The publication check reads the source and renders no artifact: its report is the same
    /// above a tiny injected artifact limit, and only the report's own 8 MiB bound holds.
    #[test]
    fn sod1_publication_check_keeps_report_bound_only() {
        assert_eq!(rdy_engine::publication::REPORT_BYTES, 8_388_608);
        let fx = fixture();
        let st = fx.r.read();
        let report = rdy_engine::publication::check(&st, "guide", None, None, None).expect("check");
        within((1, 1, 1), || {
            budget_hooks::take_dispatched();
            let (builds, renders, admits) = (page_builds(), page_renders(), budget_hooks::admits());
            let again =
                rdy_engine::publication::check(&st, "guide", None, None, None).expect("check");
            assert_eq!(again, report, "the same facts");
            assert_eq!(
                (page_builds(), page_renders(), budget_hooks::admits()),
                (builds, renders, admits),
                "no render and no budget"
            );
            assert_eq!(budget_hooks::take_dispatched(), vec![]);
        });
        assert!(serde_json::to_vec(&report).unwrap().len() < 1 << 20);
    }
}
