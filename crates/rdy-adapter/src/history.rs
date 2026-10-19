//! Recorded history facts and object-key presence over one captured snapshot (D-D2NPN,
//! D-WDKWQ).
//!
//! Records and rules belong to core (`rdy_engine::history_facts`); this module pins them to the
//! refs the snapshot captured and shapes nothing else. A landing is readable only when the
//! snapshot's captured canon head or one of its captured tags reaches it, so a landing made after
//! the snapshot is refused. The chains are newest first and bounded at 100000 landings. No trust
//! verdict is computed, and no current gate policy or assurance is recomputed.

use crate::store::Snapshot;
use rdy_engine::RdyError;
use rdy_engine::history_facts;
use rdy_model::Hash;

pub use rdy_engine::history_facts::{
    BlockOp, BlockWrite, LandingFacts, ObjectPresence, SignatureFact,
};
pub use rdy_model::signature::Signature;
pub use rdy_model::{
    ActorRun, AltChoice, ApprovalEvidence, Assurance, EvidenceKind, LandingRecord, Provenance,
    RelayEvidence, Role,
};
pub use rdy_store::object_refs::ObjectKey;

impl Snapshot {
    /// The heads whose histories the snapshot captured: the canon head, then every captured tag's
    /// landing in name order.
    fn history_heads(&self) -> Vec<Hash> {
        self.canon()
            .into_iter()
            .chain(self.captured_tags().values().map(|(_, tag)| tag.landing))
            .collect()
    }

    /// The recorded facts of landing `hash`. The landing must lie in the captured canon history or
    /// in a captured tag's history; a later landing is `E_NOT_FOUND`.
    pub fn landing_facts(&self, hash: Hash) -> Result<LandingFacts, RdyError> {
        let read = self.read_state();
        if !history_facts::reachable(read, &self.history_heads(), hash)? {
            return Err(history_facts::not_reachable(hash));
        }
        history_facts::landing(read, hash)
    }

    /// The captured canon history, newest landing first. Empty before the first landing, and
    /// always empty in a holder, which has no canon.
    pub fn canon_chain(&self) -> Result<Vec<LandingFacts>, RdyError> {
        match self.canon() {
            Some(head) => history_facts::chain(self.read_state(), head),
            None => {
                self.read_state().ensure_current_epoch()?;
                Ok(Vec::new())
            }
        }
    }

    /// The history of the captured tag `tag` (`1.2.0` finds `v1.2.0`), newest landing first, from
    /// the tag's own landing by `prev`: a divergent tag's chain is its own, never canon's.
    pub fn tag_history(&self, tag: &str) -> Result<Vec<LandingFacts>, RdyError> {
        self.read_state().ensure_current_epoch()?;
        let (_, captured) = self.captured_tag(tag)?;
        history_facts::chain(self.read_state(), captured.landing)
    }

    /// The signatures the snapshot holds for the captured tag `tag`, in signature hash order. At
    /// most 4096; more is the signing `E_LIMIT`.
    pub fn tag_signatures(&self, tag: &str) -> Result<Vec<SignatureFact>, RdyError> {
        self.read_state().ensure_current_epoch()?;
        let (hash, _) = self.captured_tag(tag)?;
        history_facts::signatures(self.read_state(), hash)
    }

    /// Whether the sealed log up to the snapshot's captured end holds each of `keys`, by exact
    /// kind and hash, in key order without repeats. Unreferenced objects count; a removed object
    /// is present and redacted. At most 100000 keys.
    pub fn has_objects(&self, keys: &[ObjectKey]) -> Result<Vec<ObjectPresence>, RdyError> {
        history_facts::presence(self.read_state(), keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreHandle;
    use rdy_engine::redaction::RedactInput;
    use rdy_engine::{E_BAD_INPUT, E_LIMIT, E_NOT_FOUND, Engine, Rdy, Session, SessionKind};
    use rdy_engine::{WriteRequest, WriteResult};
    use rdy_model::Role;
    use rdy_store::LandRequest;
    use rdy_store::kinds;
    use serde_json::{Value, json};

    fn session(role: Role) -> Session {
        Session {
            actor: if role == Role::Approver {
                "human:founder".into()
            } else {
                "agent:a".into()
            },
            run: None,
            role,
            kind: SessionKind::Human,
        }
    }

    fn write(r: &mut Rdy, ops: Value) -> WriteResult {
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "ha10".into(),
            },
        )
        .unwrap()
    }

