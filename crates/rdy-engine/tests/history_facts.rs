//! ha10 (D-D2NPN, D-DAGNB, D-WDKWQ, D-3M81W): the facts a landing recorded, the history of a tag
//! by its own `prev` chain, the signatures a state holds, and the presence of object keys. The
//! expectations come from the stored records and from the store's own readers; nothing is
//! recomputed, and no trust verdict or gate policy is involved.
mod common;

use rdy_engine::history_facts::{self, BlockOp, MAX_KEYS, MAX_LANDINGS};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::signature::{Signature, StatementSource, statement};
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::path::Path;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};
/// A key fingerprint in a statement: `SHA256:` and 43 base64 characters.
const FP: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

// ---------------------------------------------------------------------------------------------
// helpers

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "write to work".into(),
        },
    )
    .unwrap()
}

/// Proposes view `work` under `title` and lands it. Returns the landing.
fn land(r: &mut Rdy, title: &str) -> Hash {
    let p = r
        .propose(&writer(), "work", title, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&founder(), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn project(dir: &Path) -> Rdy {
    Rdy::init(dir, "rdy-standard").unwrap()
}

/// A project loaded from a dump of `r`'s current state: the same history, now free to diverge.
fn fork(r: &Rdy, root: &Path, name: &str) -> Rdy {
    let file = root.join(format!("{name}.rdydump"));
    r.read().dump(&file).unwrap();
    let dir = root.join(name);
    load::load(&file, &dir).unwrap();
    Rdy::open(&dir).unwrap()
}

/// The kinds a copy carries into a holder: what a tag's history reaches.
const COPIED: [u8; 8] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::TAG,
];

/// Every object of `src`'s log of a copied kind, in log order.
fn objects(src: &Rdy) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    src.store()
        .for_each_frame_from(ORIGIN, src.store().log_end(), |_, kind, _, payload, _| {
            if COPIED.contains(&rdy_store::codec::logical_kind(kind)) {
                out.push((kind, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

fn tag_refs(src: &Rdy, names: &[&str]) -> Vec<(String, Vec<u8>)> {
    src.store()
        .tags()
        .unwrap()
        .into_iter()
        .filter(|(n, _, _)| names.contains(&n.as_str()))
        .map(|(n, h, _)| (format!("tag/{n}"), h.0.to_vec()))
        .collect()
}

/// A new holder at `dir` holding `items` and `refs`, closed and opened again.
fn holder_with(dir: &Path, items: Vec<(u8, Vec<u8>)>, refs: Vec<(String, Vec<u8>)>) -> Rdy {
    let mut h = Rdy::create_holder(dir, RepoId([1; 16])).unwrap();
    h.store_mut().write(items).unwrap();
    h.store_mut().set_refs(refs).unwrap();
    h.seal_open().unwrap();
    h.close().unwrap();
    Rdy::open_holder(dir).unwrap()
}

fn hs(n: u8) -> Hash {
    Hash([n; 32])
}

fn session_a(role: Role) -> Session {
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

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: String) {
    let s = session_a(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
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
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

// ---------------------------------------------------------------------------------------------
// recorded facts

#[test]
fn ha10_gate_facts_preserve_every_landing_field() {
    let _clock = pin_clock(1_000_000);
    // a stored landing with every field set: it is read back, never decided again
    let record = LandingRecord {
        proposal: hs(1),
        prev: None,
        state: hs(2),
        approved_by: Provenance {
            actor: "human:founder".into(),
            run: Some("cli".into()),
            role: Role::Approver,
            at_ms: 42,
        },
        changes: vec![],
        chosen: Some(AltChoice {
            set: hs(3),
            member: "left".into(),
        }),
        checks: vec![hs(4)],
        assurance: Assurance::Relayed,
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: Some("run/1".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "yes, land it".into(),
            session_ref: "s/9".into(),
            question: Some("Land this?".into()),
        }),
        approved_version: Some(hs(5)),
        rebased_from: vec![hs(6), hs(7)],
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: vec!["approve".into(), "reject".into()],
            choice: Some("approve".into()),
            card_hash: Some(hs(8)),
        }),
        assessment: None,
        policy: None,
        read_identity: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut h = Rdy::create_holder(dir.path(), RepoId([1; 16])).unwrap();
    let id = h.store_mut().put(kinds::LANDING, &record).unwrap();
    h.seal_open().unwrap();
    h.close().unwrap();
    let h = Rdy::open_holder(dir.path()).unwrap();
    assert_eq!(id, record.id());
    let facts = history_facts::landing(&h.read(), id).unwrap();
    assert_eq!(facts.hash, id);
    assert_eq!(facts.record, record, "every stored field, unchanged");
    assert_eq!(facts.record.assurance, Assurance::Relayed);
    assert!(facts.blocks.is_empty() && facts.signatures.is_empty());

    // a landing of the ordinary path carries its stored record, whole
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path());
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    );
    let l = land(&mut r, "first");
    let stored = r.store().reader().landing(&l).unwrap();
    let facts = history_facts::landing(&r.read(), l).unwrap();
    assert_eq!(facts.record, stored);
    assert_eq!(facts.record.assurance, Assurance::Local);
    assert_eq!(facts.record.approved_by.actor, "human:founder");
    assert!(facts.record.approved_version.is_some());
    assert_eq!(facts.record.changes.len(), 1);
}

#[test]
fn ha10_block_writes_keep_order_and_removed_identities() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path());
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]),
    );
    let (d, p) = (w.created["$d"].clone(), w.created["$p"].clone());
    let w2 = write(
        &mut r,
        json!([
            {"op": "update", "id": p, "body": "The launch code is ZEBRA-7731, twice."},
            {"op": "create", "type": "prose", "id": "$q", "parent": d, "body": "Scratch."}
        ]),
    );
    let q = w2.created["$q"].clone();
    write(&mut r, json!([{"op": "delete", "id": q}]));
    let l = land(&mut r, "three changes");
    // a second landing, so the removal is a third
    write(
        &mut r,
        json!([{"op": "update", "id": d, "fields": {"title": "A2"}}]),
    );
    let l2 = land(&mut r, "second");

    let before = history_facts::landing(&r.read(), l).unwrap();
    let changes = before.record.changes.clone();
    assert_eq!(changes.len(), 3);
    let shape: Vec<(Hash, String, BlockOp, bool, bool)> = before
        .blocks
        .iter()
        .map(|b| {
            (
                b.change,
                b.id.to_string(),
                b.op,
                b.base.is_some(),
                b.version.is_some(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (changes[0], d.clone(), BlockOp::Create, false, true),
            (changes[0], p.clone(), BlockOp::Create, false, true),
            (changes[1], p.clone(), BlockOp::Update, true, true),
            (changes[1], q.clone(), BlockOp::Create, false, true),
            (changes[2], q.clone(), BlockOp::Delete, true, false),
        ],
        "changes in landing order, then ops in order"
    );
    // an update starts from the version the create wrote, a delete from the create of q
    assert_eq!(before.blocks[2].base, before.blocks[1].version);
    assert_eq!(before.blocks[4].base, before.blocks[3].version);
    assert!(before.blocks.iter().all(|b| !b.redacted));
    assert_eq!(BlockOp::Delete.as_str(), "delete");

    let stale = r.read();
    remove(&mut r, p.clone());
    // a state read before the removal is behind the epoch: it answers nothing
    let e = history_facts::landing(&stale, l).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");

    let after = history_facts::landing(&r.read(), l).unwrap();
    assert_eq!(after.hash, before.hash);
    assert_eq!(after.record, before.record);
    assert_eq!(after.blocks.len(), before.blocks.len());
    for (a, b) in after.blocks.iter().zip(&before.blocks) {
        // identities stay the original ones; only the removal flag differs
        assert_eq!(
            (&a.change, &a.id, a.op, a.base, a.version),
            (&b.change, &b.id, b.op, b.base, b.version)
        );
        assert_eq!(a.redacted, a.id.to_string() == p && a.op != BlockOp::Delete);
    }
    assert!(after.blocks[1].redacted && after.blocks[2].redacted);
    // the later landing is unaffected by the removal of another block's content
    let second = history_facts::landing(&r.read(), l2).unwrap();
    assert!(second.blocks.iter().all(|b| !b.redacted));
}

// ---------------------------------------------------------------------------------------------
// histories

/// Two projects that share a first landing (tagged v1.0.0), then diverge: the left one lands
/// `left` (v1.1.0), the right one `right` (v2.0.0).
struct Divergent {
    root: tempfile::TempDir,
    left: Rdy,
    right: Rdy,
    l1: Hash,
    l_left: Hash,
    l_right: Hash,
}

fn divergent() -> Divergent {
    let root = tempfile::tempdir().unwrap();
    let mut left = project(&root.path().join("left"));
    let w = write(
        &mut left,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "base"}
        ]),
    );
    let prose = w.created["$p"].clone();
    let l1 = land(&mut left, "first");
    tag(&mut left, "v1.0.0");
    let mut right = fork(&left, root.path(), "right");
    write(
        &mut left,
        json!([{"op": "update", "id": prose.clone(), "body": "left"}]),
    );
    let l_left = land(&mut left, "left");
    tag(&mut left, "v1.1.0");
    write(
        &mut right,
        json!([{"op": "update", "id": prose, "body": "right"}]),
    );
    let l_right = land(&mut right, "right");
    tag(&mut right, "v2.0.0");
    Divergent {
        root,
        left,
        right,
        l1,
        l_left,
        l_right,
    }
}

