//! Linked reads (repo links, D-DAPR8, D-MEMMQ): what a project reads of the content it cites from
//! other projects, and the facts a briefing adds for it.
//!
//! - [`ReadState::links`], [`ReadState::link_show`] and [`ReadState::link_refs`] read canon's repo
//!   links and the foreign cut. They never refuse a write and write nothing.
//! - A link has a state: `none` (no delivery yet), `delivered`, or `revoked` (its last view is
//!   frozen). A delivered or revoked link gives the tag, profile version and time of its last
//!   delivery, and `pins_behind`: the cited blocks of its repo that canon pins at a version
//!   other than the one the repo's view delivers now, or that no delivery selects. A pin of an
//!   adopted instruction cites nothing and is never behind. A delivered repo that no live link
//!   names is `unlinked`.
//! - `show` lists what one alias delivers, its profile's selection alone, and the instructions
//!   the link has not adopted (D-MEMMQ): `added`, `changed` or `removed` against the pins of its
//!   `repo_link` block.
//! - `refs` lists the pins of canon to cited content, by repo.
//! - [`read_cited`] is `rdy_read` of `<alias>:<id>`: the cited block read-only, with its provenance
//!   and the versions the project's pins hold. A block the selection left is still read through a
//!   pin that holds it.
//! - [`cited_governs`] and [`brief_links`] give a briefing its cited `governs` rows and its
//!   `links` rows. Nothing here changes a briefing's token: a delivery moves no pin.
use crate::brief::{BriefLink, Cited, DeviationRow, Governs};
use crate::error::{E_BAD_INPUT, E_NOT_FOUND, E_UNKNOWN_REF};
use crate::foreign::ForeignState;
use crate::foreign_refs::{
    ForeignCite, RepoLinks, instruction_baseline, not_delivered, unknown_alias,
};
use crate::pending::adopt::{AdoptChange, adopted_instructions, instruction_rows};
use crate::query::{compact_summary, source_body, summary};
use crate::tool_reads::{Pattern, ReadEntries, markdown, scope_of};
use crate::{Materialized, RdyError, ReadState, Scope, block_to_yaml, rfc3339};
use rdy_model::foreign::{ForeignEvent, ForeignId, ForeignPin, ForeignSource};
use rdy_model::reference::ForeignRef;
use rdy_model::{Block, BlockId, Hash, RepoId, decision};
use rdy_store::foreign::{HeldVersion, LINK_REF_PREFIX, SourceView};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// The most rows `show` and `refs` list; `omitted` counts the rest.
pub const ROWS_MAX: usize = 10_000;

/// The most `links` rows a briefing carries; `links_omitted` counts the rest.
pub const BRIEF_LINKS_CAP: usize = 100;

/// The hex digits of a version a read line shows.
const SHORT: usize = 12;

/// Where a link stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// No delivery has arrived.
    None,
    Delivered,
    /// A revocation ended the source: its last view stays readable and no delivery is taken.
    Revoked,
}

/// The last delivery of a source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LastDelivery {
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// The delivery's time in milliseconds, to order sources; not part of a reply.
    #[serde(skip)]
    at_ms: Option<i64>,
}

/// One live `repo_link` block and what its source delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkInfo {
    pub alias: String,
    /// The `repo_link` block.
    pub link: String,
    pub repo: String,
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: LinkState,
    #[serde(flatten)]
    pub last: Option<LastDelivery>,
    pub pins_behind: usize,
}

/// A delivered repo that no live `repo_link` block names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnlinkedRepo {
    pub repo: String,
    pub state: LinkState,
    #[serde(flatten)]
    pub last: Option<LastDelivery>,
    pub pins_behind: usize,
}

/// The reply of `rdy_links {op: "list"}`: every live link in alias order, then the unlinked repos.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinksList {
    pub links: Vec<LinkInfo>,
    pub unlinked: Vec<UnlinkedRepo>,
}

/// One block a link delivers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ShownBlock {
    pub id: String,
    pub block_type: String,
    pub version: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub superseded_by: Vec<String>,
    /// `current` when canon pins the delivered content, `behind` when a pin holds other content;
    /// absent when canon does not cite the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<&'static str>,
}

/// An instruction the link has not adopted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PendingInstruction {
    pub id: String,
    pub change: AdoptChange,
    /// The delivered version, or the version in force when the delivery removed it.
    pub version: String,
    pub summary: String,
}

