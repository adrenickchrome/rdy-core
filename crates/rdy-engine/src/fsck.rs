//! `rdy fsck`: the store's consistency report.

use crate::signature_check::{
    CheckContext, STATEMENT_FOREIGN_REPO, STATEMENT_MATCH, carried_sources,
    check_signature_carried, repo_ids, subject_context,
};
use crate::{E_LIMIT, E_SIG_INVALID, Rdy, RdyError, sig_invalid, sig_object_mismatch};
use rdy_schema::{Issue, Severity};
use rdy_store::StoreError;

/// The most signature issues one failed fsck returns: past it, the first
/// `MAX_SIGNATURE_ISSUES - 1` and one `E_LIMIT` issue that counts the rest.
pub const MAX_SIGNATURE_ISSUES: usize = 1000;

#[derive(Clone, Debug, serde::Serialize)]
pub struct FsckReport {
    pub frames: usize,
    pub landings: usize,
    pub versions: usize,
    pub views: usize,
    pub landed_changes: usize,
    /// The intents, each with its version chain checked.
    pub intents: usize,
    /// The alternative sets, each with its version chain checked.
    pub alt_sets: usize,
    /// The dismissals, each a `dismiss/` ref naming a review of its own tension.
    pub reviews: usize,
    /// The TRANSITION records, each folded in log order into the open-move tables.
    pub transitions: usize,
    /// The moves a reconcile would log now: not zero when the pack or the rdy build changed
    /// since the last write. Not an error.
    pub unlogged_moves: usize,
    /// The stored signatures, each checked against its subject with no trust file. Present only
    /// when not zero: an unsigned store's report has no such member.
    #[serde(skip_serializing_if = "is_zero")]
    pub signatures: usize,
    /// In a holder: the stored signatures that are valid over their subject but name another
    /// project's repo id (`foreign_repo`), counted as information, not damage. Present only
    /// when not zero.
    #[serde(skip_serializing_if = "is_zero")]
    pub foreign_signatures: usize,
    /// The redaction counts, present only when an active redaction exists: an ordinary store's
    /// report has none of these members.
    #[serde(flatten)]
    pub redaction: Option<FsckRedaction>,
    /// The carried-removal warnings (`W_STOP_BEFORE_REDACTION`), derived from the log and the
    /// current mask. Present only when not empty. A warning is not damage: fsck still succeeds.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<crate::CarriedWarning>,
    /// The project's history (D-GGTYA): absent for a store with no history ref; while it is
    /// incomplete, `held` is the exact count a full recheck of the staged batches finds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<FsckHistory>,
}

/// How much of the project's history the store holds, as fsck counted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FsckHistory {
    pub complete: bool,
    pub held: u64,
    pub total: u64,
}

impl FsckHistory {
    /// The text line: `history: partial (<n> of <m>)`, or `history: complete (<m> records)`.
    pub fn line(&self) -> String {
        if self.complete {
            format!("history: complete ({} records)", self.total)
        } else {
            format!("history: partial ({} of {})", self.held, self.total)
        }
    }
}

/// What an active redaction removed, and the payloads fsck hashed, counted apart (D-3M81W).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FsckRedaction {
    /// The distinct `(kind, hash)` objects whose payloads were removed.
    pub redacted_objects: u64,
    /// The removed payloads' frames, each counted once.
    pub redacted_frames: u64,
    /// The removed payloads' lengths.
    pub redacted_bytes: u64,
    /// The intact data frames whose payload fsck hashed; no removed payload is among them.
    pub checked_payloads: u64,
}

impl Rdy {
    pub fn fsck(&self) -> Result<FsckReport, RdyError> {
        if self.is_holder() {
            return self.fsck_holder();
        }
        self.fsck_in(&self.read())
    }

