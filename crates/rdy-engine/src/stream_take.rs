//! The take of one upload into a holder (D-D2NPN "verify-on-receive", D-BPYN8, D-CC90E; hub
//! p-97JVE, f-NE0Z4, r-DR5Z9, r-M1GA9). [`take`] hands every batch file of the upload to the
//! store's one take (`Store::take_batches`), which checks the union of the files over the holder
//! and writes the closure, its approved removals and the tag in one sealed batch. The engine adds
//! the checks that need its rules, in the store's validation callback, under the writer:
//!
//! 1. The root tag's name is a version by the existing parser (`parse_version`: one optional
//!    leading `v`, no `+build` metadata). A held tag of the same version (SemVer precedence, so
//!    `v1.2.0` equals `1.2.0`) at the same landing survives with its original name, hash and
//!    metadata (`TagChoice::Existing`); at another landing it is `E_TAG_EXISTS`, before anything
//!    is appended. The imported version need not exceed the held ones. Two held tags of that
//!    version are damage: the take returns fsck's tag report instead of picking one.
//! 2. Every block and blob manifest the holder lacks keeps the local limits the batch check
//!    keeps for one record (`stream_check::check_batch`), so a take keeps no record that a batch
//!    check refuses. Each such blob keeps the chunk lengths a blob read keeps, over the union
//!    and the holder; a blob with a removed chunk, or a removed manifest, has none to judge.
//! 3. Every landing of the upload that the holder does not hold keeps the landing rules the
//!    holder's fsck keeps (`rdy_store::landing_field_rules`: relay, evidence and the objects it
//!    names), and the root tag's chain keeps the chain rules (`check_holder::chain_rules`: each
//!    change lands once, after its dependencies). These are structural rules, not the current
//!    gate policy.
//! 4. Every signature of the upload that the holder does not hold passes SIG's check
//!    (`signature_check::check_signature`) against its subject in the union and the holder, a
//!    relayed tag's relay answer as the take names it: the check the holder's fsck runs once the
//!    take is sealed. An unknown key is no refusal, nor is a signature of another project
//!    (`foreign_repo`); an invalid one is E_SIG_INVALID, an uncovered one E_SIG_OBJECT_MISMATCH.
//! 5. Every landing of the upload that the holder does not hold has its snapshot read from the
//!    union and the holder: each entry must name a version of its own block, and the whole
//!    snapshot must keep the Publications resource limits (`check_snapshot_limits`). A removed
//!    block reads as its stub. No block is checked against a pack, and no approval is judged by
//!    the current gate: a block of a type no pack knows is taken when these checks pass.
//!
//! Steps 2 to 5 are `render_source::check_snapshots`, which a stream render runs too, with
//! nothing held.
//!
//! A record that breaks a structural rule is `E_BAD_RECORD` with the holder fsck's own text (a
//! signature its fsck code), so no take acknowledges an upload that the holder's fsck would then
//! refuse. An ANSWER's record rules are the batch check's (`sync_stream::check_batch`), which the
//! store's take runs on every file first.
//!
//! The closure, the prerequisites, the removal proofs and the upload target are the store's
//! checks (`rdy_store::take_batch`), not repeated here. After a take that wrote something, the
//! holder's read state is published again from the store (a full publish; a holder logs no
//! moves, so no other batch is written). A take that sealed a removal but could not finish its
//! erasure returns `E_REDACTION_PENDING` and the reopen finishes it, as a native removal does.
//! A caller whose reply was lost repairs from the holder's tags and object-key presence: the
//! take keeps no receipt, transaction id or timer.
use crate::error::{E_BAD_INPUT, E_REDACTION_STALE, E_REPO_MISMATCH, E_TAG_EXISTS, RdyError};
use crate::render_source::check_snapshots;
use crate::stream_error::to_rdy;
use crate::version::parse_version;
use crate::{Delta, Rdy};
use rdy_model::{Hash, Tag};
use rdy_schema::{Issue, Severity};
use rdy_store::sync_proof::ObjectSource;
use rdy_store::take_batch::{
    BatchTaken, TagChoice, TakeError, UploadFacts, UploadTarget, check_input_bounds,
};
use rdy_store::{StoreError, StoreReader};
use serde::{Serialize, Serializer};
use std::cmp::Ordering;
use std::fs::File;

