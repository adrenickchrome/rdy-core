//! The export of one tag's filtered view as a link delivery (repo links, D-DAPR8, D-C94DV), and
//! the foreign facts one tag uses.
//!
//! [`write`] reuses the publication selection at the named tag: [`resolve`] picks the blocks
//! exactly as a publication of the same profile does, and the delivery carries the `BLOCK`
//! objects of the selected versions with the facts a citing project cannot compute (status,
//! lifecycle, the selected blocks that supersede one, the content identity a pin keeps). It reads
//! one [`ReadState`] and the log up to that state's captured end, so a landing, tag or delivery
//! stored later is not in it. It writes no foreign copy and no `TAG` object, and holds only the
//! `BLOCK` objects of the selection, one at a time: the stream goes to `out` as it is read.
//!
//! - A planned block that a hiding profile keeps only as a shell is not delivered: the shell is a
//!   presentation stub, and its `BLOCK` object holds the content the profile hides. The delivery's
//!   `selection` is the publication selection identity (`rdy_model::foreign::selection_identity`)
//!   of the versions it delivers, so it equals the profile's own identity when the profile keeps
//!   no shell.
//! - A version that an approved removal dropped travels as its residue (a `DROPPED` record), with
//!   the content identity the removal recorded and no derived fact.
//! - `superseded_by` lists only selected blocks, so no id outside the selection is disclosed.
//!
//! [`tag_uses`] lists the foreign pins of the blocks a tag holds, each with the version this
//! state holds for it and, while a source still selects that version, the delivery that carried
//! it.
use crate::Scope;
use crate::error::{E_INTERNAL, E_LIMIT, RdyError};
use crate::publication::{profile_facts, resolve};
use crate::read_state::ReadState;
use crate::stream_error::to_rdy;
use rdy_model::decision::SUPERSEDES;
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPin, ForeignPublication, ForeignVersion, selection_identity,
};
use rdy_model::{BlockId, Hash};
use rdy_store::closure::StoreSource;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{
    DeliveryRecord, DeliveryReport, MAX_DELIVERY_VERSIONS, StreamHeader, StreamRecord, StreamWriter,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

/// The most rows [`tag_uses`] returns.
pub const MAX_TAG_USES: usize = 100_000;

fn internal(what: &str) -> RdyError {
    RdyError::new(E_INTERNAL, what, "report this as a bug in rdy")
}

/// Writes the link delivery of `profile` (a profile id, a key, or `implicit:org`) at the tag
/// `tag_name`, whose `TAG` object has the hash `tag_hash` and whose landing is `landing`, as one
/// complete stream to `out`. `tag_hash` is provenance in the header only. The tag must be one
/// `read` captured. Returns what the stream holds.
///
/// A public profile whose reference check is blocked is refused, as an output of it is
/// (`E_PUBLICATION_REFERENCE`). The redaction epoch is checked before and after: a removal
/// applied while the stream is written makes the call fail with `E_REDACTED_STATE`, and the
/// caller discards what it wrote.
pub fn write(
    read: &ReadState,
    tag_name: &str,
    tag_hash: Hash,
    landing: Hash,
    profile: &str,
    out: impl Write,
) -> Result<DeliveryReport, RdyError> {
    read.ensure_current_epoch()?;
    let scope = Scope::At(landing);
    let resolved = resolve(read, &scope, profile)?;
    resolved.require_output()?;
    let facts = profile_facts(&resolved);
    let reader = read.reader();
    let Some(repo_id) = reader.repo_id()? else {
        return Err(internal("the store has no repo id"));
    };
    let tracking = reader.tracking();
    let mut source = StoreSource {
        reader,
        end: read.seq(),
    };

    // the delivered selection: every retained block but a shell, in id order
    let mut entries: Vec<_> = resolved
        .entries
        .iter()
        .filter(|e| !resolved.shells.contains(&e.id))
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    if entries.len() > MAX_DELIVERY_VERSIONS {
        return Err(RdyError::new(
            E_LIMIT,
            format!("a delivery holds at most {MAX_DELIVERY_VERSIONS} blocks"),
            "narrow the profile's selection",
        ));
    }
    let selected: BTreeSet<&BlockId> = entries.iter().map(|e| &e.id).collect();

    // the selected blocks that supersede each selected block
    let mut superseded_by: BTreeMap<&BlockId, Vec<BlockId>> = BTreeMap::new();
    for e in &entries {
        let Some(b) = resolved.m.get(&e.id) else {
            return Err(internal("a selected block is missing from its selection"));
        };
        if e.redaction.is_some() {
            continue;
        }
        for l in b.links.get(SUPERSEDES).into_iter().flatten() {
            if let Some(t) = selected.get(&l.target)
                && **t != e.id
            {
                let by = superseded_by.entry(*t).or_default();
                if !by.contains(&e.id) {
                    by.push(e.id.clone());
                }
            }
        }
    }

    // one version per selected block; each block is read once and dropped
    let mut versions = Vec::with_capacity(entries.len());
    for e in &entries {
        let Some(b) = resolved.m.get(&e.id) else {
            return Err(internal("a selected block is missing from its selection"));
        };
        let version = if e.redaction.is_some() {
            let key = ObjectKey {
                kind: kinds::BLOCK,
                hash: e.version,
            };
            let encoding = match ObjectSource::record(&mut source, key).map_err(to_rdy)? {
                Some(StreamRecord::Present { encoding, .. })
                | Some(StreamRecord::Dropped { encoding, .. }) => encoding,
                None => {
                    return Err(to_rdy(rdy_store::stream_error::StreamError::unresolved(
                        0, 0, key,
                    )));
                }
            };
            ForeignVersion {
                id: e.id.clone(),
                version: e.version,
                content: b.content_hash_with(tracking),
                encoding,
                derived: ForeignDerived::default(),
            }
        } else {
            let stored = reader.get_block(&e.version)?;
            ForeignVersion {
                id: e.id.clone(),
                version: e.version,
                content: stored.content_hash_with(tracking),
                encoding: stored.encoding(),
                derived: ForeignDerived {
                    status: stored.field_str("status").map(str::to_string),
                    lifecycle: stored.field_str("lifecycle").map(str::to_string),
                    superseded_by: superseded_by.remove(&e.id).unwrap_or_default(),
                },
            }
        };
        versions.push(version);
    }
    let Some(selection) = selection_identity(versions.iter().map(|v| (&v.id, &v.version))) else {
        return Err(internal("a delivery selection does not fit its identity"));
    };
    let delivery = DeliveryRecord {
        publication: ForeignPublication {
            tag: tag_name.to_string(),
            profile: facts.selector,
            profile_version: facts.version,
        },
        selection,
        versions,
    };
    let header = StreamHeader::link_delivery(reader.store_features()?, repo_id, tag_hash);
    let mut w = StreamWriter::new_delivery(out, &header, &delivery).map_err(to_rdy)?;
    let mut keys = Vec::with_capacity(delivery.versions.len());
    for key in delivery.keys() {
        let r = ObjectSource::record(&mut source, key)
            .map_err(to_rdy)?
            .ok_or_else(|| to_rdy(rdy_store::stream_error::StreamError::unresolved(0, 0, key)))?;
        w.record(&r).map_err(to_rdy)?;
        keys.push(key);
    }
    let (_, totals) = w.finish().map_err(to_rdy)?;
    read.ensure_current_epoch()?;
    Ok(DeliveryReport {
        header,
        delivery,
        keys,
        records: totals.records,
        bytes: totals.bytes,
        digest: totals.digest,
    })
}

/// One foreign pin a tag holds: the local block that keeps it, the cited block, the pin, the
/// version this state holds for the pin (none when no delivery holds it) and, while a source
/// still selects that version, that source's ref name and the entry that delivered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagUse {
    pub citing: BlockId,
    pub target: ForeignId,
    pub pin: ForeignPin,
    pub held: Option<Hash>,
    pub source: Option<String>,
    pub entry: Option<Hash>,
}