    /// The project fsck over `st` and its reader: the published state for [`Rdy::fsck`], whose
    /// walks stop at an incomplete history's frontier; a full check's state for
    /// [`Rdy::fsck_full`], which has no frontier. The checks that need the store's own handle
    /// (the frame walk and the tag rules, with the snapshot chains between them) come first
    /// ([`Rdy::fsck_store_part`]); every other check reads `st` alone ([`fsck_state_part`]).
    fn fsck_in(&self, st: &crate::ReadState) -> Result<FsckReport, RdyError> {
        // the snapshot chains between the frames and the tags, so the first refusal stays
        let part = self.fsck_store_part(st, true)?;
        fsck_state_part(st, part)
    }

    /// The checks of a project fsck that need the store's own handle: every log frame verified,
    /// with any active removal's counts, and the tag rules over the landings of `st`'s canon.
    /// With `chains`, every project landing's snapshot chain is checked between the two, as a
    /// project fsck orders them; the state part then skips it.
    fn fsck_store_part(&self, st: &crate::ReadState, chains: bool) -> Result<StorePart, RdyError> {
        let verified = self.store.verify_report()?;
        store_part(st, &self.store.reader(), verified, chains)
    }

    /// The full check: [`Rdy::fsck`] with no partial-history exception. A project whose history
    /// is complete, or that has none, takes the ordinary check. While the history ref is
    /// incomplete, the store must hold every record its checked key file lists, else it is
    /// E_HISTORY_PENDING (the records behind the frontier are missing); then every walk and
    /// check takes the whole history, past the frontier, the missing-link exception of the
    /// canon state link table off (D-4PV25), and the incomplete ref is kept as it is. A
    /// completion runs it before `Store::complete_history` sets the flag.
    pub fn fsck_full(&self) -> Result<FsckReport, RdyError> {
        if self.is_holder() {
            return self.fsck_holder();
        }
        self.capture_full_check()?.finish()
    }

    /// [`Rdy::fsck_full`]'s first part, for a check that runs off the writer (a history
    /// completion on the daemon's blocking pool). It reads what bounds the check and nothing
    /// whole: the redaction epoch, the published state, the incomplete history it names, the
    /// full check's own state (its heads; every cell is built on first read) and the frame
    /// check's capture (`Store::capture_frame_check`: a reader, the log's end and its
    /// authorization). [`FullCheck::finish`] runs every check on those captures, with no store
    /// handle: the refusal while records are missing, the frame walk, the tag rules, the
    /// snapshot chains and the rest. A holder is refused: it has no history.
    pub fn capture_full_check(&self) -> Result<FullCheck, RdyError> {
        if self.is_holder() {
            return Err(RdyError::new(
                crate::E_BAD_INPUT,
                "a holder has no project history to check",
                "run rdy fsck",
            ));
        }
        let epoch = self.store.redaction_epoch();
        let published = self.read();
        let pending = published
            .partial_history()?
            .map(|(remote, history)| (remote.to_string(), history.clone()));
        let state = match &pending {
            None => std::sync::Arc::clone(&published),
            Some(_) => std::sync::Arc::new(self.full_history_state()?),
        };
        let frames = self.store.capture_frame_check()?;
        Ok(FullCheck {
            frontier: pending.as_ref().map(|(_, h)| h.frontier),
            published,
            state,
            pending,
            reader: self.store.reader(),
            frames,
            epoch,
        })
    }
}

/// The store part of a project fsck over captured reads: `verified` (the frame walk's report),
/// then with `chains` every project landing's snapshot chain, then the tag rules over `st`'s
/// canon landings, read through `reader`.
fn store_part(
    st: &crate::ReadState,
    reader: &rdy_store::StoreReader,
    verified: rdy_store::redaction_io::VerifyReport,
    chains: bool,
) -> Result<StorePart, RdyError> {
    if chains {
        check_chains(st)?;
    }
    let landings = st.reader().landings()?;
    let chain: Vec<rdy_model::Hash> = landings.iter().map(|(h, _)| *h).collect();
    crate::history_fetch::check_point("tags");
    let versions = crate::version::check_tags_in(reader, &chain)?;
    Ok(StorePart {
        frames: verified.frames,
        redaction: verified.redaction.map(|c| FsckRedaction {
            redacted_objects: c.objects,
            redacted_frames: c.frames,
            redacted_bytes: c.bytes,
            checked_payloads: c.checked_payloads,
        }),
        landings: landings.len(),
        versions,
        chains,
    })
}

