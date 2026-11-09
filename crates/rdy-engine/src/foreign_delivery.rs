//! The citing side of a repo link (D-DAPR8, D-FK58A, D-MEMMQ): the take of one link delivery
//! into a project store, and the revocation of a link's source.
//!
//! [`take_delivery`] runs under the store's one writer. Before anything is written it checks:
//!
//! 1. The bounds of the caller's input (`check_take_bounds`): the source key, its text fields,
//!    the set's team name and the stream's length. The adapter checks them before it clones the
//!    caller's handle.
//! 2. The store is a project (`this store is a holder` otherwise), and the source is not revoked
//!    (`the repo link is revoked`).
//! 3. The stream's header and `DELIVERY`: format, purpose (E_STREAM_PURPOSE names
//!    `link_delivery`), and a repo that is not this project's (`a project cannot cite itself`).
//! 4. The whole stream (`sync_stream::check_delivery_with`): framing, hashes, bounds, the
//!    trailer and the complete selection. Each present BLOCK the store does not hold is decoded
//!    within its bounds and kept for the batch. A dropped BLOCK the store does not hold has no
//!    payload to keep: it is E_UNRESOLVED_REF at the `DELIVERY`, as a missing object is.
//!
//! Then the delivery is compared with the source's view (D-MEMMQ §5). It is the same when the
//! profile, the set kind and team, and each selected block's version and derived facts
//! (status, lifecycle, superseded by) match: [`same_delivery_digest`] of both is equal. A same
//! delivery writes nothing and returns `unchanged: true`, so a new tag or profile version alone
//! writes nothing. Any other delivery is one entry: the ids that are new (`added`), the members
//! whose version or derived facts changed (`changed`, even with no new BLOCK) and the members
//! the selection left (`removed`).
//!
//! Right before the write the take checks the caller's file again: the same file identity and
//! length, and a second read of its framing to the same trailer counts and digest (`input changed
//! during take` otherwise). The store's own foreign checks (`Store::append_foreign`) then run
//! before the append. The new BLOCK versions, the FOREIGN entry and the source's ref (with the
//! `meta/store_features` ref on first use) start one held batch, and the publish seals it with
//! the moves it makes as its last frame. A refusal at any step writes nothing.
//!
//! [`revoke_link`] seals a Revoked entry after the source's newest entry. The source keeps its
//! last view, frozen, and refuses every later delivery. A second revocation writes nothing.
use crate::error::{E_BAD_INPUT, E_IO, E_LIMIT, E_NOT_FOUND, RdyError};
use crate::holder::is_a_holder;
use crate::stream_error::to_rdy;
use crate::stream_take::StorePosition;
use crate::{Delta, Rdy};
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignSet, ForeignSource, ForeignVersion,
};
use rdy_model::{Block, BlockId, Hash, RepoId};
use rdy_store::bounded::{MAX_TEXT_BYTES, TEXT_BYTES};
use rdy_store::fileid::FileId;
use rdy_store::foreign::{SourceView, check_source_key, source_ref};
use rdy_store::kinds;
use rdy_store::stream_error::{LIMIT_TOTAL_BYTES, Reason, StreamError};
use rdy_store::sync_stream::{
    DeliveryReport, MAX_STREAM_BYTES, StreamPurpose, StreamReader, StreamRecord,
    check_delivery_with,
};
use serde::{Serialize, Serializer};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};

/// The domain prefix of [`same_delivery_digest`], the NUL included.
pub const SAME_DELIVERY_DOMAIN: &[u8] = b"rdy-foreign-delivery-same-1\0";

/// What one delivery take did. `entry` is the source's newest entry after the call: the new
/// entry, or the head that a same delivery left as it was. `added`, `changed` and `removed`
/// count the entry's delta, `blocks_taken` the BLOCK versions the store did not hold before.
/// `selection` is the delivery's publication selection identity. `unchanged` is true when the
/// take wrote nothing (D-MEMMQ §5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeliveryTaken {
    #[serde(serialize_with = "hex")]
    pub entry: Hash,
    #[serde(serialize_with = "hex_repo")]
    pub repo: RepoId,
    pub added: u64,
    pub changed: u64,
    pub removed: u64,
    pub blocks_taken: u64,
    #[serde(serialize_with = "hex")]
    pub selection: Hash,
    pub end: StorePosition,
    pub unchanged: bool,
}

