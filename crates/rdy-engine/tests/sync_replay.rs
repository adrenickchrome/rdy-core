//! scc3 (D-QCE8M, D-NME32, D-8ZEHP, D-9YAQS, D-3RSCW, D-HVZC9): a pull's second batch. Canon
//! takes the fetched hub tip, and the unpushed local landings replay onto it oldest first, with
//! their authors, tokens, approvals and signatures. A replay that stops keeps its prefix and puts
//! the rest in a `pull-<n>` view the caller owns; the old tip stays readable under `replaced/`.
//! The whole second batch is one batch.
mod common;

use ed25519_dalek::Signer as _;
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check::{SignatureCheck, signature_checks};
use rdy_engine::signing_read::signature_line;
use rdy_engine::stream_export;
use rdy_engine::sync_fetch::{self, HubState};
use rdy_engine::sync_replay::{
    self, NO_SYNC_STATE, PULLED, PULLED_WITH_CONFLICTS, PullOutcome, PullRequest, StopReason,
};
use rdy_engine::*;
use rdy_model::signature::{Signature, StatementSource, parse_statement, statement};
use rdy_model::*;
use rdy_store::hub_canon::{Replaced, replaced_ref};
use rdy_store::kinds;
use rdy_store::sync_state::{SyncCanon, SyncStep};
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use serde_json::{Value, json};
use ssh_encoding::Encode;
use ssh_key::public::{Ed25519PublicKey, KeyData};
use ssh_key::{Algorithm, HashAlg, SshSig};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

const URL: &str = "https://127.0.0.1:9/r/demo";

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: if role == Role::Approver {
            None
        } else {
            Some("run-a".into())
        },
        role,
        kind: SessionKind::Human,
    }
}

/// The caller of a pull: a person who owns the `pull-<n>` view.
fn puller() -> Session {
    Session {
        actor: "human:puller".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn write_in(r: &mut Rdy, view: &str, ops: Value, message: &str) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: message.into(),
        },
    )
    .unwrap()
}

fn land_view(r: &mut Rdy, view: &str, title: &str, opts: LandOptions) -> Hash {
    let p = r
        .propose(
            &session(Role::Writer),
            view,
            title,
            Some(common::TEST_SUMMARY),
        )
        .unwrap();
    common::confirm_exceptions(r, &session(Role::Approver), &p);
    r.land_with(&session(Role::Approver), &p.id, opts)
        .unwrap()
        .landing
        .parse()
        .unwrap()
}

/// Writes a new doc named `name` in `work` and lands it; returns the landing.
fn land_doc(r: &mut Rdy, name: &str) -> Hash {
    write_in(
        r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
        name,
    );
    land_view(r, "work", name, LandOptions::default())
}

/// Sets the title of block `id` in `work` and lands it; returns the landing.
fn land_title(r: &mut Rdy, id: &str, title: &str) -> Hash {
    write_in(
        r,
        "work",
        json!([{"op": "update", "id": id, "fields": {"title": title}}]),
        title,
    );
    land_view(r, "work", title, LandOptions::default())
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// The hub's copy (`hub`) and the local project (`local`) after one shared landing `base`,
/// which holds doc `doc` and its prose `prose`.
struct World {
    dir: tempfile::TempDir,
    hub: Rdy,
    local: Rdy,
    base: Hash,
    doc: String,
    prose: String,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hub");
    let mut hub = Rdy::init(&a, "rdy-standard").unwrap();
    let w = write_in(
        &mut hub,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "scc3-secret-token"}
        ]),
        "seed",
    );
    let base = land_view(&mut hub, "work", "seed", LandOptions::default());
    drop(hub);
    let b = dir.path().join("local");
    copy_dir(&a, &b);
    World {
        hub: Rdy::open(&a).unwrap(),
        local: Rdy::open(&b).unwrap(),
        base,
        doc: w.created["$d"].clone(),
        prose: w.created["$p"].clone(),
        dir,
    }
}

impl World {
    fn repo(&self) -> RepoId {
        self.hub.store().reader().repo_id().unwrap().unwrap()
    }

    /// Records that the hub held local landing `applied` when this project last synced.
    fn applied(&mut self, applied: Hash) {
        let state = SyncCanon {
            repo_id: self.repo(),
            url: URL.into(),
            fetched: Some(applied),
            applied: Some(applied),
            held: None,
        };
        self.local
            .store_mut()
            .write_sync("origin", vec![SyncStep::Canon(state)])
            .unwrap();
    }

