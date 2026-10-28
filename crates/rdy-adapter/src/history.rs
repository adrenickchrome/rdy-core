//! Recorded history facts and object-key presence over one captured snapshot (D-D2NPN,
//! D-WDKWQ).
//!
//! Records and rules belong to core (`rdy_engine::history_facts`); this module pins them to the
//! refs the snapshot captured and shapes nothing else. A landing is readable only when the
//! snapshot's captured canon head or one of its captured tags reaches it, so a landing made after
//! the snapshot is refused. The chains are newest first and bounded at 100000 landings. Many
//! landings by hash are read in one walk of one snapshot (`landings`), never one call per hash.
//! No trust verdict is computed, and no current gate policy or assurance is recomputed.

use crate::store::{Snapshot, StoreHandle};
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

    /// The recorded facts of each landing of `hashes`: one per distinct hash, in the order each
    /// first occurs, each equal to [`Snapshot::landing_facts`] of it. Every landing must lie in
    /// the captured canon history or in a captured tag's history (canon first, then the tags in
    /// name order); one that does not fails the whole call with `E_NOT_FOUND`, naming the first
    /// such hash of the input. The input counts before its repeats collapse: at most 100000
    /// hashes. All the hashes are found in one walk of the captured histories.
    pub fn landings(&self, hashes: &[Hash]) -> Result<Vec<LandingFacts>, RdyError> {
        history_facts::landings(self.read_state(), &self.history_heads(), hashes)
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

impl StoreHandle {
    /// [`Snapshot::landings`] of one snapshot this call captures: a write that lands meanwhile
    /// is not in the answer.
    pub fn landings(&self, hashes: &[Hash]) -> Result<Vec<LandingFacts>, RdyError> {
        let snapshot = self.snapshot()?;
        after_capture();
        snapshot.landings(hashes)
    }
}

#[cfg(test)]
thread_local! {
    /// What runs once on this thread just after the next handle read captures its snapshot.
    static AFTER_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Sets what runs once on this thread just after the next handle read captures its snapshot.
#[cfg(test)]
fn set_after_capture(hook: Box<dyn FnOnce()>) {
    AFTER_CAPTURE.with(|h| *h.borrow_mut() = Some(hook));
}

/// Runs the hook a test set, if any; nothing in any other build.
fn after_capture() {
    #[cfg(test)]
    if let Some(hook) = AFTER_CAPTURE.with(|h| h.borrow_mut().take()) {
        hook();
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
    use std::sync::{Arc, Barrier};

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
            .propose(
                &session(Role::Writer),
                "work",
                "first",
                Some("test summary"),
            )
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
                let p = r.propose(&session(Role::Writer), "work", title, Some("test summary"))?;
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
                    assessment: None,
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

    /// An error as its code, message and fix.
    fn parts(e: RdyError) -> (String, String, String) {
        (e.code.to_string(), e.message, e.fix)
    }

    #[test]
    fn sdc2_handle_batch_captures_one_snapshot() {
        let _clock = rdy_engine::pin_clock(1_000_000);
        let (dir, _, l1) = tagged();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let captured = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let (l2, got) = std::thread::scope(|s| {
            let batch = s.spawn(|| {
                let (captured, resume) = (captured.clone(), resume.clone());
                // the batch waits after its capture until the write below has landed
                set_after_capture(Box::new(move || {
                    captured.wait();
                    resume.wait();
                }));
                handle.landings(&[l1])
            });
            captured.wait();
            // a later landing and a signature of L1, both after the capture
            let l2 = handle
                .local()
                .write(Box::new(move |r| {
                    write(
                        r,
                        json!([{"op": "create", "type": "doc", "fields": {"title": "later", "doc_type": "page"}}]),
                    );
                    let p = r.propose(
                        &session(Role::Writer),
                        "work",
                        "later",
                        Some("test summary"),
                    )?;
                    let l = r.land(&session(Role::Approver), &p.id)?;
                    r.store_mut().put_signature(&Signature {
                        subject: l1,
                        namespace: "rdy-approval-v1".into(),
                        statement: b"x".to_vec(),
                        sig: b"not checked here".to_vec(),
                        signed_by: Provenance {
                            actor: "agent:s".into(),
                            run: None,
                            role: Role::Approver,
                            at_ms: 1,
                        },
                    })?;
                    r.seal_open()?;
                    Ok(Value::String(l))
                }))
                .unwrap();
            resume.wait();
            let l2: Hash = l2.as_str().unwrap().parse().unwrap();
            (l2, batch.join().unwrap().unwrap())
        });
        // the batch read the captured state only: L1 without the later signature
        assert_eq!(hashes(&got), [l1]);
        assert!(got[0].signatures.is_empty());
        // a new capture sees the later landing and the signature
        let now = handle.landings(&[l1, l2]).unwrap();
        assert_eq!(hashes(&now), [l1, l2]);
        assert_eq!(now[0].signatures.len(), 1);
        assert_eq!(now[0].record, got[0].record);
        handle.close().unwrap();
    }

    #[test]
    fn sdc2_batch_unreachable_fails_the_whole_call() {
        let _clock = rdy_engine::pin_clock(1_000_000);
        let (dir, _, l1) = tagged();
        // a stored landing that neither canon nor a tag reaches
        let orphan = {
            let mut r = Rdy::open(dir.path()).unwrap();
            let mut record = r.store().reader().landing(&l1).unwrap();
            // a forged record from a read: it no longer has the read's identity
            record.read_identity = None;
            record.proposal = Hash([7; 32]);
            record.prev = Some(l1);
            let orphan = r.store_mut().put(kinds::LANDING, &record).unwrap();
            r.seal_open().unwrap();
            r.close().unwrap();
            orphan
        };
        let handle = StoreHandle::open(dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let future = land_more(&handle, "second", Some("v2.0.0"));
        let (missing, missing2) = (Hash([9; 32]), Hash([8; 32]));
        assert_eq!(hashes(&snap.landings(&[l1, l1]).unwrap()), [l1]);
        for (input, named) in [
            (vec![l1, missing], missing),
            (vec![future, l1], future),
            (vec![l1, orphan, l1], orphan),
            (vec![missing2, missing, missing2], missing2),
            (vec![l1, missing, future, orphan], missing),
            (vec![orphan, future], orphan),
        ] {
            let e = snap.landings(&input).unwrap_err();
            let want = parts(history_facts::not_reachable(named));
            assert_eq!(parts(e), want, "{input:?}");
            assert_eq!(parts(snap.landing_facts(named).unwrap_err()), want);
        }
        assert_eq!(
            parts(history_facts::not_reachable(missing)),
            (
                E_NOT_FOUND.to_string(),
                format!("no landing {missing} in the captured history"),
                "read landings from canon_chain or tag_history".to_string()
            )
        );
        // a new capture reaches the later landing; the orphan still fails
        assert_eq!(
            hashes(&handle.landings(&[future, l1]).unwrap()),
            [future, l1]
        );
        assert_eq!(
            parts(handle.landings(&[future, orphan]).unwrap_err()),
            parts(history_facts::not_reachable(orphan))
        );
        handle.close().unwrap();
    }
}