/// What one revocation did: the Revoked entry (or the one a source already ended with), where
/// the log ends, and whether nothing was written (the source was revoked already).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkRevoked {
    #[serde(serialize_with = "hex")]
    pub entry: Hash,
    pub end: StorePosition,
    pub unchanged: bool,
}

fn hex<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

fn hex_repo<S: Serializer>(r: &RepoId, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(r)
}

/// E_BAD_INPUT: the stream's repo is this project's own.
fn cites_itself() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "a project cannot cite itself",
        "deliver content from another project",
    )
}

/// E_BAD_INPUT: the source is revoked.
fn revoked() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "the repo link is revoked",
        "use a live repo link",
    )
}

/// E_NOT_FOUND: a revocation of a source with no delivery.
fn no_delivery(name: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("source {name} has no delivery to revoke"),
        "revoke a repo link after its first delivery",
    )
}

/// E_IO: the caller's stream file cannot be read.
fn io(e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("stream input: {e}"),
        "check that the stream file can be read",
    )
}

/// E_BAD_RECORD: the caller's file changed after the take checked it.
fn changed_input() -> RdyError {
    to_rdy(StreamError::bad_record(
        0,
        0,
        Reason::InputChangedDuringTake,
    ))
}

/// E_LIMIT: a text field of the caller's input is over the bounded decoder's text bound.
fn text_limit(field: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("{field} exceeds {MAX_TEXT_BYTES} {TEXT_BYTES}"),
        "reduce the text to the stated limit",
    )
}

/// The bounds of a source: its key (`rdy_store::foreign::check_source_key`) and its text fields
/// within the bounded decoder's text bound.
fn check_source_bounds(source: &ForeignSource) -> Result<(), RdyError> {
    check_source_key(source)?;
    match source {
        ForeignSource::Link { site, .. } if site.len() > MAX_TEXT_BYTES => Err(text_limit("site")),
        ForeignSource::Adr { revision, .. } if revision.len() > MAX_TEXT_BYTES => {
            Err(text_limit("revision"))
        }
        _ => Ok(()),
    }
}

/// The bounds of a take's input, checked before the caller's handle is cloned or any of the
/// stream is read: the source's key and text fields, the set's team name, and a stream of at
/// most [`MAX_STREAM_BYTES`] (`stream_len`).
pub fn check_take_bounds(
    source: &ForeignSource,
    set: Option<&ForeignSet>,
    stream_len: u64,
) -> Result<(), RdyError> {
    check_source_bounds(source)?;
    if let Some(ForeignSet::Team(team)) = set
        && team.len() > MAX_TEXT_BYTES
    {
        return Err(text_limit("team"));
    }
    if stream_len > MAX_STREAM_BYTES {
        return Err(to_rdy(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES)));
    }
    Ok(())
}