    /// Fetches the hub's canon above `base` into the local project; returns the hub's reply.
    fn fetch(&mut self) -> HubState {
        let tip = self.hub.store().reader().canon_head().unwrap().unwrap();
        let mut out = Vec::new();
        stream_export::write_history(
            &self.hub.read(),
            StreamPurpose::CanonHistory,
            tip,
            Some(self.base),
            &BTreeSet::new(),
            &mut out,
        )
        .unwrap();
        let path: PathBuf = self.dir.path().join("batch");
        std::fs::write(&path, out).unwrap();
        let target = FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: StreamPurpose::CanonHistory,
            root: tip,
            view: None,
        };
        sync_fetch::fetch(&mut self.local, &[File::open(&path).unwrap()], &target).unwrap();
        HubState {
            repo_id: Some(self.repo()),
            canon: Some(tip),
        }
    }

    fn pull_with(&mut self, take: bool, strict: bool) -> Result<PullOutcome, RdyError> {
        let hub = self.fetch();
        self.pull_only(hub, take, strict)
    }

    fn pull_only(
        &mut self,
        hub: HubState,
        take: bool,
        strict: bool,
    ) -> Result<PullOutcome, RdyError> {
        sync_replay::pull(
            &mut self.local,
            &puller(),
            &PullRequest {
                remote: "origin".into(),
                hub,
                take,
                strict,
            },
        )
    }

    fn pull(&mut self) -> PullOutcome {
        self.pull_with(false, false).unwrap()
    }

    /// Closes the local project and opens it again.
    fn reopen(&mut self) {
        let spare = self.dir.path().join("spare");
        if !spare.exists() {
            drop(Rdy::init(&spare, "rdy-standard").unwrap());
        }
        drop(std::mem::replace(
            &mut self.local,
            Rdy::open(&spare).unwrap(),
        ));
        self.local = Rdy::open(&self.dir.path().join("local")).unwrap();
    }

    fn landing(&self, h: &Hash) -> LandingRecord {
        self.local.store().reader().landing(h).unwrap()
    }

    fn canon(&self) -> Option<Hash> {
        self.local.store().reader().canon_head().unwrap()
    }

    fn change(&self, h: &Hash) -> Change {
        self.local.store().reader().change(h).unwrap()
    }

    fn replaced(&self, old: &Hash) -> Option<Replaced> {
        self.local
            .store()
            .reader()
            .get_ref(&replaced_ref(old))
            .unwrap()
            .map(|v| Replaced::decode(&v).unwrap())
    }

    fn applied_tip(&self) -> Option<Hash> {
        self.local
            .store()
            .reader()
            .sync_canon("origin")
            .unwrap()
            .unwrap()
            .applied
    }
}

fn sorted(mut v: Vec<Hash>) -> Vec<Hash> {
    v.sort();
    v
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// The canon chain of `r`, oldest first.
fn chain(r: &Rdy) -> Vec<Hash> {
    let reader = r.store().reader();
    let mut out = vec![];
    let mut at = reader.canon_head().unwrap();
    while let Some(l) = at {
        out.push(l);
        at = reader.landing(&l).unwrap().prev;
    }
    out.reverse();
    out
}

// ---- the software signer (test only, fixed in-memory seed) ----

const SEED: [u8; 32] = [0x61; 32];

fn key_data() -> KeyData {
    KeyData::Ed25519(Ed25519PublicKey(
        ed25519_dalek::SigningKey::from_bytes(&SEED)
            .verifying_key()
            .to_bytes(),
    ))
}

fn fingerprint() -> String {
    let mut blob = vec![];
    key_data().encode(&mut blob).unwrap();
    rdy_engine::signing_crypto::blob_fingerprint(&blob)
}

fn sshsig(ns: &str, msg: &[u8]) -> Vec<u8> {
    let data = SshSig::signed_data(ns, HashAlg::Sha512, msg).unwrap();
    let raw = ed25519_dalek::SigningKey::from_bytes(&SEED).sign(&data);
    let sig = ssh_key::Signature::new(Algorithm::Ed25519, raw.to_bytes().to_vec()).unwrap();
    let mut out = vec![];
    SshSig::new(key_data(), ns, HashAlg::Sha512, sig)
        .unwrap()
        .encode(&mut out)
        .unwrap();
    out
}

/// The signature of `source` for `repo`, with subject `subject`.
fn signed(subject: Hash, source: StatementSource<'_>, repo: RepoId) -> Signature {
    let stmt = statement(source, Some(repo), &fingerprint()).unwrap();
    let ns = parse_statement(&stmt).unwrap().namespace().to_string();
    Signature {
        subject,
        sig: sshsig(&ns, &stmt),
        namespace: ns,
        statement: stmt,
        signed_by: Provenance {
            actor: "agent:signer".into(),
            run: Some("s/1".into()),
            role: Role::Approver,
            at_ms: 1_000,
        },
    }
}

fn put_signature(r: &mut Rdy, g: &Signature) -> Hash {
    let id = r.store_mut().put_signature(g).unwrap();
    r.seal_open().unwrap();
    id
}

fn checks(r: &Rdy, l: &Hash) -> Vec<SignatureCheck> {
    signature_checks(&r.read(), l, None).unwrap()
}

fn relayed() -> LandOptions {
    LandOptions {
        strict: false,
        relay: Some(Relay {
            by: ActorRun {
                actor: "agent:relay".into(),
                run: Some("r-9".into()),
            },
            evidence: RelayEvidence {
                quoted: "Yes, land it.".into(),
                session_ref: "chat-1".into(),
                question: Some("Land it?".into()),
            },
        }),
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Text,
            options: vec![],
            choice: None,
            card_hash: None,
        }),
    }
}