/// The reply of `rdy_links {op: "show", alias}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkShow {
    pub link: LinkInfo,
    pub blocks: Vec<ShownBlock>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingInstruction>,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
}

/// One pinned version of one cited block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinRow {
    pub target: String,
    /// The version the pin holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// The version the repo's view delivers now; absent when no delivery selects the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered: Option<String>,
    pub behind: bool,
    /// The blocks that hold this pin, in id order.
    pub cited_by: Vec<String>,
}

/// The pins to one repo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RepoRefs {
    pub alias: String,
    pub repo: String,
    pub pins: Vec<PinRow>,
}

/// The reply of `rdy_links {op: "refs"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkRefs {
    pub repos: Vec<RepoRefs>,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// `E_NOT_FOUND`: no live repo link has `alias`.
fn not_found(alias: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("no repo link has the alias {alias}"),
        "list the links: rdy_links / rdy links",
    )
}

/// One pin of canon to cited content.
struct Pinned<'a> {
    target: &'a ForeignId,
    citer: &'a BlockId,
    pin: &'a ForeignPin,
}

/// The pins of `m` to cited content. A repo link's pins of its instructions cite nothing.
fn pinned(m: &Materialized) -> Vec<Pinned<'_>> {
    let mut out = vec![];
    for b in m
        .iter()
        .filter(|b| !b.foreign.pins.is_empty() && !b.is_redacted())
    {
        let baseline: BTreeSet<&ForeignId> = instruction_baseline(b).map(|(t, _)| t).collect();
        for (target, pin) in b.foreign.pins.iter().filter(|(t, _)| !baseline.contains(t)) {
            out.push(Pinned {
                target,
                citer: &b.id,
                pin,
            });
        }
    }
    out
}

/// What the live sources of `repo` deliver now, per block, newest in log order; with no live
/// source, what its revoked sources last delivered.
fn repo_view<'a>(
    foreign: &'a ForeignState,
    repo: &RepoId,
) -> BTreeMap<&'a BlockId, &'a HeldVersion> {
    let of = |live: bool| {
        let mut out: BTreeMap<&BlockId, &HeldVersion> = BTreeMap::new();
        for (_, view) in foreign.sources() {
            if view.repo != *repo || view.revoked == live {
                continue;
            }
            for (id, held) in &view.members {
                match out.get(id) {
                    Some(have) if have.at >= held.at => {}
                    _ => {
                        out.insert(id, held);
                    }
                }
            }
        }
        out
    };
    let live = of(true);
    if live.is_empty() { of(false) } else { live }
}

/// Whether `pin` holds other content than `held` delivers, or `held` is gone.
fn behind(pin: &ForeignPin, held: Option<&HeldVersion>) -> bool {
    match held {
        Some(h) => (h.version.content, h.version.encoding) != (pin.content, pin.encoding),
        None => true,
    }
}

/// The last delivery of the source `view`.
fn last_delivery(st: &ReadState, view: &SourceView) -> Result<LastDelivery, RdyError> {
    let reader = st.reader();
    let mut entry = reader.foreign_entry(&view.head)?;
    if let ForeignEvent::Revoked { prev } = &entry.event {
        entry = reader.foreign_entry(prev)?;
    }
    let at_ms = match entry.source {
        ForeignSource::Link { at_ms, .. } => Some(at_ms),
        ForeignSource::Adr { .. } => None,
    };
    Ok(LastDelivery {
        tag: view.publication.tag.clone(),
        profile_version: view.publication.profile_version.map(|h| h.to_string()),
        delivered_at: at_ms.map(rfc3339),
        at_ms,
    })
}

/// The canon and foreign facts every linked read starts from.
struct Cut<'a> {
    st: &'a ReadState,
    m: std::sync::Arc<Materialized>,
    foreign: &'a ForeignState,
    links: RepoLinks,
}

impl<'a> Cut<'a> {
    fn new(st: &'a ReadState) -> Result<Cut<'a>, RdyError> {
        let m = st.materialized(&Scope::Canon)?;
        let links = RepoLinks::of_view(&m);
        Ok(Cut {
            st,
            m,
            foreign: st.foreign()?,
            links,
        })
    }

    /// The pins of canon to each repo.
    fn pins(&self) -> BTreeMap<RepoId, Vec<Pinned<'_>>> {
        let mut out: BTreeMap<RepoId, Vec<Pinned<'_>>> = BTreeMap::new();
        for p in pinned(&self.m) {
            out.entry(p.target.repo).or_default().push(p);
        }
        out
    }