/// Where the holder's log ends after a take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StorePosition {
    pub segment: u32,
    pub offset: u64,
}

/// What one take did. The counts are the store's: the objects it wrote that the holder lacked,
/// its data frames (the marker not counted) and the objects it removed. `tag` and `tag_hash`
/// name the tag that survives (the held one when the same version already named the landing),
/// `landing` its landing. A holder has no canon: `canon` is always None. `unchanged` is true
/// when the take appended no frame and no marker (a complete retry).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TakeOutcome {
    pub records_taken: u64,
    pub frames_written: u64,
    pub redacted_objects: u64,
    pub tag: String,
    #[serde(serialize_with = "hex")]
    pub tag_hash: Hash,
    #[serde(serialize_with = "hex")]
    pub landing: Hash,
    #[serde(serialize_with = "hex_opt")]
    pub canon: Option<Hash>,
    pub end: StorePosition,
    pub unchanged: bool,
}

fn hex<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

fn hex_opt<S: Serializer>(h: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
    match h {
        Some(h) => s.collect_str(h),
        None => s.serialize_none(),
    }
}

/// A tag the holder holds, by its ref: its parsed version, its record and its hash.
struct Held {
    version: semver::Version,
    hash: Hash,
    tag: Tag,
}

/// Takes the batch files `batches` of one upload for the publication `target` into the holder
/// `rdy` (see the module docs). The files may come in any order and repeat each other; together
/// they must hold the root tag's whole closure less what the holder holds. Every refusal before
/// the append leaves the holder unchanged.
pub fn take(
    rdy: &mut Rdy,
    batches: &[File],
    target: UploadTarget,
) -> Result<TakeOutcome, RdyError> {
    check_input_bounds(batches.len(), &target).map_err(to_rdy)?;
    // a project is refused before anything of it is sealed (the store refuses it too)
    if !rdy.is_holder() {
        return Err(take_error(TakeError::ModeMismatch));
    }
    // a take is a batch of its own: one a raw write left open is sealed and published first
    rdy.seal_open()?;
    // the writer is held for the whole call, so the tags read here are the ones the take sees
    let held = held_tags(&rdy.store)?;
    let damage = if same_version_pair(&held) {
        rdy.check_holder_tags().err()
    } else {
        None
    };
    let epoch = rdy.store.redaction_epoch();
    let reader = rdy.store.reader();
    let (held, damage) = (&held, &damage);
    // the writer's held batches are off for the take, which seals its own batch, as for the
    // store's other self-sealing writes
    rdy.store.set_held(false)?;
    let taken = rdy
        .store
        .take_batches(batches, &target, move |source, facts| {
            validate(&reader, source, facts, held, damage.as_ref())
        });
    rdy.store.set_held(true)?;
    match taken {
        Ok(t) => {
            if t.frames > 0 {
                republish(rdy, epoch)?;
            }
            Ok(outcome(t))
        }
        Err(e) => {
            if matches!(e, TakeError::Store(StoreError::RedactionPending)) {
                // the removal is sealed and its erasure unfinished: no state built before it is
                // served again (the reopen finishes the erasure); the take's error is returned
                let _ = republish(rdy, epoch);
            }
            Err(take_error(e))
        }
    }
}

/// Publishes the holder's state again from its store after a take wrote a batch. A take that
/// advanced the redaction epoch (or sealed a removal it could not finish) starts the shared
/// caches over, as a native removal does, so no read of the new state reuses an old build.
fn republish(rdy: &mut Rdy, epoch: u64) -> Result<(), RdyError> {
    let pending = rdy.store.reader().check_redaction_epoch().is_err();
    if pending || rdy.store.redaction_epoch() != epoch {
        rdy.renew_epoch_caches();
        rdy.logged = None;
    }
    rdy.publish(Delta::Full)
}

fn outcome(t: BatchTaken) -> TakeOutcome {
    TakeOutcome {
        records_taken: t.new_objects,
        frames_written: t.frames,
        redacted_objects: t.redacted_objects,
        tag: t.tag.name.clone(),
        tag_hash: t.tag_hash,
        landing: t.tag.landing,
        canon: None,
        end: StorePosition {
            segment: t.end.segment,
            offset: t.end.offset,
        },
        unchanged: t.frames == 0,
    }
}