// ---- tests ----

/// D-NME32: L′ copies L's approval fields, takes the new base, changes, state and
/// `rebased_from`, and names no assessment when L names none; one fresh ASSESSMENT of L's
/// approved version at L′'s base is written in the batch. Each new change keeps its source's
/// author, justification, message and briefing token and names the source.
#[test]
fn scc3_replay_preserves_authors_tokens_and_approval() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    write_in(
        &mut w.local,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": "local", "title": "Local", "doc_type": "page"}}]),
        "local work",
    );
    let l = land_view(&mut w.local, "work", "local", relayed());
    let t = land_doc(&mut w.hub, "hub");
    let hub = w.fetch();
    let before = w.local.store().write_count();
    let out = w.pull_only(hub, false, false).unwrap();
    assert_eq!(out.status, PULLED);
    assert_eq!(out.hub_tip, Some(t.to_string()));
    assert!(out.not_replayed.is_empty() && out.view.is_none() && out.replaced.is_none());
    assert_eq!(out.left_canon, [l.to_string()]);
    assert!(!out.no_sync_state);
    // the published state reads the whole second batch
    assert_eq!(
        w.local.read().canon_head(),
        Some(h(&out.replayed[0].landing))
    );
    assert!(w.local.store().write_count() > before);
    let [r] = out.replayed.as_slice() else {
        panic!("{out:?}")
    };
    let l2 = h(&r.landing);
    assert_eq!(r.source, l.to_string());
    assert_eq!(w.canon(), Some(l2));
    assert_eq!(w.applied_tip(), Some(t));
    let (src, new) = (w.landing(&l), w.landing(&l2));
    // the closed field table
    assert_eq!(new.proposal, src.proposal);
    assert_eq!(new.approved_version, src.approved_version);
    assert_eq!(new.approved_by, src.approved_by);
    assert_eq!(new.assurance, src.assurance);
    assert_eq!(new.relayed_by, src.relayed_by);
    assert!(new.relay_evidence.is_some());
    assert_eq!(
        new.relay_evidence, src.relay_evidence,
        "relay evidence is copied"
    );
    assert_eq!(new.evidence, src.evidence);
    assert_eq!(new.chosen, src.chosen);
    assert_eq!(new.checks, src.checks);
    assert_eq!(new.prev, Some(t));
    assert_eq!(new.rebased_from, [l]);
    assert_ne!(new.state, src.state);
    // L names its assessment, so L′ names the fresh one (a landing that names none, such as a
    // re-affirmation's, gives an L′ that names none: `prepare_replay_landing`)
    assert!(src.assessment.is_some());
    assert_eq!(new.assessment, Some(h(&r.assessment)));
    assert_ne!(new.assessment, src.assessment);
    assert_eq!(new.policy, None);
    // the fresh assessment: L's approved version, at L′'s base
    let a = w
        .local
        .store()
        .reader()
        .assessment(&h(&r.assessment))
        .unwrap();
    assert_eq!(a.proposal, src.approved_version.unwrap_or(src.proposal));
    assert_eq!(a.base, Some(t));
    // the changes: one per change of L, in L's order, with the source's author and token
    assert_eq!(new.changes.len(), src.changes.len());
    for (k, (s, n)) in src.changes.iter().zip(&new.changes).enumerate() {
        assert_eq!(r.changes[k].source, s.to_string());
        assert_eq!(r.changes[k].change, n.to_string());
        let (s, n) = (w.change(s), w.change(n));
        assert_eq!(n.provenance, s.provenance, "author");
        assert_eq!(n.justification, s.justification);
        assert_eq!(n.message, s.message);
        assert_eq!(n.ops, s.ops);
        assert_eq!(n.links.briefing, s.links.briefing, "briefing token");
        assert_eq!(n.links.replayed_from, Some(h(&r.changes[k].source)));
    }
    // L stays readable by L′'s rebased_from; the store is whole
    w.local.store().check_objects().unwrap();
}