    /// How many cited blocks of `repo` have a pin that is behind.
    fn pins_behind(&self, repo: &RepoId, pins: &BTreeMap<RepoId, Vec<Pinned<'_>>>) -> usize {
        let Some(own) = pins.get(repo) else {
            return 0;
        };
        let view = repo_view(self.foreign, repo);
        own.iter()
            .filter(|p| behind(p.pin, view.get(&p.target.id).copied()))
            .map(|p| &p.target.id)
            .collect::<BTreeSet<_>>()
            .len()
    }

    fn info(
        &self,
        alias: &str,
        row: &crate::foreign_refs::LinkRow,
        pins: &BTreeMap<RepoId, Vec<Pinned<'_>>>,
    ) -> Result<LinkInfo, RdyError> {
        let view = self.foreign.link_source(&row.link);
        let (state, last) = match view {
            None => (LinkState::None, None),
            Some(v) => (
                if v.revoked {
                    LinkState::Revoked
                } else {
                    LinkState::Delivered
                },
                Some(last_delivery(self.st, v)?),
            ),
        };
        Ok(LinkInfo {
            alias: alias.to_string(),
            link: row.link.to_string(),
            repo: row.repo.to_string(),
            profile: row.profile.clone(),
            title: self
                .m
                .get(&row.link)
                .and_then(|b| b.field_str("title"))
                .map(str::to_string),
            state,
            last,
            pins_behind: self.pins_behind(&row.repo, pins),
        })
    }
}

impl ReadState {
    /// The repo links of canon with their state, and the delivered repos no live link names.
    pub fn links(&self) -> Result<LinksList, RdyError> {
        let cut = Cut::new(self)?;
        let pins = cut.pins();
        let mut links = vec![];
        for (alias, row) in cut.links.rows() {
            links.push(cut.info(alias, row, &pins)?);
        }
        let linked: BTreeSet<RepoId> = cut.links.rows().map(|(_, r)| r.repo).collect();
        let mut by_repo: BTreeMap<RepoId, Vec<&SourceView>> = BTreeMap::new();
        for (name, view) in cut.foreign.sources() {
            if name.starts_with(LINK_REF_PREFIX) && !linked.contains(&view.repo) {
                by_repo.entry(view.repo).or_default().push(view);
            }
        }
        let mut unlinked = vec![];
        for (repo, views) in by_repo {
            // the newest live source speaks for the repo; with none live, the newest revoked one
            let live = views.iter().any(|v| !v.revoked);
            let mut best: Option<(Option<i64>, Hash, LastDelivery)> = None;
            for v in views.iter().filter(|v| v.revoked != live) {
                let last = last_delivery(self, v)?;
                let key = (last.at_ms, v.head);
                if best.as_ref().is_none_or(|(at, head, _)| key > (*at, *head)) {
                    best = Some((last.at_ms, v.head, last));
                }
            }
            unlinked.push(UnlinkedRepo {
                repo: repo.to_string(),
                state: if live {
                    LinkState::Delivered
                } else {
                    LinkState::Revoked
                },
                last: best.map(|(_, _, l)| l),
                pins_behind: cut.pins_behind(&repo, &pins),
            });
        }
        Ok(LinksList { links, unlinked })
    }

