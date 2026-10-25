//! Source preparation (D-0VCQ5): everything an output reads of the store, read before the output
//! boundary. The site and print builders receive a [`Prepared`] and never a source reader: no
//! page, trail, home, search, agent-file or print module reads the store, its history or its
//! tensions on its own.
use crate::site::Site;
use crate::{changes, disagree, review, version};
use rdy_engine::{
    BlobState, E_REDACTED, Materialized, RdyError, ReadState, Scope, VersionInfo, VersionPosition,
};
use rdy_graph::Graph;
use rdy_model::redaction::BlockResidue;
use rdy_model::{Block, BlockId, Hash, Op};
use rdy_schema::Pack;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Which output a preparation is for: the site reads the review and the change summaries; the
/// print reads only its label and warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    Site,
    Print,
}

/// What an output reads of one scope, read before it builds anything. It holds no store reader,
/// no closure that reads the store and no `ReadState`. Never kept across operations.
pub(crate) struct Prepared {
    /// The scope's blocks, each figure whose blob lost content read as removed
    /// ([`FigureBlobs::project`]).
    pub m: Arc<Materialized>,
    pub g: Arc<Graph>,
    pub pack: Pack,
    /// The figure blobs the store holds, with their bytes, and those a redaction removed content
    /// of.
    pub blobs: FigureBlobs,
    pub site_name: String,
    pub description: Option<String>,
    pub position: VersionPosition,
    pub versions: Vec<VersionInfo>,
    /// When each version's landing was approved, by landing hash (hex).
    pub landed_ms: BTreeMap<String, i64>,
    /// The disagreements the consumer's site applies (`Site::set_disagreements`).
    pub disagreements: Vec<disagree::Disagreement>,
    /// The scope's text: `canon`, `view:<name>`, `tag:<version>`, `landing:<hash>` or
    /// `proposal:<hash>`.
    pub scope_text: String,
    /// The project's `.rdy` directory: for output safety only, never written into an output.
    /// Empty for an in-memory publication (`publication::prepare_resolved`), which is never
    /// placed by it; a durable local output keeps the project's directory and its guard.
    pub rdy_dir: PathBuf,
    pub history: History,
    /// How a named profile's output presents itself (`publication::prepare`); none for a source
    /// output, which keeps its existing rules.
    pub profile: Option<ProfilePresentation>,
    /// Where a live page's pill opens the menu (`SiteTarget::menu_url`); none for the source's
    /// default `/~versions/`. A static page never reads it.
    pub menu_url: Option<String>,
}

/// What a named profile's output reads beyond its selected state (RULING F3, D-0VCQ5): whether
/// it shows the decision trail, and the explicitly chosen tag's name with its `redacted` mark.
/// Only this mode chooses the neutral wording of a reference outside the selection and filters
/// authored local links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfilePresentation {
    pub trail: bool,
    /// The chosen tag's stored name; none for canon and a bare landing.
    pub tag_name: Option<String>,
    /// The chosen tag's history holds content a redaction removed (`VersionInfo::redacted`).
    pub tag_redacted: bool,
    /// Which output the profile is shown in.
    pub presentation: Presentation,
}

/// Where a named profile is shown (D-CWC91, D-DK03K).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Presentation {
    /// A durable output (a static site, a print, a publication): the chosen tag's name alone.
    Output,
    /// A live route under a scope: the existing version header with the profile's title, its
    /// selected counts, the menu and the review controls.
    LiveRoute {
        /// The header's words (`version::profile_header`).
        header: version::Header,
        /// The whole colophon sentence (`version::profile_colophon`).
        colophon: String,
    },
    /// The profile-only site of canon: no pill, chip, menu, review or history.
    LiveOnly {
        /// The colophon sentence: the profile's title and a period.
        colophon: String,
    },
}

/// What an output reads of the scope's history: its review, the view or proposal version it
/// shows, the change summaries and the build's warnings.
pub(crate) struct History {
    /// What a review render changes, as its change rows read it ([`review::listed`]).
    pub listed: Option<review::Review>,
    /// The scope names the base `listed` is read against: the pages carry review marks.
    pub review_marks: bool,
    /// The view or proposal version the scope shows, with the blocks it changes.
    pub reviewed: Option<version::Reviewed>,
    /// The word diff of each changed block a page with review marks shows.
    pub inline_diffs: BTreeMap<BlockId, String>,
    /// The change summary of a review render (`changes.html`), when no doc owns the page.
    pub changes: Option<changes::Summary>,
    /// The summary of the landings since the tag (`changes-since.html`), when no doc owns the
    /// page.
    pub since: Option<changes::Summary>,
    /// The build's warnings for the scope, in order.
    pub warnings: Vec<String>,
    /// A named profile's review base, its selected state (D-CWC91): the words only it holds (a
    /// removed line, a removed page's row) read their references through it. None for the
    /// source, whose removed text reads against the scope's site.
    pub old: Option<Arc<Materialized>>,
}