    /// A closed project: one landing (L1) with a secret prose block, tagged `v1.0.0`.
    fn tagged() -> (tempfile::TempDir, String, Hash) {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let w = write(
            &mut r,
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
            ]),
        );
        let secret = w.created["$p"].clone();
        let p = r
            .propose(&session(Role::Writer), "work", "first", None)
            .unwrap();
        let l1: Hash = r
            .land(&session(Role::Approver), &p.id)
            .unwrap()
            .parse()
            .unwrap();
        r.tag(&session(Role::Approver), "v1.0.0", None, None)
            .unwrap();
        r.close().unwrap();
        (dir, secret, l1)
    }

    /// Lands one more doc through the open handle's own `Local`, optionally tagging it.
    fn land_more(handle: &StoreHandle, title: &'static str, tag: Option<&'static str>) -> Hash {
        let landed = handle
            .local()
            .write(Box::new(move |r| {
                write(
                    r,
                    json!([{"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}]),
                );
                let p = r.propose(&session(Role::Writer), "work", title, None)?;
                let l = r.land(&session(Role::Approver), &p.id)?;
                if let Some(name) = tag {
                    r.tag(&session(Role::Approver), name, None, None)?;
                }
                Ok(Value::String(l))
            }))
            .unwrap();
        landed.as_str().unwrap().parse().unwrap()
    }

    /// Removes every version of `secret` through the store's own landing path.
    fn remove(handle: &StoreHandle, secret: String) {
        handle
            .local()
            .write(Box::new(move |rdy| {
                let s = session(Role::Writer);
                let req = rdy.redact(
                    &s,
                    RedactInput {
                        targets: vec![secret],
                        reason_class: "secret".into(),
                        reason: "a leaked code".into(),
                        dry_run: false,
                    },
                )?;
                assert!(req.redaction.is_some());
                let p = rdy.propose(&s, "work", "Remove the code", None)?;
                let version: Hash = p.id.parse().unwrap();
                rdy.seal_open()?;
                rdy.store_mut().set_held(false)?;
                rdy.store_mut().land_atomic(LandRequest {
                    proposal: version,
                    accept: None,
                    approver: Provenance {
                        actor: "human:t".into(),
                        run: None,
                        role: Role::Approver,
                        at_ms: 1_000_000,
                    },
                    alt: None,
                    relay: None,
                    evidence: None,
                })?;
                rdy.store_mut().set_held(true)?;
                rdy.invalidate();
                Ok(Value::Null)
            }))
            .unwrap();
    }

    fn hashes(chain: &[LandingFacts]) -> Vec<Hash> {
        chain.iter().map(|f| f.hash).collect()
    }

    #[test]
    fn ha10_snapshot_cannot_read_a_later_landing() {
        let _clock = rdy_engine::pin_clock(1_000_000);
        let (dir, _, l1) = tagged();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let before = handle.snapshot().unwrap();
        let l2 = land_more(&handle, "second", Some("v2.0.0"));

        // the older snapshot reaches L1 only: canon and its one tag end there
        assert_eq!(hashes(&before.canon_chain().unwrap()), [l1]);
        assert_eq!(before.landing_facts(l1).unwrap().hash, l1);
        for e in [
            before.landing_facts(l2).unwrap_err(),
            before.landing_facts(Hash([9; 32])).unwrap_err(),
            before.tag_history("v2.0.0").unwrap_err(),
            before.tag_signatures("2.0.0").unwrap_err(),
        ] {
            assert_eq!(e.code, E_NOT_FOUND, "{e}");
        }
        assert_eq!(hashes(&before.tag_history("v1.0.0").unwrap()), [l1]);

        // a new snapshot reaches both, newest first, and finds a tag by its version
        let after = handle.snapshot().unwrap();
        assert_eq!(hashes(&after.canon_chain().unwrap()), [l2, l1]);
        assert_eq!(after.landing_facts(l2).unwrap().hash, l2);
        assert_eq!(hashes(&after.tag_history("2.0.0").unwrap()), [l2, l1]);
        handle.close().unwrap();
    }

    #[test]
    fn ha10_tag_history_is_local_to_its_chain() {
        let _clock = rdy_engine::pin_clock(1_000_000);
        let (dir, _, l1) = tagged();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let l2 = land_more(&handle, "second", Some("v2.0.0"));
        let l3 = land_more(&handle, "third", None);
        let snap = handle.snapshot().unwrap();
        // each tag's chain starts at its own landing; canon's runs past both
        assert_eq!(hashes(&snap.tag_history("v1.0.0").unwrap()), [l1]);
        assert_eq!(hashes(&snap.tag_history("v2.0.0").unwrap()), [l2, l1]);
        assert_eq!(hashes(&snap.canon_chain().unwrap()), [l3, l2, l1]);
        // the untagged landing is reachable through canon
        let facts = snap.landing_facts(l3).unwrap();
        assert_eq!(facts.record.prev, Some(l2));
        assert!(snap.tag_signatures("v1.0.0").unwrap().is_empty());
        handle.close().unwrap();
    }

    #[test]
    fn ha10_canon_chain_is_empty_before_the_first_landing() {
        let dir = tempfile::tempdir().unwrap();
        Rdy::init(dir.path(), "rdy-standard")
            .unwrap()
            .close()
            .unwrap();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        assert!(snap.canon().is_none());
        assert!(snap.canon_chain().unwrap().is_empty());
        assert_eq!(
            snap.landing_facts(Hash([1; 32])).unwrap_err().code,
            E_NOT_FOUND
        );
        handle.close().unwrap();
    }

    #[test]
    fn ha10_presence_stops_at_captured_seq_and_counts_drops() {
        let _clock = rdy_engine::pin_clock(1_000_000);
        let (dir, secret, l1) = tagged();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let first = handle.snapshot().unwrap();
        let facts = first.landing_facts(l1).unwrap();
        let secret_write = facts
            .blocks
            .iter()
            .find(|b| b.id.to_string() == secret)
            .unwrap();
        let secret_key = ObjectKey {
            kind: kinds::BLOCK,
            hash: secret_write.version.unwrap(),
        };
        let landing_key = ObjectKey {
            kind: kinds::LANDING,
            hash: l1,
        };
        let got = first.has_objects(&[landing_key, secret_key]).unwrap();
        assert!(got.iter().all(|p| p.present && !p.redacted), "{got:?}");

        // objects of a later landing are not in the older snapshot's log
        let l2 = land_more(&handle, "second", None);
        let second = handle.snapshot().unwrap();
        let later = ObjectKey {
            kind: kinds::LANDING,
            hash: l2,
        };
        assert!(!first.has_objects(&[later]).unwrap()[0].present);
        assert!(second.has_objects(&[later]).unwrap()[0].present);

        // a removal drops the secret: a snapshot from before it is stale, a new one counts the
        // dropped object as present and redacted
        remove(&handle, secret);
        for e in [
            first.has_objects(&[landing_key]).unwrap_err(),
            second.has_objects(&[landing_key]).unwrap_err(),
        ] {
            assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
        }
        let third = handle.snapshot().unwrap();
        let got = third.has_objects(&[secret_key, landing_key]).unwrap();
        let by_key = |k: ObjectKey| got.iter().find(|p| p.key == k).copied().unwrap();
        assert_eq!(
            (by_key(secret_key).present, by_key(secret_key).redacted),
            (true, true)
        );
        assert_eq!(
            (by_key(landing_key).present, by_key(landing_key).redacted),
            (true, false)
        );
        // the recorded facts keep the original identity and say the content was removed
        let after = third.landing_facts(l1).unwrap();
        let w = after
            .blocks
            .iter()
            .find(|b| b.version == secret_write.version)
            .unwrap();
        assert!(w.redacted);
        handle.close().unwrap();
    }

    #[test]
    fn ha10_presence_refuses_local_kinds_and_too_many_keys() {
        let (dir, _, l1) = tagged();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let key = |kind| ObjectKey { kind, hash: l1 };
        let e = snap.has_objects(&[key(kinds::REF_UPDATE)]).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "object presence kind 8 is not portable",
                "request stored object kinds only"
            )
        );
        let over = vec![key(kinds::LANDING); 100_001];
        let e = snap.has_objects(&over).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "object presence query exceeds 100000 keys",
                "request at most 100000 object keys"
            )
        );
        // repeated keys are one key: 100000 copies are within the limit
        let at = vec![key(kinds::LANDING); 100_000];
        assert_eq!(snap.has_objects(&at).unwrap().len(), 1);
        handle.close().unwrap();
    }
}
