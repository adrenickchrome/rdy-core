//! Source-existence facts (ck3, D-JQD5X, D-4WC6R, D-Z8P09; O3, O16): whether a source's release
//! tag or commit exists in a linked local checkout, as the read-time fact `found`, `missing` or
//! `unchecked`.
//!
//! The fact depends on the machine, so it is never stored. It reaches briefings, `rdy_context`
//! and the displayed coverage ([`ReadState::display_coverage_issues`]) and nothing else: not the
//! gate, the assessment, a token or the intrinsic coverage. A read runs local `git` only: no
//! network, no fetch and no shell. A failed probe is `unchecked`; it never fails the read.
use crate::code_index::{
    CodeRepo, Limits, check_top, commit_exists_with, git_command, is_hash, run_with,
};
use crate::coverage::{BlockEntry, CoverageIssue, CoverageIssues, Dimension, IssueCode};
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::{Block, BlockId};
use rdy_schema::supporting::{REFLECTS_LINK, SOURCE};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

/// At most this many distinct `(kind, locator)` values are probed in one read; further values
/// are `unchecked` (O3).
pub const MAX_SOURCE_PROBES: usize = 256;
/// A locator longer than this many bytes is `unchecked`, before any command is built (O3).
pub const MAX_LOCATOR_BYTES: usize = 4096;
/// A probe keeps at most this many bytes of git's stdout and of its stderr (O16).
pub const PROBE_OUTPUT_LIMIT: usize = 65_536;

const PROBE: Limits = Limits {
    stdout: PROBE_OUTPUT_LIMIT,
    stderr: PROBE_OUTPUT_LIMIT,
};

/// The fix of `source_not_found`.
pub(crate) const FIX_SOURCE_NOT_FOUND: &str =
    "check the source's locator, or point the source at a tag or commit that exists";

/// Whether a source's release or commit exists in a local checkout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceExistence {
    /// A linked checkout holds it.
    Found,
    /// Every linked checkout that could be read lacks it.
    Missing,
    /// The state of the machine does not say: a spec, a locator that is no tag or commit, no
    /// readable checkout, a git failure or a read cap.
    Unchecked,
}

/// The existence of each source block of a scope, by block id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SourceFacts {
    pub by_source: BTreeMap<BlockId, SourceExistence>,
}

/// Runs `c` as a probe does, its output cut at [`PROBE_OUTPUT_LIMIT`] on each pipe: `E_LIMIT`
/// past the cut, with the child killed and reaped first.
#[doc(hidden)]
pub fn run_probe_for_tests(c: Command) -> Result<Output, RdyError> {
    run_with(c, PROBE)
}

/// One linked checkout: its configured name and, when its path is a directory, the repository.
struct Checkout {
    name: String,
    repo: Option<CodeRepo>,
    /// Whether the checkout can be read by git as a full clone (checked once per probe).
    readable: Option<bool>,
}

impl Checkout {
    /// The repository when git can read it.
    fn readable(&mut self) -> Option<&CodeRepo> {
        let repo = self.repo.as_ref()?;
        let ok = *self.readable.get_or_insert_with(|| check_top(repo).is_ok());
        ok.then_some(repo)
    }
}

/// The probes of one read: the linked checkouts (read once), the values already answered and the
/// count of distinct values probed so far.
pub(crate) struct SourceProbe {
    root: PathBuf,
    checkouts: Option<Vec<Checkout>>,
    answered: HashMap<(String, String), SourceExistence>,
    probed: usize,
}

impl SourceProbe {
    pub(crate) fn new(root: &Path) -> SourceProbe {
        SourceProbe {
            root: root.to_path_buf(),
            checkouts: None,
            answered: HashMap::new(),
            probed: 0,
        }
    }