impl Divergent {
    fn holder(&self) -> Rdy {
        let mut items = objects(&self.left);
        items.extend(objects(&self.right));
        let mut refs = tag_refs(&self.left, &["v1.0.0", "v1.1.0"]);
        refs.extend(tag_refs(&self.right, &["v2.0.0"]));
        holder_with(&self.root.path().join("holder"), items, refs)
    }
}

fn hashes(chain: &[history_facts::LandingFacts]) -> Vec<Hash> {
    chain.iter().map(|f| f.hash).collect()
}

#[test]
fn ha10_tag_history_is_local_to_its_chain() {
    let d = divergent();
    let h = d.holder();
    let read = h.read();
    let landing_of = |name: &str| {
        h.store()
            .tags()
            .unwrap()
            .into_iter()
            .find(|(n, _, _)| n == name)
            .unwrap()
            .2
            .landing
    };
    // each chain runs from its tag's landing by `prev`, newest first, and no further
    assert_eq!(
        hashes(&history_facts::chain(&read, landing_of("v1.0.0")).unwrap()),
        [d.l1]
    );
    assert_eq!(
        hashes(&history_facts::chain(&read, landing_of("v1.1.0")).unwrap()),
        [d.l_left, d.l1]
    );
    assert_eq!(
        hashes(&history_facts::chain(&read, landing_of("v2.0.0")).unwrap()),
        [d.l_right, d.l1]
    );
    // a landing is reachable only from the heads that pass it
    let left = [landing_of("v1.1.0")];
    assert!(history_facts::reachable(&read, &left, d.l_left).unwrap());
    assert!(history_facts::reachable(&read, &left, d.l1).unwrap());
    assert!(!history_facts::reachable(&read, &left, d.l_right).unwrap());
    let both = [landing_of("v1.1.0"), landing_of("v2.0.0")];
    assert!(history_facts::reachable(&read, &both, d.l_right).unwrap());
    assert!(history_facts::reachable(&read, &both, d.l1).unwrap());
    assert!(!history_facts::reachable(&read, &both, hs(9)).unwrap());
    assert!(!history_facts::reachable(&read, &[], d.l1).unwrap());
    let e = history_facts::not_reachable(d.l_right);
    assert_eq!(e.code, "E_NOT_FOUND");
}