impl History {
    /// No review, no summary, no warning.
    pub fn empty() -> History {
        History {
            listed: None,
            review_marks: false,
            reviewed: None,
            inline_diffs: BTreeMap::new(),
            changes: None,
            since: None,
            warnings: vec![],
            old: None,
        }
    }
}

/// Reads everything an output of `scope` shows, for `purpose`.
pub(crate) fn source(
    rdy: &ReadState,
    scope: &Scope,
    purpose: Purpose,
) -> Result<Prepared, RdyError> {
    source_reading(rdy, scope, purpose, rdy)
}

/// [`source`], reading the figure blobs through `reads`.
fn source_reading(
    rdy: &ReadState,
    scope: &Scope,
    purpose: Purpose,
    reads: &dyn BlobReads,
) -> Result<Prepared, RdyError> {
    let position = rdy.version_position(scope)?;
    let versions = rdy.versions()?;
    // when each version's landing was approved (only the versions' landings are read)
    let landed_ms: BTreeMap<String, i64> = versions
        .iter()
        .filter_map(|v| {
            let h: Hash = v.landing.parse().ok()?;
            let rec = rdy.reader().landing(&h).ok()?;
            Some((v.landing.clone(), rec.approved_by.at_ms))
        })
        .collect();
    // the figure blobs the store holds; a figure whose blob lost content reads as removed from
    // here on, so no page, summary or print reads its file or words
    let mut blobs = FigureBlobs::default();
    let m = blobs.side(rdy, reads, scope, true)?;
    let site_name = rdy.project_name();
    let description = rdy.project_description();
    let pack = rdy.pack().clone();
    let g = rdy.graph(scope)?;
    let disagreements = disagree::list(rdy, scope)?;
    let history = history(
        rdy, reads, scope, purpose, &m, &pack, &site_name, &position, &mut blobs,
    )?;
    let scope_text = scope_text(&versions, scope);
    Ok(Prepared {
        m,
        g,
        pack,
        blobs,
        site_name,
        description,
        position,
        versions,
        landed_ms,
        disagreements,
        scope_text,
        rdy_dir: rdy.root().join(".rdy"),
        history,
        profile: None,
        menu_url: None,
    })
}

/// The scope as rdy reads it back: a version by its tag.
fn scope_text(versions: &[VersionInfo], scope: &Scope) -> String {
    match scope {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => format!("view:{v}"),
        Scope::At(h) => {
            let hex = h.to_string();
            match versions.iter().find(|v| v.landing == hex) {
                Some(v) => format!("tag:{}", v.name),
                None => format!("landing:{hex}"),
            }
        }
        Scope::Proposal(v) => format!("proposal:{v}"),
        Scope::Inflight => rdy_engine::INFLIGHT_SCOPE.to_string(),
    }
}