    /// The linked checkouts in `code_repos` order; none when the configuration cannot be read.
    fn checkouts(&mut self) -> &mut Vec<Checkout> {
        let root = &self.root;
        self.checkouts.get_or_insert_with(|| {
            let user = crate::config_levels::user_file_from_env();
            let Ok((linked, _warnings)) = crate::config_levels::linked_repos(root, user.as_deref())
            else {
                return Vec::new();
            };
            linked
                .into_iter()
                .map(|l| {
                    let repo = std::fs::canonicalize(&l.path)
                        .ok()
                        .filter(|p| p.is_dir())
                        .map(|root| CodeRepo {
                            configured: l.name.clone(),
                            root,
                        });
                    Checkout {
                        name: l.name,
                        repo,
                        readable: None,
                    }
                })
                .collect()
        })
    }

    /// The existence of the source block `b`.
    pub(crate) fn existence_of(&mut self, b: &Block) -> SourceExistence {
        let kind = b.field_str("kind").unwrap_or_default();
        let locator = b.field_str("locator").unwrap_or_default();
        self.existence(kind, locator)
    }

    /// The existence of `(kind, locator)`. A spec, a locator past [`MAX_LOCATOR_BYTES`] and a
    /// malformed one are `unchecked` without a probe. The first [`MAX_SOURCE_PROBES`] distinct
    /// values that need a probe get one; the rest are `unchecked`.
    pub(crate) fn existence(&mut self, kind: &str, locator: &str) -> SourceExistence {
        if locator.len() > MAX_LOCATOR_BYTES {
            return SourceExistence::Unchecked;
        }
        let key = (kind.to_string(), locator.to_string());
        if let Some(e) = self.answered.get(&key) {
            return *e;
        }
        let found = match kind {
            "commit" => match locator.split_once('@') {
                Some((repo, hash)) if !repo.is_empty() && is_hash(hash.as_bytes()) => {
                    if self.probed >= MAX_SOURCE_PROBES {
                        return SourceExistence::Unchecked;
                    }
                    self.probed += 1;
                    self.commit(repo, hash)
                }
                _ => SourceExistence::Unchecked,
            },
            "release" if valid_tag_name(locator) => {
                if self.probed >= MAX_SOURCE_PROBES {
                    return SourceExistence::Unchecked;
                }
                self.probed += 1;
                self.release(locator)
            }
            _ => SourceExistence::Unchecked,
        };
        self.answered.insert(key, found);
        found
    }

    /// A commit in the checkout named `repo`.
    fn commit(&mut self, repo: &str, hash: &str) -> SourceExistence {
        let Some(c) = self.checkouts().iter_mut().find(|c| c.name == repo) else {
            return SourceExistence::Unchecked;
        };
        let Some(repo) = c.readable() else {
            return SourceExistence::Unchecked;
        };
        match commit_exists_with(repo, hash, PROBE) {
            Ok(true) => SourceExistence::Found,
            Ok(false) => SourceExistence::Missing,
            Err(_) => SourceExistence::Unchecked,
        }
    }

    /// A release tag in any linked checkout.
    fn release(&mut self, tag: &str) -> SourceExistence {
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut linked = false;
        let mut unreadable = false;
        for c in self.checkouts().iter_mut() {
            if let Some(r) = &c.repo
                && !seen.insert(r.root.clone())
            {
                continue;
            }
            linked = true;
            let Some(repo) = c.readable() else {
                unreadable = true;
                continue;
            };
            match tag_exists(repo, tag) {
                Ok(true) => return SourceExistence::Found,
                Ok(false) => {}
                Err(_) => unreadable = true,
            }
        }
        if linked && !unreadable {
            SourceExistence::Missing
        } else {
            SourceExistence::Unchecked
        }
    }
}

/// Whether `refs/tags/<tag>` exists in `repo`: the exact ref, never a branch or a revision.
fn tag_exists(repo: &CodeRepo, tag: &str) -> Result<bool, RdyError> {
    let mut c = git_command(&repo.root);
    c.args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/tags/{tag}"));
    let out = run_with(c, PROBE)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) if out.stderr.is_empty() => Ok(false),
        _ => Err(RdyError::new(
            crate::E_IO,
            format!("git show-ref failed in code repo {}", repo.configured),
            "check the repository with git fsck",
        )),
    }
}