#[test]
fn ha10_holder_canon_is_empty() {
    let d = divergent();
    let h = d.holder();
    let read = h.read();
    // a holder has no canon: its chain has no head, and its tags are what it holds
    assert!(read.canon_head().is_none());
    assert_eq!(h.store().tags().unwrap().len(), 3);
    // the project it was copied from does have one
    assert!(d.left.read().canon_head().is_some());
}

#[test]
fn ha10_a_holder_exports_the_history_of_one_captured_tag() {
    let d = divergent();
    let h = d.holder();
    let read = h.read();
    let (_, tag_hash, tag_rec) = h
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.1.0")
        .unwrap();
    let mut bytes = Vec::new();
    let report = rdy_engine::stream_export::write(
        &read,
        tag_hash,
        &std::collections::BTreeSet::new(),
        &mut bytes,
    )
    .unwrap();
    assert_eq!(
        rdy_store::sync_stream::check_batch(&bytes[..]).unwrap(),
        report
    );
    // the batch holds the tag, its landing and the first landing, and not the other branch
    let has = |kind, hash| report.keys.contains(&ObjectKey { kind, hash });
    assert!(has(kinds::TAG, tag_hash));
    assert!(has(kinds::LANDING, tag_rec.landing));
    assert!(has(kinds::LANDING, d.l1));
    assert!(!has(kinds::LANDING, d.l_right));
    assert_eq!(report.header.repo_id, RepoId([1; 16]));
}