    /// What the link `alias` delivers: its profile's selection (a revoked link's frozen view),
    /// and the instructions it has not adopted. `E_NOT_FOUND` for an alias no live link has.
    pub fn link_show(&self, alias: &str) -> Result<LinkShow, RdyError> {
        let cut = Cut::new(self)?;
        let row = cut.links.get(alias).ok_or_else(|| not_found(alias))?;
        let pins = cut.pins();
        let link = cut.info(alias, row, &pins)?;
        let view = cut.foreign.link_source(&row.link);
        let members: Vec<(&BlockId, &HeldVersion)> = match view {
            None => vec![],
            Some(v) if v.revoked => v.members.iter().collect(),
            Some(_) => cut
                .foreign
                .current_for_profile(&row.repo, &row.profile)
                .into_iter()
                .collect(),
        };
        let own = pins.get(&row.repo).map(Vec::as_slice).unwrap_or(&[]);
        let mut blocks = vec![];
        for (id, held) in members.iter().take(ROWS_MAX) {
            let b = self.reader().get_block(&held.version.version)?;
            let mine: Vec<&Pinned<'_>> = own.iter().filter(|p| p.target.id == **id).collect();
            let pin = (!mine.is_empty()).then(|| {
                if mine.iter().any(|p| behind(p.pin, Some(held))) {
                    "behind"
                } else {
                    "current"
                }
            });
            let d = &held.version.derived;
            blocks.push(ShownBlock {
                id: id.to_string(),
                block_type: b.block_type.clone(),
                version: held.version.version.to_string(),
                summary: summary(&b),
                status: d.status.clone(),
                lifecycle: d.lifecycle.clone(),
                superseded_by: d.superseded_by.iter().map(|s| s.to_string()).collect(),
                pin,
            });
        }
        let mut pending = vec![];
        if let Some(v) = view.filter(|v| !v.revoked) {
            let baseline = adopted_instructions(self, &cut.m, &row.link, &row.repo, &v.members)?;
            for r in instruction_rows(&v.members, &baseline)
                .into_iter()
                .take(ROWS_MAX)
            {
                let version = r.new.or(r.old).expect("a row has a version");
                let b = self.reader().get_block(&version)?;
                pending.push(PendingInstruction {
                    id: r.id.to_string(),
                    change: r.change,
                    version: version.to_string(),
                    summary: summary(&b),
                });
            }
        }
        Ok(LinkShow {
            link,
            blocks,
            pending,
            omitted: members.len().saturating_sub(ROWS_MAX),
        })
    }

    /// The pins of canon to cited content, by repo; with `alias`, to its repo alone.
    /// `E_NOT_FOUND` for an alias no live link has.
    pub fn link_refs(&self, alias: Option<&str>) -> Result<LinkRefs, RdyError> {
        let cut = Cut::new(self)?;
        let only = match alias {
            Some(a) => Some(cut.links.get(a).ok_or_else(|| not_found(a))?.repo),
            None => None,
        };
        let pins = cut.pins();
        let mut repos = vec![];
        let mut listed = 0usize;
        let mut omitted = 0usize;
        for (repo, own) in &pins {
            if only.is_some_and(|r| r != *repo) {
                continue;
            }
            let view = repo_view(cut.foreign, repo);
            let mut rows: BTreeMap<(&BlockId, Option<Hash>), PinRow> = BTreeMap::new();
            for p in own {
                let held = self.foreign_held_pin(p.target, p.pin)?;
                let delivered = view.get(&p.target.id).copied();
                let row = rows.entry((&p.target.id, held)).or_insert_with(|| PinRow {
                    target: p.target.id.to_string(),
                    held: held.map(|h| h.to_string()),
                    delivered: delivered.map(|h| h.version.version.to_string()),
                    behind: behind(p.pin, delivered),
                    cited_by: vec![],
                });
                row.cited_by.push(p.citer.to_string());
            }
            let mut pins: Vec<PinRow> = rows.into_values().collect();
            for r in &mut pins {
                r.cited_by.sort();
                r.cited_by.dedup();
            }
            let room = ROWS_MAX.saturating_sub(listed);
            omitted += pins.len().saturating_sub(room);
            pins.truncate(room);
            listed += pins.len();
            if pins.is_empty() {
                continue;
            }
            repos.push(RepoRefs {
                alias: alias.map_or_else(|| cut.links.shown(repo), str::to_string),
                repo: repo.to_string(),
                pins,
            });
        }
        repos.sort_by(|a, b| a.alias.cmp(&b.alias).then(a.repo.cmp(&b.repo)));
        Ok(LinkRefs { repos, omitted })
    }
}

fn short(h: &Hash) -> String {
    h.to_string()[..SHORT].to_string()
}

/// How the delivery in `entry` reached this project.
fn provenance(st: &ReadState, entry: &Hash) -> Result<String, RdyError> {
    Ok(match st.reader().foreign_entry(entry)?.source {
        ForeignSource::Link { link, .. } => format!("delivered by the hub (link {link})"),
        ForeignSource::Adr { source, .. } => format!("synced from {source}"),
    })
}

/// The tag of the delivery in `entry`.
fn tag_of(st: &ReadState, entry: &Hash) -> Result<Option<String>, RdyError> {
    Ok(match st.reader().foreign_entry(entry)?.event {
        ForeignEvent::Delivery { publication, .. } => Some(publication.tag),
        _ => None,
    })
}