/// A take's error as the engine's: the stream conversion for a stream refusal, the existing
/// mapping for a store error, the validation's own error, and the take's three structured
/// refusals.
fn take_error(e: TakeError<RdyError>) -> RdyError {
    match e {
        TakeError::Store(e) => e.into(),
        TakeError::Stream(e) => to_rdy(e),
        TakeError::Validation(e) => e,
        TakeError::UncoveredChange(h) => RdyError::new(
            E_REDACTION_STALE,
            format!("the imported removal does not cover receiver change {h}"),
            format!(
                "redact this content in the project that made change {h}, upload that removal, then upload again"
            ),
        ),
        TakeError::ModeMismatch => RdyError::new(
            E_BAD_INPUT,
            "this store is a project",
            "open it through the project adapter",
        ),
        TakeError::RepoMismatch { source, target } => RdyError::new(
            E_REPO_MISMATCH,
            format!("stream repo {source} does not match holder repo {target}"),
            "use the holder for the stream's repo",
        ),
    }
}

/// Every `tag/` ref whose record names a version, read as the holder's fsck reads them. A ref
/// that does not read as a version tag is fsck's to report; it names no version here.
fn held_tags(reader: &StoreReader) -> Result<Vec<Held>, RdyError> {
    let mut out = Vec::new();
    for (_, value) in reader.refs_with_prefix(rdy_store::holder::TAG_REF)? {
        let Ok(bytes) = <[u8; 32]>::try_from(value.as_slice()) else {
            continue;
        };
        let hash = Hash(bytes);
        let Ok(tag) = reader.read_tag(&hash) else {
            continue;
        };
        if let Ok(version) = parse_version(&tag.name) {
            out.push(Held { version, hash, tag });
        }
    }
    Ok(out)
}

/// Whether two held tags denote the same version.
fn same_version_pair(held: &[Held]) -> bool {
    let mut versions: Vec<&semver::Version> = held.iter().map(|h| &h.version).collect();
    versions.sort_by(|a, b| a.cmp_precedence(b));
    versions
        .windows(2)
        .any(|w| w[0].cmp_precedence(w[1]) == Ordering::Equal)
}

/// The store take's validation callback: the tag rule, then the structural and snapshot checks
/// of every new record (`render_source::check_snapshots`, which a render runs with nothing
/// held).
fn validate(
    reader: &StoreReader,
    source: &mut dyn ObjectSource,
    facts: &UploadFacts,
    held: &[Held],
    damage: Option<&RdyError>,
) -> Result<TagChoice, RdyError> {
    let choice = choose_tag(&facts.root, held, damage)?;
    // steps 2 to 5: the structural and snapshot checks a render runs too
    check_snapshots(reader, source, facts)?;
    Ok(choice)
}

/// The tag rule (module docs, step 1).
fn choose_tag(root: &Tag, held: &[Held], damage: Option<&RdyError>) -> Result<TagChoice, RdyError> {
    let version = parse_version(&root.name)?;
    let same: Vec<&Held> = held
        .iter()
        .filter(|h| h.version.cmp_precedence(&version) == Ordering::Equal)
        .collect();
    match same[..] {
        [] => Ok(TagChoice::Incoming),
        [h] if h.tag.landing == root.landing => Ok(TagChoice::Existing {
            name: h.tag.name.clone(),
            hash: h.hash,
        }),
        [h] => Err(tag_exists(&root.name, &h.tag.landing)),
        // two held tags of one version: fsck's report, never a choice by iteration order
        _ => Err(damage.cloned().unwrap_or_else(|| {
            RdyError::new(
                crate::E_IO,
                format!("two tags denote version {}", root.name),
                "a tag ref is damaged or was written outside rdy tag: restore the store from a backup",
            )
        })),
    }
}

/// E_TAG_EXISTS: the incoming version `name` already names landing `existing` in the holder.
fn tag_exists(name: &str, existing: &Hash) -> RdyError {
    const FIX: &str = "use another version name";
    let mut e = RdyError::new(
        E_TAG_EXISTS,
        format!("version {name} already names landing {existing}"),
        FIX,
    );
    e.issues.push(Issue {
        severity: Severity::Error,
        code: E_TAG_EXISTS,
        block: None,
        message: format!("existing_landing: {existing}"),
        fix: FIX.into(),
    });
    e
}