/// A stored landing that follows `prev`, distinct by `n`; it reads back as a record only.
fn link(n: u32, prev: Option<Hash>) -> LandingRecord {
    let mut proposal = [0u8; 32];
    proposal[..4].copy_from_slice(&n.to_le_bytes());
    LandingRecord {
        proposal: Hash(proposal),
        prev,
        state: hs(2),
        approved_by: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 1,
        },
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: None,
        rebased_from: vec![],
        evidence: None,
        assessment: None,
        policy: None,
        read_identity: None,
    }
}

#[test]
fn ha10_history_depth_is_bounded() {
    assert_eq!(MAX_LANDINGS, 100_000);
    // a chain of 100001 stored landings, newest last
    let dir = tempfile::tempdir().unwrap();
    let mut h = Rdy::create_holder(dir.path(), RepoId([1; 16])).unwrap();
    let mut ids = Vec::with_capacity(MAX_LANDINGS + 1);
    let mut prev = None;
    for n in 0..=MAX_LANDINGS as u32 {
        let id = h.store_mut().put(kinds::LANDING, &link(n, prev)).unwrap();
        ids.push(id);
        prev = Some(id);
    }
    h.seal_open().unwrap();
    h.close().unwrap();
    let h = Rdy::open_holder(dir.path()).unwrap();
    let read = h.read();
    let newest = ids[MAX_LANDINGS];
    let second = ids[MAX_LANDINGS - 1];
    let unknown = hs(9);
    // 100000 landings (from the second newest) are walked to their end; 100001 are over the cap
    assert!(!history_facts::reachable(&read, &[second], unknown).unwrap());
    let e = history_facts::reachable(&read, &[newest], unknown).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "history exceeds 100000 landings",
            "request a shorter history"
        )
    );
    let e = history_facts::chain(&read, newest).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "history exceeds 100000 landings")
    );
    // the oldest landings are reachable from a head within the cap
    assert!(history_facts::reachable(&read, &[second], ids[0]).unwrap());
    // a short suffix of the chain is a short history
    let c = history_facts::chain(&read, ids[2]).unwrap();
    assert_eq!(hashes(&c), [ids[2], ids[1], ids[0]]);
}

/// One request-wide budget: the distinct landings met across all heads, and a matched target,
/// count against [`MAX_LANDINGS`] together.
/// A holder at `dir` with two disjoint histories of [`MAX_LANDINGS`] / 2 + 1 (50001) landings
/// each, oldest first.
fn two_histories(dir: &Path) -> (Rdy, Vec<Hash>, Vec<Hash>) {
    let mut h = Rdy::create_holder(dir, RepoId([1; 16])).unwrap();
    let half = MAX_LANDINGS / 2 + 1;
    let mut chain = |base: u32| {
        let mut ids = Vec::with_capacity(half);
        let mut prev = None;
        for n in 0..half as u32 {
            let id = h
                .store_mut()
                .put(kinds::LANDING, &link(base + n, prev))
                .unwrap();
            ids.push(id);
            prev = Some(id);
        }
        ids
    };
    let a = chain(0);
    let b = chain(1_000_000);
    h.seal_open().unwrap();
    h.close().unwrap();
    (Rdy::open_holder(dir).unwrap(), a, b)
}