/// D-QCE8M: the unpushed landings replay oldest first, each onto the one before; the pushed
/// prefix and the hub's own landings never replay.
#[test]
fn scc3_replay_order_and_sources_are_exact() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let pushed = land_doc(&mut w.local, "pushed");
    w.applied(pushed);
    // the hub holds the pushed landing too: it lands on top of it
    drop(w.hub);
    let hub_dir = w.dir.path().join("hub");
    std::fs::remove_dir_all(&hub_dir).unwrap();
    copy_dir(&w.dir.path().join("local"), &hub_dir);
    w.hub = Rdy::open(&hub_dir).unwrap();
    let ls: Vec<Hash> = ["one", "two", "three"]
        .iter()
        .map(|n| land_doc(&mut w.local, n))
        .collect();
    let t1 = land_doc(&mut w.hub, "h1");
    let t2 = land_doc(&mut w.hub, "h2");
    let out = w.pull();
    assert_eq!(out.status, PULLED);
    let sources: Vec<String> = out.replayed.iter().map(|r| r.source.clone()).collect();
    assert_eq!(
        sources,
        ls.iter().map(Hash::to_string).collect::<Vec<_>>(),
        "oldest first, the pushed landing never"
    );
    let new: Vec<Hash> = out.replayed.iter().map(|r| h(&r.landing)).collect();
    assert_eq!(
        chain(&w.local),
        [vec![w.base, pushed, t1, t2], new.clone()].concat()
    );
    for (k, l) in new.iter().enumerate() {
        let rec = w.landing(l);
        assert_eq!(rec.rebased_from, [ls[k]]);
        assert_eq!(rec.prev, Some(if k == 0 { t2 } else { new[k - 1] }));
    }
    assert_eq!(
        out.left_canon,
        ls.iter().map(Hash::to_string).collect::<Vec<_>>()
    );
    assert_eq!(w.applied_tip(), Some(t2));
}

/// D-QCE8M: a conflict stops the replay; the prefix stays, the rest goes into `pull-1`, which
/// the caller owns and which holds the source changes by the view replay (caller authorship),
/// and `replaced/<old tip>` keeps the old chain readable.
#[test]
fn scc3_stop_keeps_prefix_and_moves_remainder_to_owned_view() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let l1 = land_doc(&mut w.local, "first");
    let doc = w.doc.clone();
    let l2 = land_title(&mut w.local, &doc, "Local title");
    let l3 = land_doc(&mut w.local, "third");
    let t = land_title(&mut w.hub, &doc, "Hub title");
    let out = w.pull();
    assert_eq!(out.status, PULLED_WITH_CONFLICTS);
    assert_eq!(out.status, "pulled with conflicts");
    let [r1] = out.replayed.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(r1.source, l1.to_string());
    assert_eq!(w.canon(), Some(h(&r1.landing)));
    assert_eq!(w.landing(&h(&r1.landing)).prev, Some(t));
    let reasons: Vec<(String, StopReason)> = out
        .not_replayed
        .iter()
        .map(|n| (n.landing.clone(), n.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            (l2.to_string(), StopReason::Conflict),
            (l3.to_string(), StopReason::AfterStop)
        ]
    );
    assert_eq!(out.view.as_deref(), Some("pull-1"));
    let reader = w.local.store().reader();
    let head = reader.view_head("pull-1").unwrap().unwrap();
    assert_eq!(
        head.owner.as_ref().map(|o| o.actor.as_str()),
        Some("human:puller")
    );
    assert_eq!(head.base, w.canon());
    let held = w.local.store().included("pull-1").unwrap();
    let from: Vec<Hash> = held
        .iter()
        .map(|c| w.change(c).links.replayed_from.unwrap())
        .collect();
    let want: Vec<Hash> = [l2, l3].iter().flat_map(|l| w.landing(l).changes).collect();
    // the view replay's own order (deps, then hash): the same set of sources
    assert_eq!(sorted(from.clone()), sorted(want));
    for c in held.iter() {
        assert_eq!(
            w.change(c).provenance.actor,
            "human:puller",
            "caller authorship"
        );
    }
    // the old tip and its chain stay readable
    let mark = w.replaced(&l3).expect("replaced mark");
    assert_eq!(mark.by, w.canon().unwrap());
    assert_eq!(out.replaced, Some(l3.to_string()));
    assert_eq!(w.landing(&l3).prev, Some(l2));
    assert_eq!(w.applied_tip(), Some(t));
    w.local.store().check_objects().unwrap();
}