/// What a project fsck's store part found: the frames, the removal counts, the canon landings
/// and the tags checked.
#[derive(Clone, Copy, Debug)]
struct StorePart {
    frames: usize,
    redaction: Option<FsckRedaction>,
    landings: usize,
    versions: usize,
    /// Whether the snapshot chains were checked already.
    chains: bool,
}

/// A full check captured by [`Rdy::capture_full_check`]: the published state, the incomplete
/// history it names, the full check's own state, a store reader, the frame check's capture, and
/// the incomplete history's frontier and the redaction epoch it saw (frontier None for a
/// project with no incomplete history). It holds no store handle, so it may move to another
/// thread.
pub struct FullCheck {
    published: std::sync::Arc<crate::ReadState>,
    pending: Option<(String, rdy_store::sync_state::SyncHistory)>,
    state: std::sync::Arc<crate::ReadState>,
    reader: rdy_store::StoreReader,
    frames: rdy_store::FrameCheck,
    pub frontier: Option<rdy_model::Hash>,
    pub epoch: u64,
}

impl FullCheck {
    /// Runs every check of the full check on the captures. The report is
    /// [`Rdy::fsck_full`]'s: while the history ref is incomplete, a store that does not hold
    /// every record its checked key file lists is E_HISTORY_PENDING before any other check.
    pub fn finish(self) -> Result<FsckReport, RdyError> {
        if let Some((remote, history)) = &self.pending {
            let st = &self.published;
            let held = crate::partial_history::store_held(st.reader(), st.root(), remote, history)?;
            if held < history.records {
                return Err(st
                    .history_progress()?
                    .expect("an incomplete history has a progress")
                    .refusal());
            }
        }
        crate::history_fetch::check_point("frames");
        let verified = self.frames.run()?;
        // a project with no incomplete history keeps a project fsck's order (the snapshot
        // chains between the frames and the tags); a completion's check runs them last
        let part = store_part(&self.state, &self.reader, verified, self.pending.is_none())?;
        fsck_state_part(&self.state, part)
    }
}

/// The checks of a project fsck that read `st` alone: every project landing's snapshot chain,
/// the objects, the reconcile of the open moves, the signatures, the carried warnings and the
/// history member.
fn fsck_state_part(st: &crate::ReadState, part: StorePart) -> Result<FsckReport, RdyError> {
    crate::history_fetch::check_point("state");
    let reader = st.reader();
    if !part.chains {
        check_chains(st)?;
    }
    let objects = reader.check_objects()?;
    let unlogged = crate::transitions::reconcile_moves(st, None, &[], None)?;
    let (signatures, foreign_signatures) = check_signatures(st)?;
    Ok(FsckReport {
        frames: part.frames,
        landings: part.landings,
        versions: part.versions,
        views: objects.views,
        landed_changes: objects.landed_changes,
        intents: objects.intents,
        alt_sets: objects.alt_sets,
        reviews: objects.reviews,
        transitions: objects.transitions,
        unlogged_moves: crate::transitions::count(&unlogged),
        signatures,
        foreign_signatures,
        redaction: part.redaction,
        warnings: st.carried_warnings()?,
        history: fsck_history(st)?,
    })
}

