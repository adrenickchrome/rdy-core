//! The publication take (D-91DZW; D-D2NPN "verify-on-receive", D-BPYN8, D-KDFZ3, D-Z8Q4N,
//! D-075QD, D-WDKWQ): one take of an upload into a holder that also resolves the publication
//! profile the host asked for and hands the host its verdict "after core's checks and before
//! anything is appended". A new tag's profile can exist only in the upload, so the profile is
//! read from the checked upload over the holder, inside the take, under the holder's one writer.
//!
//! # The order, under one writer
//!
//! 1. The store's take checks each batch file, their union over the holder, the repo and the
//!    root, the closure and the removal proofs (`rdy_store::take_batch`).
//! 2. The engine applies the tag rule, the record limits, the landing structure, the signatures
//!    and the snapshot limits, in the take's own order (`stream_take`).
//! 3. The chosen root landing is materialized through that checked source, approved removals
//!    applied (`render_source::materialize_landing`), and the requested profile is resolved in
//!    it (`publication::resolve_source`): [`PublicationFacts`].
//! 4. The selector, profile and public-reference refusals are core errors, before any verdict:
//!    a selector that is not a full profile id or `implicit:org`, a block that is not a
//!    profile, an invalid or redacted profile, a Publications limit, and a public profile whose
//!    reference check is blocked (`E_PUBLICATION_REFERENCE`). A well-formed full id that no
//!    block of the tag has is no refusal: the facts say `profile: None`, and the host judges.
//! 5. The host's verdict runs once with those facts. A panic in it is caught here, so it never
//!    unwinds through the writer, and is `E_INTERNAL`.
//! 6. The store enforces the receiver caps, reads each record again, appends and seals; the new
//!    read state is published; the outcome and the same facts are returned.
//!
//! Every refusal before the append (a core check, the verdict, a cap, changed input) appends
//! nothing and changes no tag, ref or removal epoch, and leaves the holder ready for its next
//! call. A complete retry resolves its facts and calls the verdict again, then appends nothing.
//!
//! The facts are the root tag's: its snapshot, never a later profile revision or the holder's
//! state. The tag is the one that survives: the held tag's name and hash when the holder already
//! names the landing by the same version. The title and description of the implicit profile are
//! the holder's captured settings, as `Snapshot::profiles` reads them after the take.
//!
//! The verdict runs on the writer's thread and must read only what the host loaded before the
//! call: it must not call the adapter on the same store, which would wait for its own writer.
//! The hub owns its policy checks and their order, site, profile and audience matching, public
//! site settings and publication records (D-WDKWQ). This library callback is no person-facing
//! policy command.
use crate::Rdy;
use crate::error::{E_INTERNAL, RdyError};
use crate::publication::{
    IMPLICIT_ORG, ProfileFacts, check_stable_selector, profile_facts, resolve_source,
};
use crate::render_source::{ObjectPublicationSource, SourceSettings, materialize_landing};
use crate::stream_error::to_rdy;
use crate::stream_take::{TakeOutcome, take_checked};
use rdy_model::{BlockId, Hash, RepoId};
use rdy_store::StoreReader;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::take_batch::{TagChoice, UploadFacts, UploadTarget, check_input_bounds};
use std::cell::Cell;
use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// What a publication take is for: the upload's target and the profile the site publishes, by
/// its full profile id or `implicit:org` (never a key, which a later landing may move).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationTarget {
    pub upload: UploadTarget,
    pub profile: String,
}

/// What the host's verdict judges: the repo, the tag that survives the take (its name and hash),
/// its landing, and the requested profile's facts in that tag's snapshot. `profile` is None when
/// the tag holds no block of the requested full id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationFacts {
    pub repo_id: RepoId,
    pub tag: String,
    pub tag_hash: Hash,
    pub landing: Hash,
    pub profile: Option<ProfileFacts>,
}

/// A publication take that sealed: the take's outcome and the facts its verdict accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationTaken {
    pub outcome: TakeOutcome,
    pub facts: PublicationFacts,
}

/// Why a publication take appended nothing: core's refusal or failure, or the host's own
/// verdict, returned unchanged.
#[derive(Debug)]
pub enum PublicationRefusal<E> {
    Core(RdyError),
    Verdict(E),
}

impl<E> From<RdyError> for PublicationRefusal<E> {
    fn from(e: RdyError) -> Self {
        PublicationRefusal::Core(e)
    }
}

/// The longest selector a take reads, in UTF-8 bytes (`check_stable_selector`'s bound): a
/// longer one is refused before any input is read.
const SELECTOR_BYTES: usize = 128;