/// The history an output of `scope` shows. The site reads its review, its change summary and
/// the summary since its tag; the print only its label's view or proposal version and the
/// warnings. Each summary reads against a site built here and dropped.
#[allow(clippy::too_many_arguments)]
fn history(
    rdy: &ReadState,
    reads: &dyn BlobReads,
    scope: &Scope,
    purpose: Purpose,
    m: &Materialized,
    pack: &Pack,
    site_name: &str,
    position: &VersionPosition,
    blobs: &mut FigureBlobs,
) -> Result<History, RdyError> {
    use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, summary_page_taken};
    let site = Site::new(m, pack, site_name);
    // what the scope changes: its chip, its change summary, the home page's list and the menu's
    // count all read it (O12). A proposal version with no base here (made on the empty project,
    // read once canon has landed) still names itself and counts against that empty base
    let listed = review::listed(rdy, scope)?;
    // the review marks on the pages read only a base a scope names: that proposal version's
    // pages show none (the task-8 ruling)
    let base = match purpose {
        Purpose::Site => review::base_of(rdy, scope),
        Purpose::Print => None,
    };
    let reviewed = match listed.as_ref().map(|rv| rv.changed_blocks()) {
        Some(blocks) => reviewed_of(rdy, scope, blocks)?,
        None => None,
    };
    let since_of = since_of(rdy, scope, position);
    let warnings = crate::scope_warnings(
        m,
        &site,
        reviewed.is_some() && listed.is_some(),
        since_of.as_ref().map(|(name, ..)| name.as_str()),
    );
    if purpose == Purpose::Print {
        // the print shows no review mark, no change summary and no diff: only the label
        return Ok(History {
            reviewed,
            warnings,
            ..History::empty()
        });
    }
    let review_marks = base.is_some();

    // the change summary of a review render (sr6): against its base, or, for a proposal version
    // made on the empty project that no scope names, against that empty project. A top-level
    // doc that owns its path leaves out only the page (and its chip's link): the review pages
    // keep each edited block's inline diff.
    let mut changes_summary: Option<changes::Summary> = None;
    let mut inline_diffs: BTreeMap<BlockId, String> = BTreeMap::new();
    if let (Some(r), Some(rv)) = (&reviewed, listed.as_ref()) {
        let (diff, from) = match &base {
            Some(b) => (Some(rdy.diff(b, scope)?), blobs.side(rdy, reads, b, false)?),
            None => (None, Default::default()),
        };
        let sides = changes::Sides {
            from: &from,
            to: m,
            text: &site,
            from_text: None,
        };
        let (s, inline) = changes::summary(
            &site,
            CHANGES_PAGE,
            rv,
            diff.as_ref(),
            &sides,
            &changes::Of::Review(r.kind),
        );
        // a top-level doc that owns the page keeps it (`scope_warnings` says so)
        if !summary_page_taken(m, CHANGES_PAGE) {
            changes_summary = Some(s);
        }
        // only a page with review marks shows an edited block's diff
        if review_marks {
            inline_diffs = inline;
        }
    }
    // the summary of the landings since the tag (sr6): from the tag to the state the position
    // counts to; a top-level doc that owns the page keeps it (`scope_warnings` says so)
    let mut since_summary: Option<changes::Summary> = None;
    if let Some((name, landings, tag, to)) = since_of
        && !summary_page_taken(m, SINCE_PAGE)
    {
        let rv = review::between(rdy, &tag, &to)?;
        let diff = rdy.diff(&tag, &to)?;
        let from = blobs.side(rdy, reads, &tag, false)?;
        // both sides read against the `to` side's site (O4)
        let to_m = (to != *scope)
            .then(|| blobs.side(rdy, reads, &to, false))
            .transpose()?;
        let to_site = to_m.as_ref().map(|tm| Site::new(tm, pack, site_name));
        let sides = changes::Sides {
            from: &from,
            to: to_m.as_deref().unwrap_or(m),
            text: to_site.as_ref().unwrap_or(&site),
            from_text: None,
        };
        let (s, _) = changes::summary(
            &site,
            SINCE_PAGE,
            &rv,
            Some(&diff),
            &sides,
            &changes::Of::Since {
                tag: name,
                landings,
            },
        );
        since_summary = Some(s);
    }
    Ok(History {
        listed,
        review_marks,
        reviewed,
        inline_diffs,
        changes: changes_summary,
        since: since_summary,
        warnings,
        old: None,
    })
}

/// The figure blobs a build reads (D-3M81W): the bytes of each readable blob of the scope, and
/// the blobs a redaction removed a chunk of, by the first request that removed one. A removed
/// blob's bytes are never read, so they never reach an asset file or the print's image decoder.
/// Each blob's state is read once per build from its metadata, and its bytes once, only when the
/// scope a build pages shows it (D-FY4VF).
#[derive(Default)]
pub(crate) struct FigureBlobs {
    pub bytes: BTreeMap<Hash, Vec<u8>>,
    pub removed: BTreeMap<Hash, Hash>,
    /// The state each blob this build reached read as, kept for the build's other sides.
    states: BTreeMap<Hash, BlobState>,
}

impl FigureBlobs {
    /// The blobs of a named profile's selection: the bytes its bounded asset reader read, and the
    /// blobs a redaction removed content of, by the request that did.
    pub(crate) fn selected(bytes: BTreeMap<Hash, Vec<u8>>, removed: BTreeMap<Hash, Hash>) -> Self {
        FigureBlobs {
            bytes,
            removed,
            states: BTreeMap::new(),
        }
    }

