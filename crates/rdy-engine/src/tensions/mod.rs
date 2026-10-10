//! Deterministic tensions: one kind-agnostic core (sides, ids, vkeys, severities, the tension
//! set and its info) and one detector per kind.
//!
//! Extension point. A new deterministic kind needs exactly three things: a `TensionKind` variant
//! in rdy-schema (its wire name, `default_severity` and `softens_in_draft`); one detector file in
//! this directory that implements [`Detector`] and reads what it needs from [`DetectCx`]; and
//! one entry in [`DETECTORS`]. Nothing else names a kind.
//!
//! Two detection scopes (FW-CH6). Every detector runs in [`DetectScope::Inflight`] (it sees the
//! live in-flight writes; a `Found` is kept only when it has a change side) and in
//! [`DetectScope::Canon`] (it sees no in-flight write; a `Found` is kept only when every side is
//! canon). The tension set is the union of both, by id. A kind reports a canon form only when it
//! describes a standing condition of canon (fork, duplicate definition and deprecated reference;
//! Coverage and confidence adds value disagreement and restatement drift), and does so when
//! `cx.scope()` is `DetectScope::Canon`. The canon part is memoised per canon build, so a
//! canon-scope run must read canon only: there `cx.writes` is empty, `DetectCx::inflight_m` and
//! `DetectCx::inflight_graph` return canon's, and a detector must not read the published
//! state's in-flight set, views or coordination refs through `cx.st`. The canon part is always
//! a full run (no seeds). An index a new kind adds to `DetectCx` is lazy (a `OnceLock`), so the
//! kinds that do not read it pay nothing.
pub mod api;
pub mod collision;
pub mod delta;
pub mod deprecated;
pub mod dismiss;
pub mod duplicate;
pub mod fork;
pub mod index;
pub mod info;
pub mod overlap;
pub mod premise;
pub mod set;
pub mod unjustified;

pub use dismiss::{DismissInfo, MAX_REASON};
pub use index::{
    CanonIndex, ChangeFacts, DetectCx, Write, Writes, normalize_name, normalize_text,
    pointing_edge, premise_edge,
};
pub use info::{DismissalInfo, SideBlock, SideInfo, TensionInfo, TensionStatus};
pub use rdy_schema::{TensionKind, TensionSeverity};
pub use set::{TensionDelta, TensionSet, tension_delta};

use crate::RdyError;
use rdy_model::{BlockId, Hash};
use rdy_schema::Pack;
use std::collections::BTreeSet;

/// One side of a tension: an in-flight change, canon (at the landing that carried the involved
/// version, when known), or an open intent (read-time overlaps only). Ordered in declaration
/// order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SideRef {
    Change(Hash),
    Canon { landing: Option<Hash> },
    Intent { stable: Hash, version: Hash },
}

impl SideRef {
    /// The full label: a change's 64 hex, `canon@<landing hex>` or `canon`, `intent:in-<12 hex>`.
    pub fn label(&self) -> String {
        match self {
            SideRef::Change(h) => h.to_string(),
            SideRef::Canon { landing: Some(l) } => format!("canon@{l}"),
            SideRef::Canon { landing: None } => "canon".to_string(),
            SideRef::Intent { stable, .. } => format!("intent:in-{}", stable.short()),
        }
    }

    /// The short label: `<12 hex>`, `canon@<12 hex>`, `canon`, `intent:in-<12 hex>`.
    pub fn short(&self) -> String {
        match self {
            SideRef::Change(h) => h.short(),
            SideRef::Canon { landing: Some(l) } => format!("canon@{}", l.short()),
            SideRef::Canon { landing: None } => "canon".to_string(),
            SideRef::Intent { stable, .. } => format!("intent:in-{}", stable.short()),
        }
    }
}