/// Whether `refs/tags/<name>` is a valid reference name by git's rules (`git check-ref-format`):
/// no empty component, none that starts with `.` or ends with `.lock`, no `..` or `@{`, no
/// control character, space or one of `~ ^ : ? * [ \`, and no trailing `.`.
pub(crate) fn valid_tag_name(name: &str) -> bool {
    if name.is_empty() || name.ends_with('.') || name.contains("..") || name.contains("@{") {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return false;
    }
    name.split('/')
        .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// The source blocks that `id` cites with a `reflects` link or a pinned reference.
fn cited_sources(g: &Graph, m: &Materialized, id: &BlockId) -> BTreeSet<BlockId> {
    g.outgoing(id)
        .iter()
        .filter(|e| is_citation(&e.kind))
        .filter(|e| m.get(&e.to).is_some_and(|t| t.block_type == SOURCE))
        .map(|e| e.to.clone())
        .collect()
}

/// Whether an edge of kind `k` cites a source: a `reflects` link or a reference.
fn is_citation(k: &EdgeKind) -> bool {
    match k {
        EdgeKind::References => true,
        EdgeKind::Link(n) => n == REFLECTS_LINK,
        EdgeKind::Contains => false,
    }
}

/// The displayed entry of `entry`, which cites `cited`: each found source adds a passing
/// Connected check, each missing one that check and one `source_not_found` issue, and an
/// unchecked one nothing. `None` when nothing is added.
fn shown_entry(
    id: &BlockId,
    entry: &BlockEntry,
    cited: &BTreeSet<BlockId>,
    facts: &BTreeMap<BlockId, SourceExistence>,
) -> Option<BlockEntry> {
    let mut shown = entry.clone();
    let mut changed = false;
    for source in cited {
        match facts.get(source) {
            Some(SourceExistence::Found) => {
                shown.check(Dimension::Connected);
                changed = true;
            }
            Some(SourceExistence::Missing) => {
                shown.check(Dimension::Connected);
                shown.issues.push(CoverageIssue::new(
                    id,
                    IssueCode::SourceNotFound,
                    Some(source.to_string()),
                    FIX_SOURCE_NOT_FOUND.to_string(),
                ));
                changed = true;
            }
            _ => {}
        }
    }
    if !changed {
        return None;
    }
    crate::coverage::sort_issues(&mut shown.issues, &BTreeMap::new());
    Some(shown)
}

/// What a briefing's coverage reads on top of its entries: the displayed entries of the blocks
/// it has asked for, and the probe they share.
#[derive(Default)]
pub(crate) struct DisplayOverlay {
    probe: Option<SourceProbe>,
    /// The blocks looked at, with their displayed entry when it differs from the intrinsic one.
    looked: BTreeMap<BlockId, Option<BlockEntry>>,
}

impl DisplayOverlay {
    /// The displayed entry of `id`, when it differs from the intrinsic one.
    pub(crate) fn get(&self, id: &BlockId) -> Option<&BlockEntry> {
        self.looked.get(id)?.as_ref()
    }

    /// Looks at `ids` in `scope`; `entry_of` gives the intrinsic entry of a scored block.
    pub(crate) fn overlay<'a>(
        &mut self,
        st: &ReadState,
        scope: &Scope,
        ids: impl IntoIterator<Item = &'a BlockId>,
        entry_of: impl Fn(&BlockId) -> Option<BlockEntry>,
    ) -> Result<(), RdyError> {
        let m = st.materialized(scope)?;
        let g = st.graph(scope)?;
        let mut cites: BTreeMap<BlockId, (BlockEntry, BTreeSet<BlockId>)> = BTreeMap::new();
        for id in ids {
            if self.looked.contains_key(id) {
                continue;
            }
            self.looked.insert(id.clone(), None);
            let cited = cited_sources(&g, &m, id);
            if cited.is_empty() {
                continue;
            }
            if let Some(entry) = entry_of(id) {
                cites.insert(id.clone(), (entry, cited));
            }
        }
        if cites.is_empty() {
            return Ok(());
        }
        let probe = self
            .probe
            .get_or_insert_with(|| SourceProbe::new(st.root()));
        let wanted: BTreeSet<&BlockId> = cites.values().flat_map(|(_, c)| c).collect();
        let mut facts = BTreeMap::new();
        for s in wanted {
            if let Some(b) = m.get(s) {
                facts.insert(s.clone(), probe.existence_of(b));
            }
        }
        for (id, (entry, cited)) in cites {
            let shown = shown_entry(&id, &entry, &cited, &facts);
            self.looked.insert(id, shown);
        }
        Ok(())
    }
}