#[test]
fn ha10_history_budget_is_one_for_all_heads() {
    let dir = tempfile::tempdir().unwrap();
    let half = MAX_LANDINGS / 2 + 1; // 50001
    // two disjoint histories of 50001 landings each, oldest first
    let (h, a, b) = two_histories(dir.path());
    let read = h.read();
    let unknown = hs(9);
    let (a_head, a_second) = (a[half - 1], a[half - 2]);
    let (b_head, b_second) = (b[half - 1], b[half - 2]);
    let over = |r: Result<bool, RdyError>| {
        let e = r.unwrap_err();
        (e.code, e.message)
    };
    let limit = (E_LIMIT, "history exceeds 100000 landings".to_string());
    // 50001 + 50001 distinct landings are over the one budget, though each head alone is not
    assert!(!history_facts::reachable(&read, &[a_head], unknown).unwrap());
    assert!(!history_facts::reachable(&read, &[b_head], unknown).unwrap());
    assert_eq!(
        over(history_facts::reachable(&read, &[b_head, a_head], unknown)),
        limit
    );
    // 50000 + 50000 is exactly the budget: walked to the end, and the oldest is reached
    assert!(!history_facts::reachable(&read, &[b_second, a_second], unknown).unwrap());
    assert!(history_facts::reachable(&read, &[b_second, a_second], a[0]).unwrap());
    // a target that is the 100001st landing met is over the budget, not a match
    assert_eq!(
        over(history_facts::reachable(&read, &[b_second, a_head], a[0])),
        limit
    );
    // landings both heads share are met once
    assert!(!history_facts::reachable(&read, &[a_head, a_second], unknown).unwrap());
    assert!(!history_facts::reachable(&read, &[a_second, a_head, a_second], unknown).unwrap());
}

// ---------------------------------------------------------------------------------------------
// signatures

fn put(r: &mut Rdy, g: &Signature) -> Hash {
    let id = r.store_mut().put_signature(g).unwrap();
    r.seal_open().unwrap();
    id
}

fn sig(subject: Hash, ns: &str, stmt: Vec<u8>, at_ms: i64) -> Signature {
    Signature {
        subject,
        namespace: ns.into(),
        statement: stmt,
        sig: b"not checked here".to_vec(),
        signed_by: Provenance {
            actor: "agent:s".into(),
            run: None,
            role: Role::Approver,
            at_ms,
        },
    }
}