/// The bounds of a publication take of `files` batch files for `target`, checked before
/// anything is allocated or read: the take's input bounds, then the selector's length. The
/// selector's meaning is checked after the take's own checks.
pub fn check_bounds(files: usize, target: &PublicationTarget) -> Result<(), RdyError> {
    check_input_bounds(files, &target.upload).map_err(to_rdy)?;
    if target.profile.len() > SELECTOR_BYTES {
        check_stable_selector(&target.profile)?;
    }
    Ok(())
}

/// Takes the batch files `batches` of one upload for `target` into the holder `rdy`, with the
/// host's `verdict` on the requested profile's facts between core's checks and the append (see
/// the module docs). A verdict that refuses returns its own error as `Verdict`, unchanged.
pub fn take_publication<E>(
    rdy: &mut Rdy,
    batches: &[File],
    target: PublicationTarget,
    verdict: impl FnOnce(&PublicationFacts) -> Result<(), E>,
) -> Result<PublicationTaken, PublicationRefusal<E>> {
    check_bounds(batches.len(), &target)?;
    let PublicationTarget { upload, profile } = target;
    // the holder's settings, captured under the writer: a take changes none of them
    let state = rdy.read();
    let (title, description) = (state.project_name(), state.project_description());
    let settings = || SourceSettings {
        pack: state.pack(),
        title: &title,
        description: description.as_deref(),
    };
    let reader = rdy.store.reader();
    let (outcome, facts) = take_checked(rdy, batches, upload, |source, upload, choice| {
        let facts = publication_facts(source, &reader, upload, choice, &profile, settings())?;
        match catch_unwind(AssertUnwindSafe(|| verdict(&facts))) {
            Ok(Ok(())) => Ok(facts),
            Ok(Err(e)) => Err(PublicationRefusal::Verdict(e)),
            Err(_) => Err(PublicationRefusal::Core(verdict_panicked())),
        }
    })?;
    Ok(PublicationTaken { outcome, facts })
}

/// E_INTERNAL: the host's verdict panicked; nothing was appended.
fn verdict_panicked() -> RdyError {
    RdyError::new(
        E_INTERNAL,
        "the publication take's verdict panicked",
        "report this as a bug in the host",
    )
}

/// Steps 3 and 4: the facts of `profile` in the root landing of `upload`, read through the
/// checked `source` over the holder `held`, with the holder's `settings`.
fn publication_facts(
    source: &mut dyn ObjectSource,
    held: &StoreReader,
    upload: &UploadFacts,
    choice: &TagChoice,
    profile: &str,
    settings: SourceSettings<'_>,
) -> Result<PublicationFacts, RdyError> {
    let (tag, tag_hash) = match choice {
        TagChoice::Incoming => (upload.root.name.clone(), upload.root_hash),
        TagChoice::Existing { name, hash } => (name.clone(), *hash),
    };
    let landing = upload.root.landing;
    check_stable_selector(profile)?;
    MATERIALIZATIONS.with(|c| c.set(c.get().wrapping_add(1)));
    let m = materialize_landing(source, landing)?;
    // a full id names a block of this tag or nothing; no prefix or key lookup
    let named = profile != IMPLICIT_ORG;
    let absent = named && BlockId::parse(profile).is_ok_and(|id| m.get(&id).is_none());
    let profile = if absent {
        None
    } else {
        let source = ObjectPublicationSource::new(source, held, upload, settings);
        let resolved = resolve_source(&source, &m, profile)?;
        resolved.require_output()?;
        Some(profile_facts(&resolved))
    };
    Ok(PublicationFacts {
        repo_id: upload.repo_id,
        tag,
        tag_hash,
        landing,
        profile,
    })
}

thread_local! {
    static MATERIALIZATIONS: Cell<u64> = const { Cell::new(0) };
    static BLOB_PAYLOAD_READS: Cell<u64> = const { Cell::new(0) };
}

/// Counts one blob chunk payload that a take's publication source returned.
pub(crate) fn count_blob_payload_read() {
    BLOB_PAYLOAD_READS.with(|c| c.set(c.get().wrapping_add(1)));
}

/// Tests only: how many root landings publication takes have materialized on this thread, so a
/// test can check that a limit refuses before any is.
#[doc(hidden)]
pub fn materializations_for_tests() -> u64 {
    MATERIALIZATIONS.with(Cell::get)
}

/// Tests only: how many blob chunk payloads the sources of publication takes have returned on
/// this thread (a take reads none to report its facts).
#[doc(hidden)]
pub fn blob_payload_reads_for_tests() -> u64 {
    BLOB_PAYLOAD_READS.with(Cell::get)
}