/// Every project landing's snapshot, off canon too, by the chain checks a holder check, a take
/// and a load share (`rdy_store::snapshot::check_snapshot_chains`); a verified foreign-removal
/// proof's landing keeps the foreign log rules only.
fn check_chains(st: &crate::ReadState) -> Result<(), RdyError> {
    let reader = st.reader();
    let all = reader.project_landings()?;
    let refs: Vec<(rdy_model::Hash, &rdy_model::LandingRecord)> =
        all.iter().map(|(h, r)| (*h, r)).collect();
    reader.check_snapshot_chains(&refs)?;
    Ok(())
}

/// The history member of the report: each staged batch rechecked whole (every payload hashed),
/// then the listed keys held counted once each.
fn fsck_history(st: &crate::ReadState) -> Result<Option<FsckHistory>, RdyError> {
    let reader = st.reader();
    let all = reader.histories()?;
    let Some((remote, h)) = all
        .iter()
        .find(|(_, h)| !h.complete)
        .or_else(|| all.first())
    else {
        return Ok(None);
    };
    let p = crate::partial_history::checked_progress_of(reader, st.root(), remote, h)?;
    Ok(Some(FsckHistory {
        complete: p.complete,
        held: p.held,
        total: p.total,
    }))
}

impl Rdy {
    /// A holder's fsck (D-BPYN8): the log's frames and any active removal, the tags by the
    /// rules every store keeps (no canon membership, no chronological order), then the store's
    /// holder check of every tag's history (`StoreReader::check_holder`) and the signatures.
    /// The report has the project report's members: `landings` and `landed_changes` count the
    /// distinct landings and landed changes across the tags' chains; a holder has no views,
    /// transitions or unlogged moves. No pack validates anything.
    fn fsck_holder(&self) -> Result<FsckReport, RdyError> {
        let verified = self.store.verify_report()?;
        let versions = self.check_holder_tags()?;
        let objects = self
            .store
            .check_holder()
            .map_err(crate::holder::holder_error)?;
        // the distinct landings of every tag's chain, read from the store's tags as checked;
        // the chains are walked together, so each landing is read once
        let heads: Vec<rdy_model::Hash> = self
            .store
            .tags()?
            .into_iter()
            .map(|(_, _, tag)| tag.landing)
            .collect();
        let landings = self
            .store
            .tag_histories(&heads)
            .map_err(crate::holder::holder_error)?
            .len();
        let (signatures, foreign_signatures) = check_signatures(&self.read())?;
        Ok(FsckReport {
            frames: verified.frames,
            landings,
            versions,
            views: objects.views,
            landed_changes: objects.landed_changes,
            intents: objects.intents,
            alt_sets: objects.alt_sets,
            reviews: objects.reviews,
            transitions: objects.transitions,
            unlogged_moves: 0,
            signatures,
            foreign_signatures,
            redaction: verified.redaction.map(|c| FsckRedaction {
                redacted_objects: c.objects,
                redacted_frames: c.frames,
                redacted_bytes: c.bytes,
                checked_payloads: c.checked_payloads,
            }),
            warnings: self.read().carried_warnings()?,
            history: None,
        })
    }
}