/// D-QCE8M: when the hub chain does not hold `applied`, the hub replaced its canon. The landings
/// up to `applied` leave canon and never replay; only the later ones do.
#[test]
fn scc3_hub_force_never_replays_pushed_prefix() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let pushed = land_doc(&mut w.local, "pushed");
    w.applied(pushed);
    let later = land_doc(&mut w.local, "later");
    // the hub was forced back to `base` and moved on without `pushed`
    let t = land_doc(&mut w.hub, "forced");
    let out = w.pull();
    assert_eq!(out.status, PULLED);
    let sources: Vec<String> = out.replayed.iter().map(|r| r.source.clone()).collect();
    assert_eq!(sources, [later.to_string()]);
    let tip = w.canon().unwrap();
    assert_eq!(chain(&w.local), [w.base, t, tip]);
    assert!(
        chain(&w.local)
            .iter()
            .all(|l| !w.landing(l).rebased_from.contains(&pushed))
    );
    assert_eq!(out.replaced, Some(later.to_string()));
    assert_eq!(w.replaced(&later).unwrap().by, tip);
    assert_eq!(out.left_canon, [pushed.to_string(), later.to_string()]);
    assert_eq!(w.landing(&later).prev, Some(pushed), "readable");
}

/// D-QCE8M: a take moves canon to the hub tip with no replay; the old tip stays readable.
#[test]
fn scc3_take_keeps_old_tip_readable() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let l = land_doc(&mut w.local, "mine");
    let t = land_doc(&mut w.hub, "theirs");
    let out = w.pull_with(true, false).unwrap();
    assert_eq!(out.status, PULLED);
    assert!(out.replayed.is_empty() && out.view.is_none());
    assert_eq!(w.canon(), Some(t));
    assert_eq!(out.replaced, Some(l.to_string()));
    assert_eq!(w.replaced(&l).unwrap().by, t);
    assert_eq!(w.landing(&l).prev, Some(w.base));
    assert_eq!(w.applied_tip(), Some(t));
    assert!(
        w.local
            .store()
            .reader()
            .view_head("pull-1")
            .unwrap()
            .is_none()
    );
}

/// D-8ZEHP: L's signature is carried to L′ with the same statement and signature bytes: no key
/// and no agent. It reads as a match, carried from L.
#[test]
fn scc3_replay_carried_signatures_need_no_agent() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let l = land_doc(&mut w.local, "signed");
    let repo = w.repo();
    let rec = w.landing(&l);
    let g = signed(l, StatementSource::Approval(&rec), repo);
    put_signature(&mut w.local, &g);
    let before = checks(&w.local, &l);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].statement, "match");
    assert_eq!(before[0].carried_from, None);
    land_doc(&mut w.hub, "hub");
    let out = w.pull();
    let [r] = out.replayed.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(r.signatures, 1);
    let l2 = h(&r.landing);
    let ids = w.local.store().reader().signatures_of(&l2).unwrap();
    let [id] = ids.as_slice() else {
        panic!("{ids:?}")
    };
    let carried = w.local.store().reader().signature(id).unwrap();
    assert_eq!(carried.subject, l2);
    assert_eq!(carried.statement, g.statement, "the statement is unchanged");
    assert_eq!(carried.sig, g.sig, "the signature bytes are unchanged");
    assert_eq!(carried.namespace, g.namespace);
    assert_eq!(carried.signed_by, g.signed_by);
    let after = checks(&w.local, &l2);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].signed, "valid");
    assert_eq!(after[0].statement, "match");
    assert_eq!(after[0].code, None);
    assert_eq!(after[0].carried_from, Some(l.to_string()));
    // fsck counts it as matched
    w.local.fsck().unwrap();
}

/// D-9YAQS: a carried signature reports `carried_from` (the full hash) and the CLI line names
/// its first 12 characters; an ordinary one has no such member.
#[test]
fn scc3_carried_signature_reports_carried_from() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let l = land_doc(&mut w.local, "signed");
    let rec = w.landing(&l);
    let repo = w.repo();
    put_signature(
        &mut w.local,
        &signed(l, StatementSource::Approval(&rec), repo),
    );
    land_doc(&mut w.hub, "hub");
    let out = w.pull();
    let l2 = h(&out.replayed[0].landing);
    let ordinary = serde_json::to_value(&checks(&w.local, &l)[0]).unwrap();
    assert!(ordinary.get("carried_from").is_none(), "{ordinary:#}");
    assert!(!signature_line(&ordinary).contains("carried from"));
    let carried = serde_json::to_value(&checks(&w.local, &l2)[0]).unwrap();
    assert_eq!(carried["carried_from"], json!(l.to_string()));
    assert_eq!(carried["statement"], json!("match"));
    let line = signature_line(&carried);
    assert!(
        line.ends_with(&format!(", carried from {}", &l.to_string()[..12])),
        "{line}"
    );
    assert!(!line.contains("statement mismatch"), "{line}");
}