impl ReadState {
    /// The existence of each source block of `scope`: a local, offline read of the linked
    /// checkouts. `unchecked` past [`MAX_SOURCE_PROBES`] distinct values in source-id order.
    pub fn source_facts(&self, scope: &Scope) -> Result<SourceFacts, RdyError> {
        let m = self.materialized(scope)?;
        let mut sources: Vec<&Block> = m.iter().filter(|b| b.block_type == SOURCE).collect();
        sources.sort_by(|a, b| a.id.cmp(&b.id));
        let mut probe = SourceProbe::new(self.root());
        Ok(SourceFacts {
            by_source: sources
                .into_iter()
                .map(|b| (b.id.clone(), probe.existence_of(b)))
                .collect(),
        })
    }

    /// The existence of the source block `block`, none for another type (a source context shows it).
    pub(crate) fn source_existence_of(&self, block: &Block) -> Option<SourceExistence> {
        (block.block_type == SOURCE).then(|| SourceProbe::new(self.root()).existence_of(block))
    }

    /// The coverage issues of `scope` as a read shows them: the intrinsic issues with the source
    /// facts added. For every scored block that cites a source with a `reflects` link or a pinned
    /// reference, each distinct source adds a passing Connected check when found, and that check
    /// and a `source_not_found` issue when missing; an unchecked source adds nothing. The
    /// intrinsic memo is untouched: the gate, the assessment and the readiness read
    /// [`ReadState::coverage_issues`]. `E_BAD_INPUT` for a past landing.
    pub fn display_coverage_issues(&self, scope: &Scope) -> Result<Arc<CoverageIssues>, RdyError> {
        let scope = &self.coverage_scope(scope)?;
        let base = self.coverage_issues(scope)?;
        let m = self.materialized(scope)?;
        let mut sources: Vec<&Block> = m.iter().filter(|b| b.block_type == SOURCE).collect();
        if sources.is_empty() {
            return Ok(base);
        }
        sources.sort_by(|a, b| a.id.cmp(&b.id));
        let g = self.graph(scope)?;
        // the scored blocks that cite each source, read from the sources' incoming edges
        let mut cites: BTreeMap<BlockId, BTreeSet<BlockId>> = BTreeMap::new();
        for s in &sources {
            for e in g.incoming(&s.id) {
                if is_citation(&e.kind) && base.blocks.contains_key(&e.from) {
                    cites
                        .entry(e.from.clone())
                        .or_default()
                        .insert(s.id.clone());
                }
            }
        }
        if cites.is_empty() {
            return Ok(base);
        }
        let mut probe = SourceProbe::new(self.root());
        let facts: BTreeMap<BlockId, SourceExistence> = sources
            .iter()
            .filter(|s| cites.values().any(|c| c.contains(&s.id)))
            .map(|s| (s.id.clone(), probe.existence_of(s)))
            .collect();
        let mut shown = (*base).clone();
        for (id, cited) in &cites {
            if let Some(entry) = base.blocks.get(id)
                && let Some(e) = shown_entry(id, entry, cited, &facts)
            {
                shown.blocks.insert(id.clone(), e);
            }
        }
        Ok(Arc::new(shown))
    }
}