#[test]
fn ha10_signature_facts_match_sig_without_trust_policy() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path());
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    );
    let l = land(&mut r, "first");
    let t = tag(&mut r, "v1.0.0");
    let repo = r.read().repo_id().unwrap();
    let rec = r.store().reader().landing(&l).unwrap();
    let tag_rec = r.store().reader().read_tag(&t).unwrap();

    let approval = statement(StatementSource::Approval(&rec), Some(repo), FP).unwrap();
    let tag_stmt = statement(
        StatementSource::Tag {
            hash: t,
            tag: &tag_rec,
        },
        Some(repo),
        FP,
    )
    .unwrap();
    let before = r.read();
    let a = put(&mut r, &sig(l, "rdy-approval-v1", approval, 1));
    // statement bytes that do not parse have no kind, and are still facts
    let junk = put(&mut r, &sig(l, "rdy-approval-v1", b"x".to_vec(), 2));
    let tg = put(&mut r, &sig(t, "rdy-tag-v1", tag_stmt, 3));

    // the state read before the signatures was published holds none of them
    assert!(
        history_facts::landing(&before, l)
            .unwrap()
            .signatures
            .is_empty()
    );

    let read = r.read();
    let facts = history_facts::landing(&read, l).unwrap();
    let mut want = [(a, Some("approval")), (junk, None)];
    want.sort_by_key(|w| w.0);
    let got: Vec<(Hash, Option<&str>)> = facts
        .signatures
        .iter()
        .map(|s| (s.hash, s.kind.as_deref()))
        .collect();
    assert_eq!(got, want, "signature hash order, kind from the statement");
    for s in &facts.signatures {
        assert_eq!(s.record, r.store().reader().signature(&s.hash).unwrap());
    }
    // the tag's own signatures, apart from the landing's
    let tag_facts = history_facts::signatures(&read, t).unwrap();
    assert_eq!(tag_facts.len(), 1);
    assert_eq!(tag_facts[0].hash, tg);
    assert_eq!(tag_facts[0].kind.as_deref(), Some("tag"));
    assert!(history_facts::signatures(&read, hs(9)).unwrap().is_empty());

    // 4096 signatures of one subject are read; one more is the signing limit
    let more: Vec<Signature> = (10..10 + 4094)
        .map(|i| sig(l, "rdy-approval-v1", b"x".to_vec(), i))
        .collect();
    r.store_mut()
        .write(rdy_store::signature::frames(&more).unwrap())
        .unwrap();
    r.seal_open().unwrap();
    assert_eq!(
        history_facts::landing(&r.read(), l)
            .unwrap()
            .signatures
            .len(),
        4096
    );
    put(&mut r, &sig(l, "rdy-approval-v1", b"x".to_vec(), 9000));
    let e = history_facts::landing(&r.read(), l).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "signing input exceeds 4096 signatures per subject",
            "reduce the input to the stated limit"
        )
    );
    // the chain and the tag still answer for their other subjects
    assert_eq!(history_facts::signatures(&r.read(), t).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// presence

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

#[test]
fn ha10_presence_is_exact_sorted_and_bounded() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path());
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    );
    let l = land(&mut r, "first");
    // an object nothing refers to still counts
    let orphan = Block {
        id: BlockId::parse("d-00009").unwrap(),
        block_type: "doc".into(),
        parent: None,
        order: "a0".into(),
        fields: Default::default(),
        body: "nobody points here".into(),
        links: Default::default(),
        pins: Default::default(),
        read_identity: None,
    };
    let orphan_hash = r.store_mut().put(kinds::BLOCK, &orphan).unwrap();
    r.seal_open().unwrap();
    let read = r.read();

    let land_key = key(kinds::LANDING, l);
    let orphan_key = key(kinds::BLOCK, orphan_hash);
    let absent = key(kinds::BLOCK, hs(9));
    // unsorted, repeated and mixed in; the answer is sorted by kind then hash, once each
    let got = history_facts::presence(&read, &[orphan_key, absent, land_key, land_key]).unwrap();
    let mut keys = vec![land_key, orphan_key, absent];
    keys.sort();
    assert_eq!(got.iter().map(|p| p.key).collect::<Vec<_>>(), keys);
    for p in &got {
        assert_eq!(p.present, p.key != absent, "{}", p.key);
        assert!(!p.redacted);
    }
    // the same hash under another kind is a different key
    let other = history_facts::presence(
        &read,
        &[
            key(kinds::TAG, l),
            key(kinds::CHANGE, l),
            key(kinds::BLOCK, l),
        ],
    )
    .unwrap();
    assert!(other.iter().all(|p| !p.present));
    assert!(history_facts::presence(&read, &[]).unwrap().is_empty());

    // every sync kind may be asked; a local-only or unassigned one may not
    for kind in [
        kinds::REF_UPDATE,
        kinds::BATCH_COMMIT,
        kinds::TRANSITION,
        12,
        0,
        200,
    ] {
        let e = history_facts::presence(&read, &[land_key, key(kind, l)]).unwrap_err();
        assert_eq!(
            (e.code, e.message.clone(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                format!("object presence kind {kind} is not portable"),
                "request stored object kinds only"
            )
        );
    }
    // a local record's hash under a portable kind is absent: ref updates and markers are no object
    let mut local = vec![];
    r.store()
        .for_each_frame_from(ORIGIN, r.store().log_end(), |_, kind, hash, _, _| {
            if [kinds::REF_UPDATE, kinds::BATCH_COMMIT, kinds::TRANSITION].contains(&kind) {
                local.push(hash);
            }
            Ok(())
        })
        .unwrap();
    assert!(!local.is_empty());
    for hash in local {
        for kind in rdy_store::sync_stream::SYNC_KINDS {
            let p = history_facts::presence(&read, &[key(kind, hash)]).unwrap();
            assert!(!p[0].present, "{kind}:{hash}");
        }
    }

    // the cap on keys: N is read, N + 1 is refused
    let many: Vec<ObjectKey> = (0..MAX_KEYS as u32)
        .map(|i| {
            let mut h = [0u8; 32];
            h[..4].copy_from_slice(&i.to_le_bytes());
            key(kinds::BLOCK, Hash(h))
        })
        .collect();
    assert_eq!(
        history_facts::presence(&read, &many).unwrap().len(),
        MAX_KEYS
    );
    let mut over = many;
    over.push(land_key);
    let e = history_facts::presence(&read, &over).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "object presence query exceeds 100000 keys",
            "request at most 100000 object keys"
        )
    );
}

// ---------------------------------------------------------------------------------------------
// landing facts by hash, in one batch (sdc2)

