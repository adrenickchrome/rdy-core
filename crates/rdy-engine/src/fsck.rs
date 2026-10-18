//! `rdy fsck`: the store's consistency report.

use crate::signature_check::{
    CheckContext, STATEMENT_MATCH, check_signature, repo_ids, subject_context,
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
    /// The redaction counts, present only when an active redaction exists: an ordinary store's
    /// report has none of these members.
    #[serde(flatten)]
    pub redaction: Option<FsckRedaction>,
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
        let verified = self.store.verify_report()?;
        let frames = verified.frames;
        let landings = self.store.landings()?;
        for (_, rec) in &landings {
            self.store.get::<rdy_model::StateSnapshot>(&rec.state)?;
        }
        let chain: Vec<rdy_model::Hash> = landings.iter().map(|(h, _)| *h).collect();
        let versions = self.check_tags(&chain)?;
        let objects = self.store.check_objects()?;
        let unlogged = crate::transitions::reconcile_moves(&self.read(), None, &[], None)?;
        let signatures = self.check_signatures()?;
        Ok(FsckReport {
            frames,
            landings: landings.len(),
            versions,
            views: objects.views,
            landed_changes: objects.landed_changes,
            intents: objects.intents,
            alt_sets: objects.alt_sets,
            reviews: objects.reviews,
            transitions: objects.transitions,
            unlogged_moves: crate::transitions::count(&unlogged),
            signatures,
            redaction: verified.redaction.map(|c| FsckRedaction {
                redacted_objects: c.objects,
                redacted_frames: c.frames,
                redacted_bytes: c.bytes,
                checked_payloads: c.checked_payloads,
            }),
        })
    }
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
        // the distinct landings of every tag's chain, read from the store's tags as checked
        let mut landings = std::collections::BTreeSet::new();
        for (_, _, tag) in self.store.tags()? {
            if landings.contains(&tag.landing) {
                continue;
            }
            let chain = self
                .store
                .tag_chain(&tag.landing)
                .map_err(crate::holder::holder_error)?;
            landings.extend(chain.into_iter().map(|(h, _)| h));
        }
        let landings = landings.len();
        let signatures = self.check_signatures()?;
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
            redaction: verified.redaction.map(|c| FsckRedaction {
                redacted_objects: c.objects,
                redacted_frames: c.frames,
                redacted_bytes: c.bytes,
                checked_payloads: c.checked_payloads,
            }),
        })
    }

    /// Checks every stored signature, streamed in subject then hash order, with the same check
    /// as every other surface and no trust file: an unknown key is no issue. Returns the count;
    /// an invalid or mismatched signature fails with the first issue's code, message and fix and
    /// at most [`MAX_SIGNATURE_ISSUES`] issues.
    fn check_signatures(&self) -> Result<usize, RdyError> {
        let st = self.read();
        let mut count = 0usize;
        let mut failed = 0usize;
        let mut issues: Vec<Issue> = Vec::new();
        let mut context: Option<CheckContext> = None;
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
                    None => repo_ids(&st).inspect(|r| repos = Some(r.clone())),
                };
                match read.and_then(|(current, sources)| {
                    subject_context(&st, &g.subject, current, sources)
                }) {
                    Ok(c) => context = Some(c),
                    Err(e) => {
                        read_error = Some(e);
                        return Err(StoreError::Refused("fsck stopped".into()));
                    }
                }
            }
            let Some(c) = context.as_ref() else {
                return Ok(());
            };
            let check = check_signature(g, c, None);
            if check.statement == STATEMENT_MATCH {
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
            return Ok(count);
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
            redaction,
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