/// The digest that makes two deliveries of one source the same (D-MEMMQ §5): their profile, the
/// set kind and team, and each selected block's version and derived facts. Neither the tag nor
/// the profile version enters it. `versions` are the complete selection in id order.
///
/// The bytes, in order: [`SAME_DELIVERY_DOMAIN`]; the profile (u64 LE length, UTF-8 bytes);
/// the set (byte 0 none, 1 organization, 2 team then the team's u64 LE length and bytes); the
/// u64 LE version count; then per version its id (u64 LE length, bytes), byte 0 and its 32 raw
/// version bytes, its status and its lifecycle (byte 0 absent, else byte 1, u64 LE length and
/// bytes), and its superseded-by ids (u64 LE count, then each id's u64 LE length and bytes).
/// Byte 0 before the version marks a delivered BLOCK version; a later form (a version held as
/// its redaction residue) takes another byte and leaves these bytes as they are.
pub fn same_delivery_digest<'a, I>(profile: &str, set: Option<&ForeignSet>, versions: I) -> Hash
where
    I: IntoIterator<Item = &'a ForeignVersion>,
    I::IntoIter: ExactSizeIterator,
{
    fn text(h: &mut blake3::Hasher, t: &str) {
        h.update(&(t.len() as u64).to_le_bytes());
        h.update(t.as_bytes());
    }
    fn opt(h: &mut blake3::Hasher, t: &Option<String>) {
        match t {
            None => {
                h.update(&[0]);
            }
            Some(t) => {
                h.update(&[1]);
                text(h, t);
            }
        }
    }
    let versions = versions.into_iter();
    let mut h = blake3::Hasher::new();
    h.update(SAME_DELIVERY_DOMAIN);
    text(&mut h, profile);
    match set {
        None => {
            h.update(&[0]);
        }
        Some(ForeignSet::Organization) => {
            h.update(&[1]);
        }
        Some(ForeignSet::Team(team)) => {
            h.update(&[2]);
            text(&mut h, team);
        }
    }
    h.update(&(versions.len() as u64).to_le_bytes());
    for v in versions {
        text(&mut h, v.id.as_str());
        h.update(&[0]);
        h.update(&v.version.0);
        let ForeignDerived {
            status,
            lifecycle,
            superseded_by,
        } = &v.derived;
        opt(&mut h, status);
        opt(&mut h, lifecycle);
        h.update(&(superseded_by.len() as u64).to_le_bytes());
        for id in superseded_by {
            text(&mut h, id.as_str());
        }
    }
    Hash(*h.finalize().as_bytes())
}

/// The same-delivery digest of a source's current view.
fn view_digest(view: &SourceView) -> Hash {
    same_delivery_digest(
        &view.publication.profile,
        view.set.as_ref(),
        view.members.values().map(|m| &m.version),
    )
}

/// A delivery's delta against the source's view: the new ids, the members whose version or
/// derived facts changed, and the members the selection left. `versions` are in id order.
fn delta(
    view: Option<&SourceView>,
    versions: &[ForeignVersion],
) -> (Vec<ForeignVersion>, Vec<ForeignVersion>, Vec<BlockId>) {
    let (mut added, mut changed, mut removed) = (vec![], vec![], vec![]);
    for v in versions {
        match view.and_then(|w| w.members.get(&v.id)) {
            None => added.push(v.clone()),
            Some(m) if m.version.version != v.version || m.version.derived != v.derived => {
                changed.push(v.clone())
            }
            Some(_) => {}
        }
    }
    if let Some(w) = view {
        for id in w.members.keys() {
            if versions.binary_search_by(|v| v.id.cmp(id)).is_err() {
                removed.push(id.clone());
            }
        }
    }
    (added, changed, removed)
}

/// The log's end, as a take reports it.
fn end_of(rdy: &Rdy) -> StorePosition {
    let end = rdy.store.log_end();
    StorePosition {
        segment: end.segment,
        offset: end.offset,
    }
}

/// A check's failure: the stream's, or the engine's own.
enum Fail {
    Stream(StreamError),
    Core(RdyError),
}

impl From<StreamError> for Fail {
    fn from(e: StreamError) -> Fail {
        Fail::Stream(e)
    }
}

impl From<Fail> for RdyError {
    fn from(f: Fail) -> RdyError {
        match f {
            Fail::Stream(e) => to_rdy(e),
            Fail::Core(e) => e,
        }
    }
}

/// A reader of the caller's file from its first byte.
fn from_start(stream: &File) -> Result<BufReader<&File>, std::io::Error> {
    let mut f = stream;
    f.seek(SeekFrom::Start(0))?;
    Ok(BufReader::new(f))
}