/// `rdy_read` of a block cited from a linked project: the cited block read-only, as `format`
/// shows it, with its provenance, the version delivered now and the versions the project's pins
/// hold (in the scope `scope` reads, canon by default). One entry; a `pattern` that matches no
/// line of it keeps none.
pub(crate) fn read_cited(
    st: &ReadState,
    fr: &ForeignRef<'_>,
    view: &str,
    scope: &Option<String>,
    format: &str,
    pattern: Option<&Pattern>,
) -> Result<ReadEntries, RdyError> {
    if !["compact", "markdown", "yaml"].contains(&format) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown format {format:?}"),
            "use compact, markdown or yaml",
        ));
    }
    let scope = scope_of(view, st, scope)?;
    let m = st.materialized(&scope)?;
    let links = RepoLinks::of_view(&m);
    let foreign = st.foreign()?;
    // the cited block, and the version the alias's profile delivers now
    let (fid, current): (ForeignId, Option<&HeldVersion>) = match fr {
        ForeignRef::Alias { alias, target } => {
            let row = links.get(alias).ok_or_else(|| unknown_alias(alias))?;
            let cite = ForeignCite::new(st, &m);
            match cite.resolve(alias, target) {
                Ok((fid, _)) => {
                    let held = foreign
                        .current_for_profile(&row.repo, &row.profile)
                        .get(&fid.id)
                        .copied();
                    (fid, held)
                }
                Err(e) if e.code == E_UNKNOWN_REF && BlockId::is_valid(target) => {
                    let fid = ForeignId::new(row.repo, BlockId::parse(target).expect("valid"));
                    if !m.iter().any(|b| b.foreign.pins.contains_key(&fid)) {
                        return Err(e);
                    }
                    (fid, None)
                }
                Err(e) => return Err(e),
            }
        }
        ForeignRef::Stored { repo, id } => {
            let fid = ForeignId::new(*repo, id.clone());
            let held = foreign.current_version(&fid);
            if held.is_none() && !m.iter().any(|b| b.foreign.pins.contains_key(&fid)) {
                return Err(not_delivered(&links.shown(repo), id.as_str()));
            }
            (fid, held)
        }
    };
    let shown = links.shown(&fid.repo);
    // the versions the pins of the scope hold, with the blocks that hold them
    let mut held: BTreeMap<Hash, BTreeSet<&BlockId>> = BTreeMap::new();
    for b in m.iter() {
        if let Some(pin) = b.foreign.pins.get(&fid)
            && let Some(v) = st.foreign_held_pin(&fid, pin)?
        {
            held.entry(v).or_default().insert(&b.id);
        }
    }
    let (block, carrier): (Block, Hash) = match (current, held.keys().next()) {
        (Some(h), _) => (st.reader().get_block(&h.version.version)?, h.entry),
        (None, Some(v)) => {
            let b = st
                .foreign_held(&fid, v)?
                .ok_or_else(|| not_delivered(&shown, fid.id.as_str()))?;
            let (_, entry) = st
                .foreign_carrier(&fid, v)?
                .ok_or_else(|| not_delivered(&shown, fid.id.as_str()))?;
            (b, entry)
        }
        (None, None) => return Err(not_delivered(&shown, fid.id.as_str())),
    };
    let mut lines = vec![format!("provenance: {}", provenance(st, &carrier)?)];
    match current {
        Some(h) => {
            let tag = tag_of(st, &h.entry)?.unwrap_or_default();
            lines.push(format!("delivered: {} in {tag}", short(&h.version.version)));
            let d = &h.version.derived;
            let mut facts = vec![];
            if let Some(s) = &d.status {
                facts.push(format!("status {s}"));
            }
            if let Some(l) = &d.lifecycle {
                facts.push(format!("lifecycle {l}"));
            }
            if !d.superseded_by.is_empty() {
                let ids: Vec<String> = d.superseded_by.iter().map(|s| s.to_string()).collect();
                facts.push(format!("superseded by {}", ids.join(", ")));
            }
            if !facts.is_empty() {
                lines.push(format!("derived: {}", facts.join(", ")));
            }
        }
        None => lines.push("delivered: no longer delivered".to_string()),
    }
    for (v, by) in &held {
        let by: Vec<String> = by.iter().map(|b| b.to_string()).collect();
        lines.push(format!("pinned: {} ({})", short(v), by.join(", ")));
    }
    let status = current.and_then(|h| h.version.derived.status.as_deref());
    let head = format!(
        "{shown}:{} {}: {}",
        fid.id,
        block.block_type,
        compact_summary(&block)
    );
    let (text, sep) = match format {
        "yaml" => {
            let facts = serde_yaml_ng::to_string(&json!({ "cited": lines })).map_err(|e| {
                RdyError::new(
                    crate::error::E_INTERNAL,
                    format!("a cited block did not serialize: {e}"),
                    "report this as a bug in rdy",
                )
            })?;
            (format!("{}{facts}", block_to_yaml(&block)), "---\n")
        }
        "markdown" => {
            let body = if block.is_redacted() {
                format!("`{}` {}", block.id, source_body(&block))
            } else {
                markdown::block_markdown(&block, &Materialized::default(), None, status)
            };
            let facts: Vec<String> = lines.iter().map(|l| format!("- {l}")).collect();
            (format!("{body}\n\n{}", facts.join("\n")), "\n\n")
        }
        _ => {
            let mut out = vec![head];
            out.extend(lines.iter().map(|l| format!("  {l}")));
            (out.join("\n"), "\n")
        }
    };
    let kept = pattern.is_none_or(|p| text.lines().any(|l| p.matches(l)));
    let (entries, ids) = if kept {
        (vec![text], vec![fid.id.clone()])
    } else {
        (vec![], vec![])
    };
    Ok(ReadEntries {
        root: fid.id.clone(),
        format: format.to_string(),
        matched: pattern.map(|_| entries.len()),
        as_built: vec![None; entries.len()],
        as_built_warnings: vec![None; entries.len()],
        entries,
        ids,
        sep,
        conflict: None,
        warnings: vec![],
    })
}

