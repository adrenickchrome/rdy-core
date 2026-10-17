//! The human landing seals its redaction applies (plan Redaction, Task 7; D-CC90E: "It lands
//! only with a human approval"; D-600CW: the removal record stays in the log).
//!
//! `land_atomic` finds the carriers of the approved proposal, checks them and their requests at
//! the store boundary, and writes one REDACTION_APPLY per request after the landing, in the
//! landing's own batch. The layouts are compared with the shared raw fixture
//! (`redaction_fixture::authorized_batch_fixture`).
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget, RedactionApply, RedactionResidue};
use rdy_model::*;
use rdy_store::kinds::{self, BATCH_COMMIT, BLOCK};
use rdy_store::transition::Transitions;
use rdy_store::{LandOutcome, LandRequest, Relay, Store, StoreError};
use redaction_fixture::{
    Draft, FixtureOptions, ORIGIN, Proof, RequestSpec, authorized_batch_fixture, build_request,
    frames, hash_of, relink, segment_bytes,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SECRET: &[u8] = b"canary alpha secret";
const BLOB_CANARY: &[u8] = b"blob canary charlie";

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn writer() -> Provenance {
    Provenance {
        actor: "agent:w".into(),
        run: Some("w1".into()),
        role: Role::Writer,
        at_ms: 10,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: writer(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn version(b: &Block) -> RedactTarget {
    RedactTarget::Version {
        id: b.id.clone(),
        version: b.version_hash(),
    }
}

/// The person who approves, with `role`.
fn person(role: Role) -> Provenance {
    Provenance {
        actor: "human:alice".into(),
        run: None,
        role,
        at_ms: 50,
    }
}

fn relay() -> Relay {
    Relay {
        by: ActorRun {
            actor: "agent:relay".into(),
            run: Some("r1".into()),
        },
        evidence: RelayEvidence {
            quoted: "Approve".into(),
            session_ref: "session-1".into(),
            question: Some("Remove the content?".into()),
        },
    }
}

fn card() -> ApprovalEvidence {
    ApprovalEvidence {
        kind: EvidenceKind::Card,
        options: vec!["Approve".into(), "Reject".into()],
        choice: Some("Approve".into()),
        card_hash: Some(Hash::of(b"redaction card")),
    }
}

/// A landing request for `proposal`, by `approver`, relayed (with card evidence) or local.
fn request(proposal: Hash, approver: Provenance, relayed: bool) -> LandRequest {
    LandRequest {
        proposal,
        accept: None,
        approver,
        alt: None,
        relay: relayed.then(relay),
        evidence: relayed.then(card),
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether any segment file of the store at `path` holds `needle`.
fn on_disk(path: &Path, needle: &[u8]) -> bool {
    segment_bytes(path).values().any(|b| contains(b, needle))
}

/// The content of every case: in view `src`, change `c1` creates the secret block `a` and the
/// bystander `b`; a one-chunk blob holds a canary; in view `work`, the ordinary change `c0`
/// creates `x`.
struct World {
    _dir: tempfile::TempDir,
    path: PathBuf,
    a: Block,
    b: Block,
    x: Block,
    c0: Hash,
    c1: Hash,
    blob: Hash,
}

impl World {
    fn new() -> (World, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let mut s = Store::create(&path).unwrap();
        let a = block("b-00001", std::str::from_utf8(SECRET).unwrap());
        let b = block("b-00002", "bravo stays");
        let x = block("b-00003", "ordinary edit");
        let c1 = s
            .commit_to_view(
                "src",
                change(
                    "src",
                    vec![
                        Op::Create { block: a.clone() },
                        Op::Create { block: b.clone() },
                    ],
                ),
            )
            .unwrap();
        let blob = s.put_blob(&BLOB_CANARY.repeat(64)).unwrap();
        let c0 = s
            .commit_to_view(
                "work",
                change("work", vec![Op::Create { block: x.clone() }]),
            )
            .unwrap();
        let w = World {
            _dir: dir,
            path,
            a,
            b,
            x,
            c0,
            c1,
            blob,
        };
        (w, s)
    }

    /// The request for the secret version, in `view`.
    fn secret(&self, view: &str) -> RequestSpec {
        RequestSpec::new(view, vec![version(&self.a)])
    }

    /// The request for the blob, in `view`.
    fn blob_spec(&self, view: &str) -> RequestSpec {
        RequestSpec::new(view, vec![RedactTarget::Blob(self.blob)]).at(41)
    }
}

/// Records `spec` through the store's checked writer.
fn record(s: &mut Store, spec: &RequestSpec) -> Draft {
    let d = build_request(s, spec);
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    assert_eq!(s.commit_redaction(p).unwrap(), d.request_id());
    d
}

/// The apply record `d`'s request gets from landing `landing`.
fn apply_of(d: &Draft, landing: Hash) -> RedactionApply {
    RedactionApply {
        request: d.request_id(),
        landing,
        report_hash: d.report_hash(),
    }
}

/// A batch's frames as the comparison sees them: the landing, each apply, the transition, and
/// every run of other frames as one `other`.
fn shape(kinds: &[u8]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec![];
    for k in kinds {
        let class = match *k {
            kinds::LANDING => "landing",
            kinds::REDACTION_APPLY => "apply",
            kinds::TRANSITION => "transition",
            _ => "other",
        };
        if class != "other" || out.last() != Some(&"other") {
            out.push(class);
        }
    }
    out
}

/// The data frames of the last sealed batch in the log, read back from the log: (kind, hash),
/// and whether a batch marker ends it.
fn last_logged_batch(s: &Store) -> (Vec<(u8, Hash)>, bool) {
    let all = frames(&s.reader(), s.log_end());
    let markers: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind == BATCH_COMMIT)
        .map(|(i, _)| i)
        .collect();
    let last = *markers.last().unwrap();
    let from = markers.iter().rev().nth(1).map_or(0, |i| i + 1);
    let batch = all[from..last].iter().map(|f| (f.kind, f.hash)).collect();
    (batch, last == all.len() - 1)
}

fn no_apply(s: &Store) -> bool {
    let mut none = true;
    s.for_each_record_from(ORIGIN, s.log_end(), |_, f, _| {
        let kind = match f {
            rdy_store::redaction::StoredFrame::Present { kind, .. }
            | rdy_store::redaction::StoredFrame::Dropped { kind, .. } => kind,
        };
        if kind == kinds::REDACTION_APPLY {
            none = false;
        }
        Ok(())
    })
    .unwrap();
    none
}

/// Asserts a refused landing wrote nothing and activated nothing.
fn assert_untouched(s: &Store, path: &Path, end: rdy_store::segment::Location, writes: u64) {
    assert_eq!(s.log_end(), end, "nothing appended");
    assert_eq!(s.write_count(), writes, "no write");
    assert!(no_apply(s), "no apply record");
    assert!(on_disk(path, SECRET), "the payload is intact");
}

/// Lands `p` again with held batches on: the store's own checks refuse it before anything joins
/// the batch (the held path has no seal-time validation to fall back on yet).
fn assert_held_refuses(s: &mut Store, p: Hash, want: fn(&StoreError) -> bool) {
    s.set_held(true).unwrap();
    let e = s
        .land_atomic(request(p, person(Role::Approver), false))
        .unwrap_err();
    assert!(want(&e), "held: {e:?}");
    assert!(s.open_batch().is_none(), "nothing joined a held batch");
    s.set_held(false).unwrap();
}

#[test]
fn rdx7_a_landing_and_its_apply_are_one_batch() {
    for (relayed, held) in [(false, false), (true, false), (false, true), (true, true)] {
        let case = format!("relayed {relayed}, held {held}");
        let (w, mut s) = World::new();
        let d1 = record(&mut s, &w.secret("work"));
        let d2 = record(&mut s, &w.blob_spec("work"));
        let p = s.propose("work", "Remove content", writer()).unwrap();
        assert_eq!(
            s.proposal(&p).unwrap().changes.len(),
            3,
            "{case}: the ordinary change and both carriers share the proposal"
        );
        let writes = s.write_count();
        s.set_held(held).unwrap();
        let out: LandOutcome = s
            .land_atomic(request(p, person(Role::Approver), relayed))
            .unwrap();
        let transition = Transitions {
            at_ms: 60,
            resolved: vec![],
            opened: vec![],
            gone: vec![],
            new: vec![],
        };
        if held {
            s.seal(vec![transition.frame()]).unwrap();
        }
        assert_eq!(s.write_count(), writes + 1, "{case}: one batch");
        let rec = s.landing(&out.landing).unwrap();
        assert_eq!(
            rec.assurance,
            if relayed {
                Assurance::Relayed
            } else {
                Assurance::Local
            }
        );
        for c in [w.c0, d1.carrier_id(), d2.carrier_id()] {
            assert_eq!(s.landing_of(&c).unwrap(), Some(out.landing), "{case}");
        }

        // the batch as logged: the landing, then one apply per request in request-hash order,
        // the optional transition last, and the marker
        let (batch, sealed) = last_logged_batch(&s);
        assert!(sealed, "{case}: a marker ends the batch");
        assert_eq!(batch, s.last_batch(), "{case}");
        let mut drafts = [&d1, &d2];
        drafts.sort_by_key(|d| d.request_id());
        let applies: Vec<Hash> = batch
            .iter()
            .filter(|(k, _)| *k == kinds::REDACTION_APPLY)
            .map(|(_, h)| *h)
            .collect();
        let want: Vec<Hash> = drafts
            .iter()
            .map(|d| hash_of(&apply_of(d, out.landing)))
            .collect();
        assert_eq!(applies, want, "{case}: landing, request and report bound");
        let kinds: Vec<u8> = batch.iter().map(|(k, _)| *k).collect();
        let landing_at = kinds.iter().position(|k| *k == kinds::LANDING).unwrap();
        assert_eq!(batch[landing_at].1, out.landing, "{case}");
        let first_apply = kinds
            .iter()
            .position(|k| *k == kinds::REDACTION_APPLY)
            .unwrap();
        assert!(first_apply > landing_at, "{case}");
        assert_eq!(
            kinds[first_apply..],
            if held {
                vec![
                    kinds::REDACTION_APPLY,
                    kinds::REDACTION_APPLY,
                    kinds::TRANSITION,
                ]
            } else {
                vec![kinds::REDACTION_APPLY, kinds::REDACTION_APPLY]
            },
            "{case}: the applies end the batch, the transition last"
        );

        // the same layout as the shared fixture's, with the same proof and transition
        let (fw, fs) = World::new();
        drop(fs);
        let proof = if relayed {
            Proof::relayed_human()
        } else {
            Proof::local_human()
        };
        let mut opts = FixtureOptions::approved(proof);
        if held {
            opts.transition = Some(transition.frame().1);
        }
        let fx =
            authorized_batch_fixture(&fw.path, &[fw.secret("work"), fw.blob_spec("work")], &opts);
        let fkinds: Vec<u8> = fx.landing_batch.iter().map(|(k, _)| *k).collect();
        assert_eq!(shape(&kinds), shape(&fkinds), "{case}");
        // the fixture's applies are the same records: same requests and reports, its landing
        let fixture_requests: Vec<(Hash, Hash)> = fx
            .applies
            .iter()
            .map(|(_, a)| (a.request, a.report_hash))
            .collect();
        let ours: Vec<(Hash, Hash)> = drafts
            .iter()
            .map(|d| (d.request_id(), d.report_hash()))
            .collect();
        assert_eq!(fixture_requests, ours, "{case}");

        // the removal finished, and reopening reads the same
        assert!(!on_disk(&w.path, SECRET), "{case}");
        assert!(!on_disk(&w.path, BLOB_CANARY), "{case}");
        let stub = s.get_block(&w.a.version_hash()).unwrap();
        assert!(stub.is_redacted() && stub.body == CONTENT_REMOVED, "{case}");
        assert_eq!(s.get_block(&w.b.version_hash()).unwrap(), w.b, "{case}");
        assert_eq!(
            s.canon_state().unwrap().blocks[&w.x.id],
            w.x.version_hash(),
            "{case}: the ordinary change landed"
        );
        drop(s);
        let s = Store::open(&w.path).unwrap();
        assert!(s.get_block(&w.a.version_hash()).unwrap().is_redacted());
        assert_eq!(
            s.redaction_of(BLOCK, &w.a.version_hash()).unwrap(),
            Some(d1.request_id())
        );
        assert!(s.change(&w.c1).unwrap().is_projection(), "{case}");
    }
}

#[test]
fn rdx7_policy_and_agent_proofs_cannot_authorize() {
    // store calls: an approver role other than Approver, local or relayed, writes nothing
    for (role, relayed) in [
        (Role::Writer, false),
        (Role::Reviewer, false),
        (Role::Reader, false),
        (Role::Writer, true),
        (Role::Reviewer, true),
    ] {
        let (w, mut s) = World::new();
        let d = record(&mut s, &w.secret("work"));
        let p = s.propose("work", "Remove content", writer()).unwrap();
        let (end, writes) = (s.log_end(), s.write_count());
        let e = s
            .land_atomic(request(p, person(role), relayed))
            .unwrap_err();
        assert!(
            matches!(e, StoreError::RedactionHuman),
            "{role:?} relayed {relayed}: {e:?}"
        );
        assert_eq!(
            e.to_string(),
            "Content removal needs a person's approval.",
            "the exact text"
        );
        assert_untouched(&s, &w.path, end, writes);
        assert_eq!(s.landing_of(&d.carrier_id()).unwrap(), None);
        assert_eq!(s.redaction_of(BLOCK, &w.a.version_hash()).unwrap(), None);
        // the held path refuses the same way, before anything joins the batch
        s.set_held(true).unwrap();
        let e = s
            .land_atomic(request(p, person(role), relayed))
            .unwrap_err();
        assert!(matches!(e, StoreError::RedactionHuman), "{e:?}");
        assert!(s.open_batch().is_none(), "nothing joined a held batch");
        s.set_held(false).unwrap();
        assert_untouched(&s, &w.path, end, writes);
    }

    // raw malformed fixtures: the open refuses each proof and erases nothing
    let human = Proof::relayed_human();
    let local = Proof::local_human();
    let mut forms: Vec<(&str, Proof)> = vec![];
    forms.push((
        "policy",
        Proof {
            assurance: Assurance::Policy,
            relayed_by: None,
            relay_evidence: None,
            evidence: None,
            ..local.clone()
        },
    ));
    forms.push((
        "policy with relay fields",
        Proof {
            assurance: Assurance::Policy,
            ..human.clone()
        },
    ));
    forms.push((
        "agent approved_by",
        Proof {
            approved_by: Provenance {
                actor: "agent:bot".into(),
                run: Some("b1".into()),
                role: Role::Writer,
                at_ms: 50,
            },
            ..local.clone()
        },
    ));
    forms.push((
        "relayed without relayed_by",
        Proof {
            relayed_by: None,
            ..human.clone()
        },
    ));
    forms.push((
        "relayed without relay_evidence",
        Proof {
            relay_evidence: None,
            ..human.clone()
        },
    ));
    forms.push((
        "local with relayed_by",
        Proof {
            relayed_by: human.relayed_by.clone(),
            ..local.clone()
        },
    ));
    forms.push((
        "local with relay_evidence",
        Proof {
            relay_evidence: human.relay_evidence.clone(),
            ..local.clone()
        },
    ));
    for (name, proof) in forms {
        let (w, s) = World::new();
        drop(s);
        authorized_batch_fixture(
            &w.path,
            &[w.secret("work")],
            &FixtureOptions::approved(proof),
        );
        let before = segment_bytes(&w.path);
        let e = Store::open(&w.path).err().expect(name);
        assert!(
            matches!(e, StoreError::RedactionCorrupt { .. }),
            "{name}: {e:?}"
        );
        assert_eq!(segment_bytes(&w.path), before, "{name}: nothing erased");
        assert!(on_disk(&w.path, SECRET), "{name}");
    }
}

#[test]
fn rdx7_carriers_must_belong_to_the_approved_history() {
    // wrong proposal: the carrier is in another view, and the landed proposal does not hold it
    {
        let (w, mut s) = World::new();
        let d = record(&mut s, &w.secret("redact"));
        let p = s.propose("work", "Ordinary", writer()).unwrap();
        let out = s
            .land_atomic(request(p, person(Role::Approver), false))
            .unwrap();
        assert_eq!(s.landing_of(&w.c0).unwrap(), Some(out.landing));
        assert_eq!(s.landing_of(&d.carrier_id()).unwrap(), None);
        assert!(no_apply(&s), "wrong proposal: no apply");
        assert!(
            !s.last_batch()
                .iter()
                .any(|(k, _)| *k == kinds::REDACTION_APPLY)
        );
        assert!(on_disk(&w.path, SECRET));
        assert_eq!(s.get_block(&w.a.version_hash()).unwrap(), w.a);
    }

    // missing view membership: a proposal of `work` names a carrier recorded in `redact`
    {
        let (w, mut s) = World::new();
        let d = record(&mut s, &w.secret("redact"));
        let p = s
            .propose_changes(
                "work",
                "Smuggled",
                None,
                writer(),
                vec![w.c0, d.carrier_id()],
                None,
            )
            .unwrap();
        let (end, writes) = (s.log_end(), s.write_count());
        let e = s
            .land_atomic(request(p, person(Role::Approver), false))
            .unwrap_err();
        assert!(matches!(e, StoreError::Refused(_)), "{e:?}");
        assert_untouched(&s, &w.path, end, writes);
        assert_held_refuses(&mut s, p, |e| matches!(e, StoreError::Refused(_)));
        assert_untouched(&s, &w.path, end, writes);
        assert_eq!(s.landing_of(&w.c0).unwrap(), None);
    }

    // a residue that is not its frame's, or a span that names another frame: the request is
    // recorded, but its landing refuses before anything is written
    type Hook = Box<dyn Fn(usize, &mut Draft)>;
    let residue: Hook = Box::new(|_, d: &mut Draft| {
        for r in &mut d.residues {
            if let RedactionResidue::Block(b) = r {
                b.order = "W".into();
            }
        }
        relink(&mut d.request, &d.residues);
    });
    let span: Hook = Box::new(|_, d: &mut Draft| {
        d.request.manifest.spans[0].len += 1;
    });
    for (name, hook) in [("residue", residue), ("span", span)] {
        let (w, s) = World::new();
        drop(s);
        let opts = FixtureOptions {
            on_draft: Some(hook),
            ..FixtureOptions::prepared_only()
        };
        let fx = authorized_batch_fixture(&w.path, &[w.secret("work")], &opts);
        let mut s = Store::open(&w.path).unwrap();
        assert_eq!(
            s.redaction_for_carrier(&fx.requests[0].carrier).unwrap(),
            Some(fx.requests[0].request)
        );
        let p = s.propose("work", "Remove content", writer()).unwrap();
        let (end, writes) = (s.log_end(), s.write_count());
        let e = s
            .land_atomic(request(p, person(Role::Approver), false))
            .unwrap_err();
        assert!(
            matches!(e, StoreError::RedactionCorrupt { .. }),
            "{name}: {e:?}"
        );
        assert_untouched(&s, &w.path, end, writes);
        assert_held_refuses(&mut s, p, |e| {
            matches!(e, StoreError::RedactionCorrupt { .. })
        });
        assert_untouched(&s, &w.path, end, writes);
    }

    // a copy written after the request: a change in another view embeds the secret version
    // again, so the recorded request no longer lists every frame of its target
    {
        let (w, mut s) = World::new();
        record(&mut s, &w.secret("work"));
        s.commit_to_view(
            "other",
            change("other", vec![Op::Create { block: w.a.clone() }]),
        )
        .unwrap();
        let p = s.propose("work", "Remove content", writer()).unwrap();
        let (end, writes) = (s.log_end(), s.write_count());
        let e = s
            .land_atomic(request(p, person(Role::Approver), false))
            .unwrap_err();
        assert!(matches!(e, StoreError::RedactionStale), "{e:?}");
        assert_untouched(&s, &w.path, end, writes);
        assert_held_refuses(&mut s, p, |e| matches!(e, StoreError::RedactionStale));
        assert_untouched(&s, &w.path, end, writes);
    }

    // a wrong request, report or landing in a raw apply: the open refuses it and erases nothing
    type Applies = Box<dyn Fn(&mut Vec<RedactionApply>)>;
    let forms: Vec<(&str, Applies)> = vec![
        (
            "request",
            Box::new(|a: &mut Vec<RedactionApply>| a[0].request = Hash::of(b"another request")),
        ),
        (
            "report",
            Box::new(|a: &mut Vec<RedactionApply>| a[0].report_hash = Hash::of(b"another report")),
        ),
        (
            "landing",
            Box::new(|a: &mut Vec<RedactionApply>| a[0].landing = Hash::of(b"another landing")),
        ),
    ];
    for (name, hook) in forms {
        let (w, s) = World::new();
        drop(s);
        let opts = FixtureOptions {
            on_applies: Some(hook),
            ..FixtureOptions::approved(Proof::local_human())
        };
        authorized_batch_fixture(&w.path, &[w.secret("work")], &opts);
        let before = segment_bytes(&w.path);
        let e = Store::open(&w.path).err().expect(name);
        assert!(
            matches!(e, StoreError::RedactionCorrupt { .. }),
            "{name}: {e:?}"
        );
        assert_eq!(segment_bytes(&w.path), before, "{name}: nothing erased");
    }
}

#[test]
fn rdx7_direct_store_landing_finishes_before_return() {
    // unheld: the landing returns only once the removal is done
    {
        let (w, mut s) = World::new();
        let d = record(&mut s, &w.secret("work"));
        let p = s.propose("work", "Remove content", writer()).unwrap();
        let reader = s.reader();
        let epoch = s.redaction_epoch();
        assert!(on_disk(&w.path, SECRET));
        s.land_atomic(request(p, person(Role::Approver), false))
            .unwrap();
        assert!(!on_disk(&w.path, SECRET), "erased before return");
        assert_eq!(s.redaction_epoch(), epoch + 1, "the epoch advanced once");
        assert!(matches!(
            reader.check_redaction_epoch(),
            Err(StoreError::RedactedState)
        ));
        let stub = s.get_block(&w.a.version_hash()).unwrap();
        assert!(stub.is_redacted() && stub.body == CONTENT_REMOVED);
        assert!(s.verify_report().unwrap().redaction.is_some());
        drop(s);
        // the reopen checks the same human proof from the log alone
        let s = Store::open(&w.path).unwrap();
        assert_eq!(
            s.redaction_of(BLOCK, &w.a.version_hash()).unwrap(),
            Some(d.request_id())
        );
        assert!(!on_disk(&w.path, SECRET));
    }

    // held: the batch erases nothing until it is sealed
    {
        let (w, mut s) = World::new();
        let d = record(&mut s, &w.secret("work"));
        let p = s.propose("work", "Remove content", writer()).unwrap();
        let epoch = s.redaction_epoch();
        s.set_held(true).unwrap();
        let out = s
            .land_atomic(request(p, person(Role::Approver), true))
            .unwrap();
        let open: Vec<u8> = s.open_batch().unwrap().iter().map(|(k, _)| *k).collect();
        assert_eq!(open.last(), Some(&kinds::REDACTION_APPLY));
        assert!(on_disk(&w.path, SECRET), "a held batch erases nothing");
        assert_eq!(s.redaction_epoch(), epoch, "no epoch before the seal");
        let transition = Transitions {
            at_ms: 60,
            resolved: vec![],
            opened: vec![],
            gone: vec![],
            new: vec![],
        };
        s.seal(vec![transition.frame()]).unwrap();
        assert!(!on_disk(&w.path, SECRET), "erased by the seal");
        assert_eq!(s.redaction_epoch(), epoch + 1);
        let (batch, sealed) = last_logged_batch(&s);
        assert!(sealed);
        let kinds: Vec<u8> = batch.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds[kinds.len() - 2..],
            [kinds::REDACTION_APPLY, kinds::TRANSITION]
        );
        assert_eq!(
            batch[batch.len() - 2].1,
            hash_of(&apply_of(&d, out.landing))
        );
        drop(s);
        let s = Store::open(&w.path).unwrap();
        assert!(s.get_block(&w.a.version_hash()).unwrap().is_redacted());
    }
}