    /// The blocks of `scope`, each figure whose blob lost content read as removed. `keep` reads
    /// and keeps the bytes of each readable blob (the scope a build pages); a change summary's
    /// other side only reads which blobs lost content, never a byte.
    fn side(
        &mut self,
        rdy: &ReadState,
        reads: &dyn BlobReads,
        scope: &Scope,
        keep: bool,
    ) -> Result<Arc<Materialized>, RdyError> {
        let m = rdy.materialized(scope)?;
        for b in m.iter() {
            let Some(h) = figure_blob(b) else { continue };
            if self.bytes.contains_key(&h) {
                continue;
            }
            let state = match self.states.get(&h) {
                Some(state) => *state,
                None => {
                    let state = reads.state(&h)?;
                    self.states.insert(h, state);
                    if let BlobState::Removed(request) = state {
                        self.removed.insert(h, request);
                    }
                    state
                }
            };
            if !keep || state != BlobState::Readable {
                continue;
            }
            // a readable blob the paged scope shows: its bytes, once
            match reads.bytes(&h) {
                Ok(bytes) => {
                    self.bytes.insert(h, bytes);
                }
                Err(e) => match removed_by(&e) {
                    Some(request) => {
                        self.states.insert(h, BlobState::Removed(request));
                        self.removed.insert(h, request);
                    }
                    None => return Err(e),
                },
            }
        }
        Ok(self.project(m))
    }

    /// `m` with each figure whose blob lost content read as a removed block: its id, type,
    /// parent and order, the notice, no fields, and its own recorded version (Task 3's
    /// projection). Its alt text, caption and file name are never read again.
    pub(crate) fn project(&self, m: Arc<Materialized>) -> Arc<Materialized> {
        let ops: Vec<Op> = m
            .iter()
            .filter_map(|b| {
                let request = self.removed.get(&figure_blob(b)?)?;
                Some(Op::Update {
                    base: b.version_hash(),
                    block: BlockResidue::of(b).stub(*request),
                })
            })
            .collect();
        if ops.is_empty() {
            m
        } else {
            Arc::new(m.with(&ops))
        }
    }
}

/// What collecting a build's figure blobs reads of the store: a blob's state from its metadata,
/// and its bytes. A source build reads through the store; a test counts the reads.
pub(crate) trait BlobReads {
    fn state(&self, h: &Hash) -> Result<BlobState, RdyError>;
    fn bytes(&self, h: &Hash) -> Result<Vec<u8>, RdyError>;
}

impl BlobReads for ReadState {
    fn state(&self, h: &Hash) -> Result<BlobState, RdyError> {
        self.reader().blob_state(h).map_err(RdyError::from)
    }

    fn bytes(&self, h: &Hash) -> Result<Vec<u8>, RdyError> {
        self.reader().get_blob(h).map_err(RdyError::from)
    }
}

/// The blob a figure shows; none for any other block, and for a removed figure (it has no
/// fields).
fn figure_blob(b: &Block) -> Option<Hash> {
    if b.block_type != "figure" || b.is_redacted() {
        return None;
    }
    b.field_str("blob")?.parse().ok()
}

/// The request an `E_REDACTED` refusal names (`Content removed by redaction {hash}.`); none for
/// any other error.
pub(crate) fn removed_by(e: &RdyError) -> Option<Hash> {
    if e.code != E_REDACTED {
        return None;
    }
    e.message
        .strip_prefix("Content removed by redaction ")?
        .strip_suffix('.')?
        .parse()
        .ok()
}

/// The summary of the landings since the tag a scope counts from (sr6): the tag's name, how many
/// landings, the tag's scope, and the state the position counts to (canon, a view's or
/// proposal's base, or the as-of landing). None when the scope counts no landing since a tag.
fn since_of(
    rdy: &ReadState,
    scope: &Scope,
    position: &VersionPosition,
) -> Option<(String, u32, Scope, Scope)> {
    match (&position.version, position.landings_since) {
        (Some(v), n) if n > 0 => {
            let to = match scope {
                Scope::Canon => Some(Scope::Canon),
                Scope::View(_) | Scope::Proposal(_) => review::base_of(rdy, scope),
                Scope::At(h) => Some(Scope::At(*h)),
                Scope::Inflight => None,
            };
            let tag = v.landing.parse::<Hash>().ok().map(Scope::At);
            tag.zip(to).map(|(tag, to)| (v.name.clone(), n, tag, to))
        }
        _ => None,
    }
}