/// Takes one link delivery from `stream` for `source` and the set `set` into the project `rdy`
/// (see the module docs). The file stays the caller's; it must not change during the call.
pub fn take_delivery(
    rdy: &mut Rdy,
    stream: &File,
    source: &ForeignSource,
    set: Option<ForeignSet>,
) -> Result<DeliveryTaken, RdyError> {
    let len = stream.metadata().map_err(io)?.len();
    check_take_bounds(source, set.as_ref(), len)?;
    if rdy.is_holder() {
        return Err(is_a_holder());
    }
    // a take is a batch of its own: one a raw write left open is sealed and published first
    rdy.seal_open()?;
    let reader = rdy.store.reader();
    let name = source_ref(source);
    let view = reader.source_view(&name)?;
    if view.as_ref().is_some_and(|v| v.revoked) {
        return Err(revoked());
    }
    let own = reader.repo_id()?;
    let file_id = FileId::of_file(stream).map_err(io)?;
    // the header and the DELIVERY first: the purpose, the format and the repo, before any
    // object is read
    let (header, delivery_at) = {
        let (r, header) =
            StreamReader::open_for(from_start(stream).map_err(io)?, StreamPurpose::LinkDelivery)
                .map_err(to_rdy)?;
        (header, r.position().1)
    };
    if own == Some(header.repo_id) {
        return Err(cites_itself());
    }
    // the whole stream, keeping each present BLOCK the store lacks (with its version)
    let mut blocks: Vec<(Hash, Block)> = vec![];
    let report: DeliveryReport = check_delivery_with(
        from_start(stream).map_err(io)?,
        |n, offset, r: &StreamRecord| -> Result<(), Fail> {
            let key = r.key();
            if reader
                .has_kind(kinds::BLOCK, &key.hash)
                .map_err(|e| Fail::Core(e.into()))?
            {
                return Ok(());
            }
            match r.typed::<Block>() {
                Some(b) => {
                    let b = b.map_err(|e| {
                        StreamError::from_record(n, offset, &e).with_object(Some(key))
                    })?;
                    blocks.push((key.hash, b));
                    Ok(())
                }
                // a dropped version this store does not hold has no payload to hold
                None => Err(StreamError::unresolved(1, delivery_at, key)
                    .with_object(Some(key))
                    .into()),
            }
        },
    )?;
    if report.header != header {
        return Err(changed_input());
    }
    let d = &report.delivery;
    let incoming = same_delivery_digest(&d.publication.profile, set.as_ref(), &d.versions);
    if let Some(v) = &view
        && view_digest(v) == incoming
    {
        return Ok(DeliveryTaken {
            entry: v.head,
            repo: v.repo,
            added: 0,
            changed: 0,
            removed: 0,
            blocks_taken: 0,
            selection: v.selection,
            end: end_of(rdy),
            unchanged: true,
        });
    }
    let (added, changed, removed) = delta(view.as_ref(), &d.versions);
    let needed: BTreeSet<Hash> = added.iter().chain(&changed).map(|v| v.version).collect();
    let blocks: Vec<Block> = blocks
        .into_iter()
        .filter(|(h, _)| needed.contains(h))
        .map(|(_, b)| b)
        .collect();
    let counts = (
        added.len() as u64,
        changed.len() as u64,
        removed.len() as u64,
        blocks.len() as u64,
    );
    let entry = ForeignEntry {
        repo: header.repo_id,
        source: source.clone(),
        event: ForeignEvent::Delivery {
            prev: view.as_ref().map(|v| v.head),
            publication: d.publication.clone(),
            set,
            added,
            changed,
            removed,
            // the delivery's shells travel with it: the store rebuilds `selection` from the
            // delivered versions plus these (D-2CG1N)
            shells: d.shells.clone(),
            selection: d.selection,
        },
    };
    // the caller's file, once more, right before the write
    recheck(stream, file_id, len, &report)?;
    let id = rdy.store.append_foreign(&entry, &blocks)?;
    // the publish seals the batch with the moves it makes, and serves the new foreign cut
    rdy.publish(Delta::Seq)?;
    Ok(DeliveryTaken {
        entry: id,
        repo: header.repo_id,
        added: counts.0,
        changed: counts.1,
        removed: counts.2,
        blocks_taken: counts.3,
        selection: d.selection,
        end: end_of(rdy),
        unchanged: false,
    })
}