/// The `governs` rows of the cited decisions that govern covered blocks (`ids` of `m`, read in
/// `scope`): a covered block names a cited decision as `decided_by` or `implements`. One row for
/// each cited decision and version in force (the pin), after the local rows' order: the in-force
/// statement and the blocks it governs, with the delivered version, `newer`, the tag and time of
/// its delivery, and the live deviations from it (their decision and statement as the reason).
pub(crate) fn cited_governs(
    st: &ReadState,
    scope: &Scope,
    m: &Materialized,
    ids: &BTreeSet<BlockId>,
) -> Result<Vec<Governs>, RdyError> {
    let foreign = st.foreign()?;
    if foreign.is_empty() {
        return Ok(vec![]);
    }
    let mut by: BTreeMap<(ForeignId, Hash, u16), (ForeignPin, BTreeSet<BlockId>)> = BTreeMap::new();
    for id in ids {
        let Some(b) = m.get(id) else { continue };
        for kind in [decision::DECIDED_BY, decision::IMPLEMENTS] {
            for l in b.foreign.links.get(kind).into_iter().flatten() {
                let Some(pin) = b.foreign.pins.get(&l.target) else {
                    continue;
                };
                by.entry((l.target.clone(), pin.content, pin.encoding))
                    .or_insert_with(|| (pin.clone(), BTreeSet::new()))
                    .1
                    .insert(id.clone());
            }
        }
    }
    if by.is_empty() {
        return Ok(vec![]);
    }
    let links = RepoLinks::of_view(m);
    let mut deviations: BTreeMap<ForeignId, Vec<DeviationRow>> = BTreeMap::new();
    for d in st.deviations(scope)?.into_iter().filter(|d| d.live) {
        let reason = m.get(&d.decision).map(statement_of).unwrap_or_default();
        deviations.entry(d.target).or_default().push(DeviationRow {
            id: d.decision.to_string(),
            reason,
        });
    }
    let mut out = vec![];
    for ((target, ..), (pin, covered)) in by {
        let Some(in_force) = st.foreign_held_pin(&target, &pin)? else {
            continue;
        };
        let Some(block) = st.foreign_held(&target, &in_force)? else {
            continue;
        };
        if block.block_type != decision::DECISION {
            continue;
        }
        let current = foreign.current_version(&target);
        let newer = current.is_some_and(|h| behind(&pin, Some(h)));
        let (mut tag, mut delivered_at) = (None, None);
        if let Some(h) = current {
            let entry = st.reader().foreign_entry(&h.entry)?;
            if let ForeignEvent::Delivery { publication, .. } = &entry.event {
                tag = Some(publication.tag.clone());
            }
            if let ForeignSource::Link { at_ms, .. } = entry.source {
                delivered_at = Some(rfc3339(at_ms));
            }
        }
        let delivered_statement = match current {
            Some(h) if newer => Some(statement_of(&st.reader().get_block(&h.version.version)?)),
            _ => None,
        };
        let status = match current {
            Some(h) if h.version.version == in_force => h.version.derived.status.clone(),
            _ => None,
        }
        .or_else(|| block.field_str("status").map(str::to_string))
        .unwrap_or_default();
        let alias = links.shown(&target.repo);
        let mut devs = deviations.get(&target).cloned().unwrap_or_default();
        devs.sort_by(|a, b| a.id.cmp(&b.id));
        devs.dedup();
        out.push(Governs {
            decision: format!("{alias}:{}", target.id),
            status,
            statement: statement_of(&block),
            blocks: covered.iter().map(|b| b.to_string()).collect(),
            through: vec![],
            cited: Some(Cited {
                alias,
                repo: target.repo.to_string(),
                in_force: in_force.to_string(),
                delivered: current.map(|h| h.version.version.to_string()),
                newer,
                tag,
                delivered_at,
                delivered_statement,
            }),
            deviations: devs,
        });
    }
    out.sort_by(|a, b| {
        let key = |g: &Governs| {
            (
                g.decision.clone(),
                g.cited.as_ref().map(|c| c.in_force.clone()),
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(out)
}

/// A decision's statement, else its one-line summary; the removal notice for a stub.
fn statement_of(b: &Block) -> String {
    if b.is_redacted() {
        return source_body(b);
    }
    match b.field_str("statement").filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => summary(b),
    }
}

/// The `links` rows of a briefing: one for each repo that covered blocks cite, in alias order,
/// with the state `rdy_links` gives. A repo a live link names shows as its first alias; any
/// other as its first 12 hex digits. At most [`BRIEF_LINKS_CAP`] rows, and how many more.
pub(crate) fn brief_links(
    st: &ReadState,
    m: &Materialized,
    ids: &BTreeSet<BlockId>,
) -> Result<(Vec<BriefLink>, usize), RdyError> {
    let mut repos: BTreeSet<RepoId> = BTreeSet::new();
    for id in ids {
        let Some(b) = m.get(id).filter(|b| !b.foreign.is_empty()) else {
            continue;
        };
        let baseline: BTreeSet<&ForeignId> = instruction_baseline(b).map(|(t, _)| t).collect();
        repos.extend(
            b.foreign
                .pins
                .keys()
                .filter(|t| !baseline.contains(t))
                .map(|t| t.repo),
        );
        for l in b.foreign.links.values().flatten() {
            repos.insert(l.target.repo);
        }
    }
    if repos.is_empty() {
        return Ok((vec![], 0));
    }
    let list = st.links()?;
    let mut rows: Vec<BriefLink> = vec![];
    for repo in repos {
        let hex = repo.to_string();
        let row = if let Some(l) = list.links.iter().find(|l| l.repo == hex) {
            BriefLink::new(&l.alias, &hex, l.state, l.last.as_ref(), l.pins_behind)
        } else if let Some(u) = list.unlinked.iter().find(|u| u.repo == hex) {
            BriefLink::new(&hex[..SHORT], &hex, u.state, u.last.as_ref(), u.pins_behind)
        } else {
            BriefLink::new(&hex[..SHORT], &hex, LinkState::None, None, 0)
        };
        rows.push(row);
    }
    rows.sort_by(|a, b| a.alias.cmp(&b.alias).then(a.repo.cmp(&b.repo)));
    let omitted = rows.len().saturating_sub(BRIEF_LINKS_CAP);
    rows.truncate(BRIEF_LINKS_CAP);
    Ok((rows, omitted))
}

impl BriefLink {
    fn new(
        alias: &str,
        repo: &str,
        state: LinkState,
        last: Option<&LastDelivery>,
        pins_behind: usize,
    ) -> BriefLink {
        BriefLink {
            alias: alias.to_string(),
            repo: repo.to_string(),
            state,
            tag: last.map(|l| l.tag.clone()),
            profile_version: last.and_then(|l| l.profile_version.clone()),
            delivered_at: last.and_then(|l| l.delivered_at.clone()),
            pins_behind,
        }
    }
}