/// One block a tension involves, on one side, at one version (`None`: deleted or absent; for an
/// intent side, the intent's version), and that version's content hash
/// ([`rdy_model::Block::content_hash_with`] the pack's tracking fields; `None` when the version
/// is; for an intent side, the intent's version). The id hashes the version; the vkey hashes the
/// content.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Involved {
    pub side: SideRef,
    pub block: BlockId,
    pub version: Option<Hash>,
    pub content: Option<Hash>,
}

/// What a detector reports: the core turns it into a [`Tension`] (or drops it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub kind: TensionKind,
    pub involved: Vec<Involved>,
    pub summary: String,
    /// Whether the content involved is stable (a kind that softens in draft is advisory when
    /// this is false and the pack does not set its severity).
    pub stable: bool,
}

/// A tension: its id (kind and involved items), its vkey (the same without side labels, with
/// content hashes for versions and with the severity, which a dismissal binds to), its severity, its distinct sides, sorted, and what
/// it involves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tension {
    pub id: Hash,
    pub vkey: Hash,
    pub kind: TensionKind,
    pub severity: TensionSeverity,
    /// Whether the pack set the severity (else the kind's default, or its draft softening).
    pub overridden: bool,
    pub sides: Vec<SideRef>,
    pub involved: Vec<Involved>,
    pub summary: String,
    /// A read-time tension (an intent overlap): never in the event-derived set.
    pub read_time: bool,
}

impl Tension {
    /// The blocks involved, sorted and distinct.
    pub fn blocks(&self) -> Vec<BlockId> {
        let set: BTreeSet<&BlockId> = self.involved.iter().map(|i| &i.block).collect();
        set.into_iter().cloned().collect()
    }
}

/// The version or content part of an involved item as hashed: its hex, or `-` when absent.
fn version_text(v: &Option<Hash>) -> String {
    v.map_or_else(|| "-".to_string(), |h| h.to_string())
}

/// A tension's id: the hash of the kind, the severity and its involved items (side label,
/// block, version), sorted, so the order a detector reports them in does not matter. The
/// severity is in it as in the vkey, so id and vkey stay one-to-one: two forms of one condition
/// that differ in severity (the in-flight form and a landing's form, read under different
/// lifecycles) are two tensions, each with its own id, status and dismissal.
pub fn tension_id(kind: TensionKind, severity: TensionSeverity, involved: &[Involved]) -> Hash {
    let mut items: Vec<&Involved> = involved.iter().collect();
    items.sort();
    let mut bytes = b"rdy-tension-2\0".to_vec();
    for part in [kind.name().to_string(), severity.to_string()] {
        bytes.extend_from_slice(part.as_bytes());
        bytes.push(0);
    }
    for i in items {
        for part in [
            i.side.label(),
            i.block.to_string(),
            version_text(&i.version),
        ] {
            bytes.extend_from_slice(part.as_bytes());
            bytes.push(0);
        }
    }
    Hash::of(&bytes)
}

/// A tension's vkey (FW-CH7): the kind, the severity, and each involved item's block and
/// content hash (items sorted by block, then content; `-` for a deleted or absent block). It
/// leaves out the side labels and the full versions, so only a change of a side's content gives
/// a new one. A landing, which changes a side's label and accepts a proposed decision
/// (`status`, `decided_at`), keeps it; so do a repin and a tracking-field edit, which
/// [`rdy_model::Block::content_hash_with`] leaves out. A change of severity (a lifecycle or
/// pack change) gives a new one, so a dismissal made under one severity does not hold under
/// another.
pub fn tension_vkey(kind: TensionKind, severity: TensionSeverity, involved: &[Involved]) -> Hash {
    let mut items: Vec<(&BlockId, &Option<Hash>)> =
        involved.iter().map(|i| (&i.block, &i.content)).collect();
    items.sort();
    let mut bytes = b"rdy-tension-vkey-2\0".to_vec();
    for part in [kind.name().to_string(), severity.to_string()] {
        bytes.extend_from_slice(part.as_bytes());
        bytes.push(0);
    }
    for (block, content) in items {
        for part in [block.to_string(), version_text(content)] {
            bytes.extend_from_slice(part.as_bytes());
            bytes.push(0);
        }
    }
    Hash::of(&bytes)
}