/// D-9YAQS: a signature on L′ whose statement names a base that no landing in L′'s
/// `rebased_from` had, or whose relay evidence differs from L′'s, is a mismatch.
#[test]
fn scc3_carried_signature_rejects_unrelated_base_or_evidence() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    write_in(
        &mut w.local,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": "relayed", "title": "R", "doc_type": "page"}}]),
        "relayed",
    );
    let l = land_view(&mut w.local, "work", "relayed", relayed());
    land_doc(&mut w.hub, "hub");
    let out = w.pull();
    let l2 = h(&out.replayed[0].landing);
    let rec = w.landing(&l2);
    let repo = w.repo();
    // an unrelated base: the source itself is no base a source landing had
    let other = LandingRecord {
        prev: Some(l),
        ..rec.clone()
    };
    let mut odd = signed(l2, StatementSource::Approval(&other), repo);
    odd.signed_by.at_ms = 2_000;
    put_signature(&mut w.local, &odd);
    // a relay statement at the source's base whose evidence is not L′'s
    let mut changed = LandingRecord {
        prev: Some(w.base),
        ..rec.clone()
    };
    changed.relay_evidence.as_mut().unwrap().quoted = "No.".into();
    let mut bad = signed(l2, StatementSource::RelayApproval(&changed), repo);
    bad.signed_by.at_ms = 3_000;
    put_signature(&mut w.local, &bad);
    // the good relay statement at the source's base matches, carried from L
    let good_rec = LandingRecord {
        prev: Some(w.base),
        ..rec.clone()
    };
    let mut good = signed(l2, StatementSource::RelayApproval(&good_rec), repo);
    good.signed_by.at_ms = 4_000;
    put_signature(&mut w.local, &good);
    let mut got: Vec<(i64, String, Option<String>, Option<String>)> = checks(&w.local, &l2)
        .into_iter()
        .map(|c| (c.at_ms, c.statement, c.code, c.carried_from))
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            (
                2_000,
                "mismatch".into(),
                Some("E_SIG_OBJECT_MISMATCH".into()),
                None
            ),
            (
                3_000,
                "mismatch".into(),
                Some("E_SIG_OBJECT_MISMATCH".into()),
                None
            ),
            (4_000, "match".into(), None, Some(l.to_string())),
        ]
    );
}

/// D-QCE8M: the second batch is one batch. A failure inside it leaves canon, `applied`, the
/// views and the marks as the fetch left them, and the fetched records stay; the pull again
/// writes the same result.
#[test]
fn scc3_second_batch_recovery_is_atomic() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let l1 = land_doc(&mut w.local, "one");
    let doc = w.doc.clone();
    let l2 = land_title(&mut w.local, &doc, "Local title");
    let t = land_title(&mut w.hub, &doc, "Hub title");
    let hub = w.fetch();
    let views = |w: &World| w.local.store().reader().view_head("pull-1").unwrap();
    let state = |w: &World| w.local.store().reader().sync_canon("origin").unwrap();
    let before = (w.canon(), state(&w), views(&w));
    assert_eq!(before.0, Some(l2));
    // a failure after the first replay: nothing of the batch stays
    sync_replay::fail_pull_after_for_tests(Some(1));
    let err = w.pull_only(hub, false, false).unwrap_err();
    sync_replay::fail_pull_after_for_tests(None);
    assert_eq!(err.message, "injected pull failure");
    w.reopen();
    assert_eq!((w.canon(), state(&w), views(&w)), before);
    assert!(w.replaced(&l2).is_none());
    assert!(
        w.local
            .store()
            .reader()
            .has_kind(kinds::LANDING, &t)
            .unwrap(),
        "the fetch stays"
    );
    // a store failure inside the batch: the same
    w.local.store_mut().fail_index_apply_after(2);
    assert!(w.pull_only(hub, false, false).is_err());
    w.reopen();
    assert_eq!((w.canon(), state(&w), views(&w)), before);
    // the pull again: the whole result
    let out = w.pull_only(hub, false, false).unwrap();
    assert_eq!(out.status, PULLED_WITH_CONFLICTS);
    assert_eq!(out.replayed[0].source, l1.to_string());
    assert_eq!(out.view.as_deref(), Some("pull-1"));
    assert_eq!(w.applied_tip(), Some(t));
    assert!(w.replaced(&l2).is_some());
    // reopened, it reads the same
    let canon = w.canon();
    w.reopen();
    assert_eq!(w.canon(), canon);
    assert!(views(&w).is_some());
    w.local.store().check_objects().unwrap();
}