/// Checks that the caller's file is still the one the take checked: the same identity and
/// length, and its framing read again to the same trailer counts and digest.
fn recheck(stream: &File, id: FileId, len: u64, report: &DeliveryReport) -> Result<(), RdyError> {
    let same_file = FileId::of_file(stream).is_ok_and(|now| now == id)
        && stream.metadata().is_ok_and(|m| m.len() == len);
    if !same_file {
        return Err(changed_input());
    }
    let input = from_start(stream).map_err(|_| changed_input())?;
    let totals = StreamReader::open_for(input, StreamPurpose::LinkDelivery)
        .and_then(|(r, _)| r.finish())
        .map_err(|_| changed_input())?;
    if (totals.records, totals.bytes, totals.digest)
        != (report.records, report.bytes, report.digest)
    {
        return Err(changed_input());
    }
    Ok(())
}

/// Revokes `source` in the project `rdy` (see the module docs): a Revoked entry after its newest
/// entry, in one batch the publish seals. A revoked source takes no more entries and keeps its
/// last view. A source revoked already writes nothing; one with no delivery is E_NOT_FOUND.
pub fn revoke_link(rdy: &mut Rdy, source: &ForeignSource) -> Result<LinkRevoked, RdyError> {
    check_source_bounds(source)?;
    if rdy.is_holder() {
        return Err(is_a_holder());
    }
    rdy.seal_open()?;
    let name = source_ref(source);
    let Some(view) = rdy.store.reader().source_view(&name)? else {
        return Err(no_delivery(&name));
    };
    if view.revoked {
        return Ok(LinkRevoked {
            entry: view.head,
            end: end_of(rdy),
            unchanged: true,
        });
    }
    let entry = ForeignEntry {
        repo: view.repo,
        source: source.clone(),
        event: ForeignEvent::Revoked { prev: view.head },
    };
    let id = rdy.store.append_foreign(&entry, &[])?;
    rdy.publish(Delta::Seq)?;
    Ok(LinkRevoked {
        entry: id,
        end: end_of(rdy),
        unchanged: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::foreign::ForeignDerived;

    fn v(id: &str, version: u8, status: &str) -> ForeignVersion {
        ForeignVersion {
            id: BlockId::parse(id).unwrap(),
            version: Hash([version; 32]),
            content: Hash([version.wrapping_add(1); 32]),
            encoding: 0,
            derived: ForeignDerived {
                status: Some(status.into()),
                lifecycle: None,
                superseded_by: vec![],
            },
        }
    }

    /// The digest covers the profile, the set, the versions and the derived facts, and not the
    /// content hash or its encoding.
    #[test]
    fn rpl10_same_delivery_digest_covers_the_recipe() {
        let a = [v("d-00001", 1, "accepted")];
        let base = same_delivery_digest("pp-00001", None, &a);
        assert_eq!(base, same_delivery_digest("pp-00001", None, &a));
        assert_ne!(base, same_delivery_digest("pp-00002", None, &a));
        let org = same_delivery_digest("pp-00001", Some(&ForeignSet::Organization), &a);
        let team = same_delivery_digest("pp-00001", Some(&ForeignSet::Team("t".into())), &a);
        assert_ne!(base, org);
        assert_ne!(org, team);
        assert_ne!(
            base,
            same_delivery_digest("pp-00001", None, &[v("d-00001", 2, "accepted")])
        );
        assert_ne!(
            base,
            same_delivery_digest("pp-00001", None, &[v("d-00001", 1, "superseded")])
        );
        let mut other_content = v("d-00001", 1, "accepted");
        other_content.content = Hash([9; 32]);
        other_content.encoding = 1;
        assert_eq!(
            base,
            same_delivery_digest("pp-00001", None, &[other_content])
        );
    }
}