/// A tension's display id: `tn-` and the first 12 hex of its id.
pub fn tension_label(id: &Hash) -> String {
    format!("tn-{}", id.short())
}

/// The severity of what `f` reports under `pack`, and whether the pack set it. A kind that
/// softens in draft is advisory when the pack leaves it at its default and the content involved
/// is not stable.
pub fn severity_of(pack: &Pack, f: &Found) -> (TensionSeverity, bool) {
    let (severity, overridden) = pack.tension_severity(f.kind);
    if !overridden && f.kind.softens_in_draft() && !f.stable {
        (TensionSeverity::Advisory, false)
    } else {
        (severity, overridden)
    }
}

/// The scope a detector runs in (FW-CH6): the live in-flight writes against canon, or canon
/// alone (no in-flight write; standing conditions of canon).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectScope {
    Inflight,
    Canon,
}

/// The tension `f` describes, or `None` when the scope does not keep it: in the in-flight scope a
/// tension needs a change side; in the canon scope every side must be canon. Its involved items
/// are sorted and deduplicated.
pub(crate) fn tension_from(
    pack: &Pack,
    f: Found,
    read_time: bool,
    scope: DetectScope,
) -> Option<Tension> {
    let keep = match scope {
        DetectScope::Inflight => f
            .involved
            .iter()
            .any(|i| matches!(i.side, SideRef::Change(_))),
        DetectScope::Canon => f
            .involved
            .iter()
            .all(|i| matches!(i.side, SideRef::Canon { .. })),
    };
    if !keep || f.involved.is_empty() {
        return None;
    }
    let (severity, overridden) = severity_of(pack, &f);
    let mut involved = f.involved;
    involved.sort();
    involved.dedup();
    let sides: BTreeSet<SideRef> = involved.iter().map(|i| i.side.clone()).collect();
    Some(Tension {
        id: tension_id(f.kind, severity, &involved),
        vkey: tension_vkey(f.kind, severity, &involved),
        kind: f.kind,
        severity,
        overridden,
        sides: sides.into_iter().collect(),
        involved,
        summary: f.summary,
        read_time,
    })
}

/// One kind's detector. `detect` appends what it finds to `out`; with `seeds`, it may report only
/// what involves one of those blocks (the core drops anything else).
pub trait Detector: Sync {
    fn kind(&self) -> TensionKind;
    /// The blocks besides `seeds` whose tensions of this kind a change on `seeds` can open or
    /// resolve (in flight). A carry also drops this kind's tensions that involve one of them and
    /// re-detects with them as seeds. A group kind (duplicate definition, FW-TN2) names the
    /// other members of every group a seed was or is in. Default: none.
    fn reach(
        &self,
        _cx: &DetectCx<'_>,
        _seeds: &BTreeSet<BlockId>,
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        Ok(BTreeSet::new())
    }
    /// Whether a proposal lists this kind only in its landing form: the proposal's net change is
    /// judged, never an in-flight form of another view or of changes the proposal leaves out
    /// (FW-JU1, FW-LP3). Default: false.
    fn judged_on_the_net_change(&self) -> bool {
        false
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError>;
}

/// Whether a proposal lists `kind` only in its landing form (see
/// [`Detector::judged_on_the_net_change`]).
pub fn judged_on_the_net_change(kind: TensionKind) -> bool {
    DETECTORS
        .iter()
        .any(|d| d.kind() == kind && d.judged_on_the_net_change())
}

/// The registry: one detector per kind, in `TensionKind::ALL` order.
pub static DETECTORS: [&dyn Detector; 7] = [
    &collision::EditCollision,
    &fork::Fork,
    &premise::PremiseShift,
    &overlap::AreaOverlap,
    &duplicate::DuplicateDefinition,
    &unjustified::UnjustifiedChange,
    &deprecated::DeprecatedReference,
];