/// D-3RSCW: with no usable sync state a pull counts nothing as unpushed. Canon takes the hub
/// tip; the local landings after the newest common landing go into `pull-1`, stay readable
/// under `replaced/`, and the reply says the remote had no sync state.
#[test]
fn scc3_no_state_pull_takes_hub_and_preserves_local_suffix() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    // no applied tip: the fetch writes the state with none
    let a = land_doc(&mut w.local, "a-local");
    let b = land_doc(&mut w.local, "b-local");
    let t = land_doc(&mut w.hub, "hub");
    let out = w.pull();
    assert!(out.no_sync_state);
    assert_eq!(out.status, PULLED_WITH_CONFLICTS);
    assert_eq!(NO_SYNC_STATE, "the remote had no sync state");
    assert!(out.replayed.is_empty(), "no replay");
    assert_eq!(w.canon(), Some(t));
    assert_eq!(
        chain(&w.local),
        [w.base, t],
        "the newest common landing is base"
    );
    assert_eq!(out.left_canon, [a.to_string(), b.to_string()]);
    assert!(
        out.not_replayed
            .iter()
            .all(|n| n.reason == StopReason::NoSyncState)
    );
    assert_eq!(out.view.as_deref(), Some("pull-1"));
    let held = w.local.store().included("pull-1").unwrap();
    let from: Vec<Hash> = held
        .iter()
        .map(|c| w.change(c).links.replayed_from.unwrap())
        .collect();
    let want: Vec<Hash> = [a, b].iter().flat_map(|l| w.landing(l).changes).collect();
    // the view replay's own order (deps, then hash): the same set of sources
    assert_eq!(sorted(from.clone()), sorted(want));
    assert_eq!(w.replaced(&b).unwrap().by, t);
    assert_eq!(w.landing(&b).prev, Some(a));
    assert_eq!(w.applied_tip(), Some(t));
}

/// D-HVZC9: a landing whose `checks` name a removal request stops the replay. The prefix stays,
/// L leaves canon with the later landings, the apply stays, and `pull-1` gets L's other changes
/// and the later changes but never L's carrier change.
#[test]
fn scc3_removal_landing_stops_replay_and_excludes_carrier() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let l1 = land_doc(&mut w.local, "before");
    // L: an ordinary change and the removal of the base's prose
    write_in(
        &mut w.local,
        "redact",
        json!([{"op": "create", "type": "doc", "fields": {"name": "beside", "title": "Beside", "doc_type": "page"}}]),
        "beside",
    );
    let prose = w.prose.clone();
    w.local
        .redact_in(
            &session(Role::Writer),
            "redact",
            RedactInput {
                targets: vec![prose],
                reason_class: "secret".into(),
                reason: "a leaked token".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let l = land_view(&mut w.local, "redact", "remove", LandOptions::default());
    let rec = w.landing(&l);
    assert!(!rec.checks.is_empty(), "a removal landing");
    let l3 = land_doc(&mut w.local, "after");
    land_doc(&mut w.hub, "hub");
    let out = w.pull();
    assert_eq!(out.status, "pulled with conflicts");
    assert_eq!(out.replayed.len(), 1);
    assert_eq!(out.replayed[0].source, l1.to_string());
    assert_eq!(w.canon(), Some(h(&out.replayed[0].landing)));
    let stop = &out.not_replayed[0];
    assert_eq!(stop.landing, l.to_string());
    assert_eq!(stop.reason, StopReason::Removal);
    assert_eq!(
        stop.removals,
        rec.checks.iter().map(Hash::to_string).collect::<Vec<_>>()
    );
    assert_eq!(out.not_replayed[1].landing, l3.to_string());
    // the apply stays: the carrier is still the removal's
    let reader = w.local.store().reader();
    let carriers: Vec<Hash> = rec
        .changes
        .iter()
        .copied()
        .filter(|c| reader.redaction_for_carrier(c).unwrap().is_some())
        .collect();
    assert_eq!(carriers.len(), 1);
    // pull-1: L's other change and the later one, never the carrier
    let held = w.local.store().included("pull-1").unwrap();
    let from: Vec<Hash> = held
        .iter()
        .map(|c| w.change(c).links.replayed_from.unwrap())
        .collect();
    let want: Vec<Hash> = [l, l3]
        .iter()
        .flat_map(|x| w.landing(x).changes)
        .filter(|c| !carriers.contains(c))
        .collect();
    // the view replay's own order (deps, then hash): the same set of sources
    assert_eq!(sorted(from.clone()), sorted(want));
    assert!(!from.contains(&carriers[0]));
    assert!(w.replaced(&l3).is_some());
}

fn agent() -> Session {
    Session::agent("agent:a1".into(), Some("r1".into()), Role::Writer).unwrap()
}

fn policy_fields(actors: Value) -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [{
            "name": "edits",
            "actors": actors,
            "lifecycle": "any",
            "justifications": ["editorial"],
            "decisions": "none",
            "max_blocks": 20,
            "max_items": 20
        }],
        "roles": {"approvers": ["human:founder"]},
        "require": [],
        "owners": []
    })
}