/// Checks every stored signature, streamed in subject then hash order, with the same check
/// as every other surface and no trust file: an unknown key is no issue. Returns the count
/// and, of them, the signatures of another project a holder cannot verify (`foreign_repo`,
/// information, not damage); an invalid or mismatched signature fails with the first
/// issue's code, message and fix and at most [`MAX_SIGNATURE_ISSUES`] issues.
fn check_signatures(st: &crate::ReadState) -> Result<(usize, usize), RdyError> {
    let mut count = 0usize;
    let mut foreign = 0usize;
    let mut failed = 0usize;
    let mut issues: Vec<Issue> = Vec::new();
    let mut context: Option<CheckContext> = None;
    // the landings a carried signature of the subject may name (D-9YAQS)
    let mut carried: Vec<(rdy_model::Hash, rdy_model::LandingRecord)> = vec![];
    let mut read_error: Option<RdyError> = None;
    // the repo ids a signature may name, read once, at the first signature
    let mut repos: Option<(rdy_model::RepoId, Vec<rdy_model::RepoId>)> = None;
    let visited = st.reader().visit_signatures(&mut |hash, g| {
        count += 1;
        // the visit is in subject order: one context serves a subject's signatures
        if context
            .as_ref()
            .is_none_or(|c| c.subject.hash() != g.subject)
        {
            let read = match &repos {
                Some(r) => Ok(r.clone()),
                None => repo_ids(st).inspect(|r| repos = Some(r.clone())),
            };
            match read
                .and_then(|(current, sources)| subject_context(st, &g.subject, current, sources))
                .and_then(|c| carried_sources(st, &c).map(|from| (c, from)))
            {
                Ok((c, from)) => {
                    context = Some(c);
                    carried = from;
                }
                Err(e) => {
                    read_error = Some(e);
                    return Err(StoreError::Refused("fsck stopped".into()));
                }
            }
        }
        let Some(c) = context.as_ref() else {
            return Ok(());
        };
        let check = check_signature_carried(g, c, None, &carried);
        if check.statement == STATEMENT_MATCH {
            return Ok(());
        }
        if check.statement == STATEMENT_FOREIGN_REPO {
            foreign += 1;
            return Ok(());
        }
        failed += 1;
        if issues.len() < MAX_SIGNATURE_ISSUES {
            let hash = hash.to_string();
            let e = if check.code.as_deref() == Some(E_SIG_INVALID) {
                sig_invalid(&hash)
            } else {
                sig_object_mismatch(&hash)
            };
            issues.push(Issue {
                severity: Severity::Error,
                code: e.code,
                block: None,
                message: e.message,
                fix: e.fix,
            });
        }
        Ok(())
    });
    if let Some(e) = read_error {
        return Err(e);
    }
    visited?;
    if failed == 0 {
        return Ok((count, foreign));
    }
    if failed > MAX_SIGNATURE_ISSUES {
        issues.truncate(MAX_SIGNATURE_ISSUES - 1);
        issues.push(Issue {
            severity: Severity::Error,
            code: E_LIMIT,
            block: None,
            message: format!(
                "fsck omitted {} signature issues",
                failed - (MAX_SIGNATURE_ISSUES - 1)
            ),
            fix: "fix the reported signatures, then run rdy fsck again".into(),
        });
    }
    let first = &issues[0];
    let mut e = RdyError::new(first.code, first.message.clone(), first.fix.clone());
    e.issues = issues;
    Err(e)
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(redaction: Option<FsckRedaction>) -> FsckReport {
        FsckReport {
            frames: 12,
            landings: 1,
            versions: 0,
            views: 1,
            landed_changes: 2,
            intents: 0,
            alt_sets: 0,
            reviews: 0,
            transitions: 0,
            unlogged_moves: 0,
            signatures: 0,
            foreign_signatures: 0,
            redaction,
            warnings: Vec::new(),
            history: None,
        }
    }

    #[test]
    fn rdx4_fsck_json_adds_the_four_members_only_with_an_active_redaction() {
        let plain = serde_json::to_value(report(None)).unwrap();
        let keys: std::collections::BTreeSet<&str> = plain
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "frames",
                "landings",
                "versions",
                "views",
                "landed_changes",
                "intents",
                "alt_sets",
                "reviews",
                "transitions",
                "unlogged_moves"
            ]),
            "ordinary fsck JSON is unchanged"
        );
        let redacted = serde_json::to_value(report(Some(FsckRedaction {
            redacted_objects: 3,
            redacted_frames: 4,
            redacted_bytes: 2_000,
            checked_payloads: 8,
        })))
        .unwrap();
        let o = redacted.as_object().unwrap();
        assert_eq!(o.len(), 14);
        assert_eq!(o["redacted_objects"], 3);
        assert_eq!(o["redacted_frames"], 4);
        assert_eq!(o["redacted_bytes"], 2_000);
        assert_eq!(o["checked_payloads"], 8);
    }
}