/// What a review render shows (D-BMX8P): a view's name and owner, or a proposal version's title
/// and author, with the `blocks` it changes (`Review::changed_blocks`). None for any other scope.
pub(crate) fn reviewed_of(
    rdy: &ReadState,
    scope: &Scope,
    blocks: u32,
) -> Result<Option<version::Reviewed>, RdyError> {
    Ok(match scope {
        Scope::View(name) => Some(version::Reviewed {
            kind: version::ReviewedKind::View,
            name: name.clone(),
            author: rdy
                .view_head(name)
                .and_then(|h| h.owner.as_ref())
                .map(|o| o.actor.clone()),
            blocks,
        }),
        Scope::Proposal(v) => {
            let p = rdy.proposal_version(v)?;
            Some(version::Reviewed {
                kind: version::ReviewedKind::Proposal,
                name: p.title,
                author: Some(p.provenance.actor),
                blocks,
            })
        }
        Scope::Canon | Scope::At(_) | Scope::Inflight => None,
    })
}

#[cfg(test)]
mod dgm2 {
    use super::*;
    use rdy_engine::{Rdy, Session, WriteRequest};
    use rdy_model::Role;
    use serde_json::{Value, json};
    use std::cell::RefCell;

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

    /// The store, with each blob state and byte read counted by hash.
    struct Counting<'a> {
        rdy: &'a ReadState,
        states: RefCell<BTreeMap<Hash, u32>>,
        bytes: RefCell<BTreeMap<Hash, u32>>,
    }

    impl BlobReads for Counting<'_> {
        fn state(&self, h: &Hash) -> Result<BlobState, RdyError> {
            *self.states.borrow_mut().entry(*h).or_default() += 1;
            self.rdy.state(h)
        }

        fn bytes(&self, h: &Hash) -> Result<Vec<u8>, RdyError> {
            *self.bytes.borrow_mut().entry(*h).or_default() += 1;
            self.rdy.bytes(h)
        }
    }

    #[test]
    fn dgm2_one_blob_read_per_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
        let shared = r.store_mut().put_blob(b"shared figure bytes").unwrap();
        let old = r.store_mut().put_blob(b"history figure bytes").unwrap();
        let fig = |id: &str, parent: &str, blob: &Hash| {
            json!({"op": "create", "type": "figure", "id": id, "parent": parent,
                "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A figure"}})
        };
        // the shared blob in two docs, and a blob only the tag's history shows
        let ids = write(
            &mut r,
            "w1",
            json!([
                {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "Doc A", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "A words."},
                {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "Doc B", "doc_type": "page"}},
                fig("$fa", "$a", &shared),
                fig("$fb", "$b", &shared),
                fig("$fo", "$a", &old),
            ]),
        );
        land(&mut r, "w1");
        r.tag(&session(Role::Approver), "v1.0.0", None, None)
            .expect("tag");
        write(&mut r, "w2", json!([{"op": "delete", "id": ids["$fo"]}]));
        land(&mut r, "w2");
        // a view under review: its base (canon), the tag and canon are the history sides
        write(
            &mut r,
            "work",
            json!([{"op": "update", "id": ids["$ap"], "body": "A words, edited."}]),
        );
        let st = r.read();
        let reads = Counting {
            rdy: &st,
            states: RefCell::default(),
            bytes: RefCell::default(),
        };
        let scope = Scope::View("work".into());
        for build in 1..=2u32 {
            let p = source_reading(&st, &scope, Purpose::Site, &reads).expect("prepare");
            assert!(p.history.since.is_some(), "the tag's history is read");
            assert_eq!(p.blobs.bytes.keys().collect::<Vec<_>>(), vec![&shared]);
            // one byte read and one metadata read of the shared blob per build; the history-only
            // blob's state is read once and its bytes never
            assert_eq!(
                *reads.bytes.borrow(),
                BTreeMap::from([(shared, build)]),
                "build {build}"
            );
            assert_eq!(
                *reads.states.borrow(),
                BTreeMap::from([(shared, build), (old, build)]),
                "build {build}"
            );
        }
    }
}