/// The world with an authority policy in its shared base: agents `agent:a*` may land
/// editorial changes. Returns the world and the policy block's id and decision.
fn policed_world() -> (World, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hub");
    let mut hub = Rdy::init(&a, "rdy-standard").unwrap();
    let w = write_in(
        &mut hub,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "scc3-policy-prose"},
            {"op": "create", "type": "doc", "id": "$r", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$r", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        "seed",
    );
    let decision = w.created["$dec"].clone();
    let pol = hub
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "authority_policy", "id": "$pol", "parent": w.created["$r"], "fields": policy_fields(json!(["agent:a*"]))}
                ]))
                .unwrap(),
                justification: Some(Justification::Implements(vec![
                    BlockId::parse(&decision).unwrap(),
                ])),
                message: "the policy".into(),
            },
        )
        .unwrap();
    let base = land_view(&mut hub, "work", "seed", LandOptions::default());
    drop(hub);
    let b = dir.path().join("local");
    copy_dir(&a, &b);
    let world = World {
        hub: Rdy::open(&a).unwrap(),
        local: Rdy::open(&b).unwrap(),
        base,
        doc: w.created["$d"].clone(),
        prose: w.created["$p"].clone(),
        dir,
    };
    (world, pol.created["$pol"].clone(), decision)
}

/// An editorial change to doc `a`'s prose by the agent, landed under the policy.
fn policy_land(r: &mut Rdy, prose: &str, body: &str) -> Hash {
    let view = "agent-edits";
    r.write(
        &agent(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(json!([{"op": "update", "id": prose, "body": body}]))
                .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "clearer".into(),
            }),
            message: body.into(),
        },
    )
    .unwrap();
    let p = r
        .propose(&agent(), view, body, Some(common::TEST_SUMMARY))
        .unwrap();
    r.land_by_policy(&agent(), &p.id)
        .unwrap()
        .landing
        .parse()
        .unwrap()
}

/// D-NME32: a policy landing replays when the policy accepts L′ at its base, and L′ names that
/// policy version and its fresh assessment. When the hub changed the policy so that it refuses
/// L′, the replay stops at L, as a gate failure does, and L goes into `pull-1`.
#[test]
fn scc3_policy_refusal_stops_at_source_landing() {
    let _clock = pin_clock(1_000_000);
    // accepted: the hub's landing leaves the policy alone
    let (mut w, _, _) = policed_world();
    w.applied(w.base);
    let prose = w.prose.clone();
    let l = policy_land(&mut w.local, &prose, "Clearer prose.");
    let src = w.landing(&l);
    assert_eq!(src.assurance, Assurance::Policy);
    assert!(src.policy.is_some() && src.assessment.is_some());
    land_doc(&mut w.hub, "hub");
    let out = w.pull();
    assert_eq!(out.status, PULLED, "{out:?}");
    let new = w.landing(&h(&out.replayed[0].landing));
    assert_eq!(new.assurance, Assurance::Policy);
    assert_eq!(
        new.policy, src.policy,
        "the accepting policy version at the base"
    );
    assert_eq!(new.assessment, Some(h(&out.replayed[0].assessment)));

    // refused: the hub's policy no longer names the agent
    let (mut w, pol, decision) = policed_world();
    w.applied(w.base);
    let prose = w.prose.clone();
    let l1 = land_doc(&mut w.local, "plain");
    let l = policy_land(&mut w.local, &prose, "Clearer prose.");
    w.hub
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "update", "id": pol, "fields": policy_fields(json!(["agent:nobody"]))}
                ]))
                .unwrap(),
                justification: Some(Justification::Implements(vec![
                    BlockId::parse(&decision).unwrap(),
                ])),
                message: "narrow the policy".into(),
            },
        )
        .unwrap();
    let t = land_view(&mut w.hub, "work", "narrow", LandOptions::default());
    let out = w.pull();
    assert_eq!(out.status, PULLED_WITH_CONFLICTS);
    assert_eq!(out.replayed.len(), 1);
    assert_eq!(out.replayed[0].source, l1.to_string());
    assert_eq!(out.not_replayed[0].landing, l.to_string());
    assert_eq!(out.not_replayed[0].reason, StopReason::Policy);
    assert_eq!(w.landing(&w.canon().unwrap()).prev, Some(t));
    assert_eq!(out.view.as_deref(), Some("pull-1"));
}