/// An error as its code, message and fix.
fn parts(e: RdyError) -> (String, String, String) {
    (e.code.to_string(), e.message, e.fix)
}

/// The landings the holder's tags name, in tag-name order.
fn tag_heads(h: &Rdy) -> Vec<Hash> {
    let mut tags = h.store().tags().unwrap();
    tags.sort_by(|a, b| a.0.cmp(&b.0));
    tags.into_iter().map(|(_, _, t)| t.landing).collect()
}

#[test]
fn sdc2_batch_reaches_divergent_tags_without_canon() {
    let d = divergent();
    let mut h = d.holder();
    // a stored landing that no captured history reaches
    let orphan = h
        .store_mut()
        .put(kinds::LANDING, &link(7, Some(d.l1)))
        .unwrap();
    h.seal_open().unwrap();
    let read = h.read();
    assert!(read.canon_head().is_none());
    assert!(
        read.reader().landing(&orphan).is_ok(),
        "the orphan is stored"
    );
    let heads = tag_heads(&h);
    assert_eq!(heads.len(), 3);

    // both divergent histories and their shared first landing, in first-occurrence order
    let got =
        history_facts::landings(&read, &heads, &[d.l_right, d.l_left, d.l1, d.l_right]).unwrap();
    assert_eq!(hashes(&got), [d.l_right, d.l_left, d.l1]);
    for f in &got {
        assert_eq!(f, &history_facts::landing(&read, f.hash).unwrap());
    }
    // one tag's history does not reach the other branch
    let right = [heads[2]];
    assert_eq!(
        hashes(&history_facts::landings(&read, &right, &[d.l1]).unwrap()),
        [d.l1]
    );
    assert_eq!(
        parts(history_facts::landings(&read, &right, &[d.l_right, d.l_left]).unwrap_err()),
        parts(history_facts::not_reachable(d.l_left))
    );
    // the unreferenced landing fails the whole call
    assert_eq!(
        parts(history_facts::landings(&read, &heads, &[d.l1, orphan, d.l_left]).unwrap_err()),
        parts(history_facts::not_reachable(orphan))
    );
}

