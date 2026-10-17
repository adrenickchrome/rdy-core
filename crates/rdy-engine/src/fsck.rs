//! `rdy fsck`: the store's consistency report.

use crate::{Rdy, RdyError};

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
            redaction: verified.redaction.map(|c| FsckRedaction {
                redacted_objects: c.objects,
                redacted_frames: c.frames,
                redacted_bytes: c.bytes,
                checked_payloads: c.checked_payloads,
            }),
        })
    }
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