/// The foreign pins of the blocks the tag with landing `landing` holds, sorted by citing block
/// and cited block. A tag of a project with no foreign state has none. More than
/// [`MAX_TAG_USES`] pins is `E_LIMIT`.
pub fn tag_uses(read: &ReadState, landing: Hash) -> Result<Vec<TagUse>, RdyError> {
    read.ensure_current_epoch()?;
    let m = read.materialized(&Scope::At(landing))?;
    let foreign = read.foreign()?;
    let mut out: Vec<TagUse> = Vec::new();
    for b in m.iter().filter(|b| !b.foreign.pins.is_empty()) {
        for (target, pin) in &b.foreign.pins {
            if out.len() >= MAX_TAG_USES {
                return Err(RdyError::new(
                    E_LIMIT,
                    format!("a tag lists at most {MAX_TAG_USES} foreign pins"),
                    "read the pins of fewer blocks",
                ));
            }
            let held = read.foreign_held_pin(target, pin)?;
            let carrier = held.and_then(|version| {
                foreign.sources().find_map(|(name, view)| {
                    let m = view.members.get(&target.id)?;
                    (view.repo == target.repo && m.version.version == version)
                        .then(|| (name.to_string(), m.entry))
                })
            });
            out.push(TagUse {
                citing: b.id.clone(),
                target: target.clone(),
                pin: pin.clone(),
                held,
                source: carrier.as_ref().map(|(n, _)| n.clone()),
                entry: carrier.map(|(_, e)| e),
            });
        }
    }
    out.sort_by(|a, b| (&a.citing, &a.target).cmp(&(&b.citing, &b.target)));
    Ok(out)
}