#[test]
fn sdc2_batch_counts_inputs_before_dedup() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path());
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]),
    );
    let l = land(&mut r, "first");
    let read = r.read();
    let heads = [l];
    // 100000 copies of one reachable hash are one fact
    let at = vec![l; MAX_KEYS];
    assert_eq!(
        history_facts::landings(&read, &heads, &at).unwrap(),
        [history_facts::landing(&read, l).unwrap()]
    );
    // 100001 copies are over the limit, counted before the repeats collapse
    let limit = (
        E_LIMIT.to_string(),
        "landing facts query exceeds 100000 hashes".to_string(),
        "request at most 100000 landing hashes".to_string(),
    );
    let over = vec![l; MAX_KEYS + 1];
    assert_eq!(
        parts(history_facts::landings(&read, &heads, &over).unwrap_err()),
        limit
    );
    // the count comes before any walk: a head that is no stored landing is never read
    assert_eq!(
        parts(history_facts::landings(&read, &[hs(9)], &over).unwrap_err()),
        limit
    );
    assert_eq!(
        history_facts::landings(&read, &[hs(9)], &[l])
            .unwrap_err()
            .message,
        format!("object {} not found", hs(9).short())
    );
    // an empty request is empty, and a stale state answers nothing
    assert!(
        history_facts::landings(&read, &heads, &[])
            .unwrap()
            .is_empty()
    );
    let stale = r.read();
    remove(&mut r, w.created["$p"].clone());
    let e = history_facts::landings(&stale, &heads, &[]).unwrap_err();
    assert_eq!(
        (e.code, e.fix.as_str()),
        (E_REDACTED_STATE, REDACTED_STATE_FIX)
    );
    assert!(
        history_facts::landings(&r.read(), &heads, &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn sdc2_batch_walk_has_one_distinct_landing_budget() {
    let dir = tempfile::tempdir().unwrap();
    let half = MAX_LANDINGS / 2 + 1; // 50001
    let (h, a, b) = two_histories(dir.path());
    let read = h.read();
    let (a_head, a_second) = (a[half - 1], a[half - 2]);
    let (b_head, b_second) = (b[half - 1], b[half - 2]);
    let over = |r: Result<Vec<history_facts::LandingFacts>, RdyError>| {
        let e = r.unwrap_err();
        (e.code, e.message)
    };
    let limit = (E_LIMIT, "history exceeds 100000 landings".to_string());
    // 50001 + 50001 distinct landings are over the one budget
    assert_eq!(
        over(history_facts::landings(&read, &[b_head, a_head], &[a[0]])),
        limit
    );
    // 50000 + 50000 is exactly the budget: both oldest landings are found, in input order
    let got = history_facts::landings(&read, &[b_second, a_second], &[a[0], b[0], a[0]]).unwrap();
    assert_eq!(hashes(&got), [a[0], b[0]]);
    // a requested hash that is the 100001st distinct visit is over the budget, not a match
    assert_eq!(
        over(history_facts::landings(&read, &[b_second, a_head], &[a[0]])),
        limit
    );
    // shared ancestors count once: 50000 + 1 (a_head, whose past was met) + 49999 = 100000
    let got =
        history_facts::landings(&read, &[a_second, a_head, b[half - 3]], &[b[0], a_head]).unwrap();
    assert_eq!(hashes(&got), [b[0], a_head]);
    // the walk stops once every requested hash is found: two visits here
    let got = history_facts::landings(&read, &[b_head, a_head], &[b_second, b_head]).unwrap();
    assert_eq!(hashes(&got), [b_second, b_head]);
    // a hash still missing keeps the walk going to the budget
    assert_eq!(
        over(history_facts::landings(
            &read,
            &[b_head, a_head],
            &[b_head, hs(9)]
        )),
        limit
    );
}

#[test]
fn sdc2_batch_uses_captured_signatures_and_removal_facts() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path());
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
        ]),
    );
    let (d, secret) = (w.created["$d"].clone(), w.created["$p"].clone());
    let l1 = land(&mut r, "first");
    write(
        &mut r,
        json!([{"op": "update", "id": d, "fields": {"title": "A2"}}]),
    );
    let l2 = land(&mut r, "second");
    let before = r.read();
    let heads = [before.canon_head().unwrap()];
    let g = put(&mut r, &sig(l1, "rdy-approval-v1", b"x".to_vec(), 1));

    // the state read before the signature holds none; each fact equals the single read
    let old = history_facts::landings(&before, &heads, &[l2, l1]).unwrap();
    assert_eq!(hashes(&old), [l2, l1]);
    for f in &old {
        assert!(f.signatures.is_empty());
        assert_eq!(f, &history_facts::landing(&before, f.hash).unwrap());
    }
    let now = r.read();
    let cur = history_facts::landings(&now, &heads, &[l1, l2]).unwrap();
    assert_eq!(
        cur[0].signatures.iter().map(|s| s.hash).collect::<Vec<_>>(),
        [g]
    );
    for f in &cur {
        assert_eq!(f, &history_facts::landing(&now, f.hash).unwrap());
    }

    // after a removal, states read before it fail
    remove(&mut r, secret.clone());
    for st in [&before, &now] {
        let e = history_facts::landings(st, &heads, &[l1]).unwrap_err();
        assert_eq!(
            (e.code, e.fix.as_str()),
            (E_REDACTED_STATE, REDACTED_STATE_FIX)
        );
    }
    // a current state reads the removal projection: the same identities, flagged as removed
    let after = r.read();
    let got = history_facts::landings(&after, &[after.canon_head().unwrap()], &[l1, l2]).unwrap();
    for f in &got {
        assert_eq!(f, &history_facts::landing(&after, f.hash).unwrap());
    }
    assert_eq!(got[0].record, cur[0].record);
    assert_eq!(got[0].signatures, cur[0].signatures);
    assert_eq!(got[0].blocks.len(), cur[0].blocks.len());
    for (a, b) in got[0].blocks.iter().zip(&cur[0].blocks) {
        assert_eq!(
            (&a.change, &a.id, a.op, a.base, a.version),
            (&b.change, &b.id, b.op, b.base, b.version)
        );
        assert_eq!(a.redacted, a.id.to_string() == secret);
        assert!(!b.redacted);
    }
    assert!(got[0].blocks.iter().any(|b| b.redacted));
    assert_eq!(got[1], cur[1]);
}
