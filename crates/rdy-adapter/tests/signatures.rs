//! sig6, second merge (D-1Z951, D-17AA8; O11 as amended by ruling task-13): the signature check
//! through the adapter. The check is SIG's pure `check_signature`; the adapter re-exports it and
//! reads a signature's context from a captured snapshot with SIG's `read_check_context`. So every
//! report here is compared, byte for byte as JSON, with the one core gives for the same input.
//! No environment, agent or allowed-signers file is read: trust is the caller's parsed input.
//!
//! The vectors are fixed public bytes, not signed here: the plain and security-key vectors of
//! SIG's signature perf ops (`rdy_qa::signing`: the plain SSHSIG made by OpenSSH over the
//! statement as written, the security-key one by SIG's standalone simulator).

use rdy_adapter::signatures::{
    ActorRun, AllowedSigners, CheckContext, KEY_TYPES, RepoId, Signature, SignatureCheck,
    SignatureSubject, SigningWarning, StatementKind, check_signature,
};
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check as core;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export, stream_take};
use rdy_model::{Assurance, LandingRecord, Provenance, Role};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

// ---- the frozen public vectors (as `rdy_qa::signing` holds them) ----

// Re-taken for M3a's LANDING encoding 1 (D-1HDVA); the old subject began b622dd3d9205.
const VECTOR_SUBJECT: [u8; 32] = [
    0x55, 0x7e, 0x2e, 0x89, 0x56, 0x3f, 0x70, 0x8e, 0xcb, 0xd8, 0x1a, 0x15, 0xbc, 0x2b, 0x88, 0xc1,
    0x99, 0xc7, 0xad, 0x39, 0x53, 0x3d, 0x5c, 0x90, 0xa1, 0x2c, 0x86, 0xc0, 0x0c, 0x16, 0xd1, 0x42,
];
const VECTOR_REPO: RepoId = RepoId([0x14; 16]);
const PLAIN_STATEMENT: &str = "rdy-statement: 1\nkind: approval\nrepo: 14141414141414141414141414141414\nproposal_version: 0303030303030303030303030303030303030303030303030303030303030303\nbase: none\nactor: human:qa\nassurance: local\nkey: SHA256:8nEdi2dFYm+E9JRNIKxJNA5RGKdcAaVkhr4NVgAnE/4\n";
const PLAIN_SIG: &str = "53534853494700000001000000330000000b7373682d6564323535313900000020a81bd5afaf10098a7606e43cd55ef8f253e2ae1088d77937c74e0df78ae7b8820000000f7264792d617070726f76616c2d76310000000000000006736861353132000000530000000b7373682d6564323535313900000040e00f585fdac44777b16afb623767f267a85d3ac92456d989759eda0401fe044fea662b05faaf8722814f5d49f8197e4ef0c93079f48f1d6bab7d27c5c599cc01";
const PLAIN_KEY_LINE: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKgb1a+vEAmKdgbkPNVe+PJT4q4QiNd5N8dODfeK57iC";
const SK_STATEMENT: &str = "rdy-statement: 1\nkind: approval\nrepo: 14141414141414141414141414141414\nproposal_version: 0303030303030303030303030303030303030303030303030303030303030303\nbase: none\nactor: human:qa\nassurance: local\nkey: SHA256:2V6EF6XZaGa6R+yy2uTYpfxOTHPP4+nkupDfZcK0hx8\n";
const SK_SIG: &str = "535348534947000000010000004a0000001a736b2d7373682d65643235353139406f70656e7373682e636f6d000000206b734a8eff246fe734b38d4046c148eee5f04fe87b3a0a423955a77956de066b000000047373683a0000000f7264792d617070726f76616c2d76310000000000000006736861353132000000670000001a736b2d7373682d65643235353139406f70656e7373682e636f6d000000404b49af80beaf8a68f32f81972926c44110ecd12739ef5e76268d3966b639e494d74d2ffa2c741d78ad07aa2674279c7b342b480db8b63fa7d25fa94f5527b80c0100000007";
const SK_KEY_LINE: &str = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIGtzSo7/JG/nNLONQEbBSO7l8E/oezoKQjlVp3lW3gZrAAAABHNzaDo=";

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn qa(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:qa".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
}

/// The landing the vectors sign: a person's local approval with fixed hashes.
fn vector_landing() -> LandingRecord {
    LandingRecord {
        proposal: Hash([1; 32]),
        prev: None,
        state: Hash([2; 32]),
        approved_by: qa(1),
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None::<ActorRun>,
        relay_evidence: None,
        approved_version: Some(Hash([3; 32])),
        rebased_from: vec![],
        evidence: None,
        assessment: None,
        policy: None,
        read_identity: None,
    }
}

fn trust(line: &str) -> AllowedSigners {
    AllowedSigners::parse(format!("human:qa {line}\n").as_bytes()).unwrap()
}

/// The plain (`sk` false) or security-key vector: its record and the context it covers.
fn vector(sk: bool) -> (Signature, CheckContext) {
    let (statement, sig) = if sk {
        (SK_STATEMENT, SK_SIG)
    } else {
        (PLAIN_STATEMENT, PLAIN_SIG)
    };
    let hash = Hash(VECTOR_SUBJECT);
    (
        Signature {
            subject: hash,
            namespace: "rdy-approval-v1".into(),
            statement: statement.as_bytes().to_vec(),
            sig: unhex(sig),
            signed_by: qa(1),
        },
        CheckContext {
            subject: SignatureSubject::Landing {
                hash,
                record: vector_landing(),
            },
            current_repo: VECTOR_REPO,
            source_repos: vec![],
            holder: false,
        },
    )
}

fn json_bytes(c: &SignatureCheck) -> Vec<u8> {
    serde_json::to_vec(c).unwrap()
}

/// The verdict fields of a report: signed, statement, trust, presence, code.
fn verdict(c: &SignatureCheck) -> (&str, &str, &str, &str, Option<&str>) {
    (
        c.signed.as_str(),
        c.statement.as_str(),
        c.trust.as_str(),
        c.presence.as_str(),
        c.code.as_deref(),
    )
}

// ---- stored subjects (core) ----

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

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "sig6".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) -> Hash {
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

fn tag_hash(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

/// A closed project with one landing whose tag `v1.0.0` an agent relayed for the founder.
/// Returns the project's repo, the landing, the tag and the tag's relay answer.
fn relayed_project(dir: &Path) -> (RepoId, Hash, Hash, Hash) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    );
    let landing = land(&mut r);
    let agent = Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap();
    let relay = RelayInput {
        evidence: Some("Yes, tag it.".into()),
        ..RelayInput::default()
    };
    r.tag_as(&agent, "human:founder", &relay, true, "v1.0.0", None, None)
        .unwrap();
    let t = tag_hash(&r, "v1.0.0");
    let (answer, _) = r.store().answer_of("tag:v1.0.0").unwrap().unwrap();
    let repo = r.store().repo_id().unwrap().unwrap();
    r.close().unwrap();
    (repo, landing, t, answer)
}

/// One signature to check and the caller's trust input for it.
type Case<'a> = (Signature, Option<&'a AllowedSigners>);
/// The context and report of each case, in case order.
type Checked = Vec<(CheckContext, SignatureCheck)>;

/// The adapter's contexts and reports for `cases` in the store at `root`, through a snapshot,
/// then core's for the same cases in the same store, opened by core once the handle is closed.
fn both(root: &Path, holder: bool, cases: &[Case<'_>]) -> (Checked, Checked) {
    let handle = if holder {
        StoreHandle::open_holder(root).unwrap()
    } else {
        StoreHandle::open(root).unwrap()
    };
    let snap = handle.snapshot().unwrap();
    let adapter = cases
        .iter()
        .map(|(g, t)| {
            let c = snap.check_context(g).unwrap();
            let report = check_signature(g, &c, *t);
            (c, report)
        })
        .collect();
    drop(snap);
    handle.close().unwrap();
    let r = if holder {
        Rdy::open_holder(root).unwrap()
    } else {
        Rdy::open(root).unwrap()
    };
    let state = r.read();
    let core = cases
        .iter()
        .map(|(g, t)| {
            let c = core::read_check_context(&state, g).unwrap();
            let report = core::check_signature(g, &c, *t);
            (c, report)
        })
        .collect();
    drop(state);
    r.close().unwrap();
    (adapter, core)
}

#[test]
fn sig6_adapter_and_core_return_identical_reports() {
    // the re-exported surface, by type
    let _: &[&str; 2] = &KEY_TYPES;
    assert_eq!(KEY_TYPES, ["ssh-ed25519", "sk-ssh-ed25519@openssh.com"]);

    let (plain, plain_ctx) = vector(false);
    let (sk, sk_ctx) = vector(true);
    let (plain_trust, sk_trust) = (trust(PLAIN_KEY_LINE), trust(SK_KEY_LINE));
    let mut bad = plain.clone();
    let last = bad.sig.len() - 1;
    bad.sig[last] ^= 0x01;
    let mut wrong_ctx = plain_ctx.clone();
    if let SignatureSubject::Landing { record, .. } = &mut wrong_ctx.subject {
        record.prev = Some(Hash([9; 32]));
    }

    // the pure check, vector by vector
    let pure: [(&str, &Signature, &CheckContext, Option<&AllowedSigners>); 6] = [
        ("valid", &plain, &plain_ctx, Some(&plain_trust)),
        ("bad signature", &bad, &plain_ctx, Some(&plain_trust)),
        ("wrong subject", &plain, &wrong_ctx, Some(&plain_trust)),
        ("unknown key", &plain, &plain_ctx, Some(&sk_trust)),
        ("no trust input", &plain, &plain_ctx, None),
        ("user presence", &sk, &sk_ctx, Some(&sk_trust)),
    ];
    let mut reports = Vec::new();
    for (name, g, c, t) in pure {
        let a = check_signature(g, c, t);
        let want = core::check_signature(g, c, t);
        assert_eq!(json_bytes(&a), json_bytes(&want), "{name}");
        reports.push(a);
    }
    let verdicts: Vec<_> = reports.iter().map(verdict).collect();
    assert_eq!(
        verdicts,
        [
            ("valid", "match", "trusted", "absent", None),
            (
                "invalid",
                "mismatch",
                "unknown",
                "absent",
                Some("E_SIG_INVALID")
            ),
            (
                "valid",
                "mismatch",
                "unknown",
                "absent",
                Some("E_SIG_OBJECT_MISMATCH")
            ),
            ("valid", "match", "unknown", "absent", None),
            ("valid", "match", "unknown", "absent", None),
            ("valid", "match", "trusted", "present", None),
        ]
    );
    let warned: &[SigningWarning] = &reports[3].warnings;
    assert_eq!(warned.len(), 1);
    assert_eq!(warned[0].code, "W_SIG_UNKNOWN_KEY");
    assert_eq!(
        reports[0].kind.as_deref(),
        Some(StatementKind::Approval.as_str())
    );
    assert_eq!(
        reports[5].key_type.as_deref(),
        Some("sk-ssh-ed25519@openssh.com")
    );

    // contexts read through a snapshot are core's, in a project and in a holder: a landing, a
    // relayed tag (its answer found by the project's `answer/tag:<name>`, and by the holder's
    // `answer/tag-object:<hash>`) and a missing subject
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let (repo, landing, t, answer) = relayed_project(&src);
    let r = Rdy::open(&src).unwrap();
    let mut batch = Vec::new();
    stream_export::write(&r.read(), t, &BTreeSet::new(), &mut batch).unwrap();
    r.close().unwrap();
    let root = dir.path().join("holder");
    let path = dir.path().join("up.rdysync");
    std::fs::write(&path, &batch).unwrap();
    let mut h = StoreHandle::create_holder(&root, repo).unwrap();
    h.take(
        &[std::fs::File::open(&path).unwrap()],
        rdy_adapter::take::UploadTarget {
            tag: "v1.0.0".into(),
            landing,
        },
    )
    .unwrap();
    h.close().unwrap();

    let missing = Hash([0xee; 32]);
    let mut cases: Vec<Case<'_>> = Vec::new();
    for subject in [landing, t, missing] {
        for (g, trust) in [
            (&plain, Some(&plain_trust)),
            (&bad, Some(&plain_trust)),
            (&plain, Some(&sk_trust)),
            (&plain, None),
            (&sk, Some(&sk_trust)),
        ] {
            let mut g = g.clone();
            g.subject = subject;
            cases.push((g, trust));
        }
    }
    for (store, holder) in [(&src, false), (&root, true)] {
        let (adapter, core) = both(store, holder, &cases);
        assert_eq!(adapter.len(), cases.len());
        for (i, ((ca, ra), (cc, rc))) in adapter.iter().zip(&core).enumerate() {
            assert_eq!(ca, cc, "holder {holder}, case {i}");
            assert_eq!(json_bytes(ra), json_bytes(rc), "holder {holder}, case {i}");
            assert_eq!(ca.holder, holder);
            assert_eq!(ca.current_repo, repo);
            match &ca.subject {
                SignatureSubject::Landing { hash, .. } => assert_eq!(*hash, landing),
                SignatureSubject::Tag {
                    hash,
                    answer: Some((a, rec)),
                    ..
                } => {
                    assert_eq!((*hash, *a), (t, answer));
                    assert_eq!(rec.targets, vec![t]);
                }
                SignatureSubject::Missing { hash } => assert_eq!(*hash, missing),
                other => panic!("holder {holder}, case {i}: {other:?}"),
            }
        }
        // a vector signed for another landing never matches a stored subject
        assert!(adapter.iter().all(|(_, r)| r.statement != "match"));
    }
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = session(Role::Writer);
    r.redact(
        &s,
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p = r.propose(&s, "work", "Remove", None).unwrap();
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

#[test]
fn ha16_check_context_refuses_a_stale_snapshot() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut r = Rdy::init(&src, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "A secret."}
        ]),
    );
    let l1 = land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let v1 = tag_hash(&r, "v1.0.0");
    // v1.0.0 is exported before the removal, which an export made after it would carry
    let mut first = Vec::new();
    stream_export::write(&r.read(), v1, &BTreeSet::new(), &mut first).unwrap();
    let p1 = dir.path().join("v1.rdysync");
    std::fs::write(&p1, &first).unwrap();
    remove(&mut r, &w.created["$p"]);
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    let v2 = tag_hash(&r, "v2.0.0");
    let repo = r.store().repo_id().unwrap().unwrap();
    let mut holder = Rdy::create_holder(&dir.path().join("holder"), repo).unwrap();
    stream_take::take(
        &mut holder,
        &[std::fs::File::open(&p1).unwrap()],
        rdy_adapter::take::UploadTarget {
            tag: "v1.0.0".into(),
            landing: l1,
        },
    )
    .unwrap();
    let mut second = Vec::new();
    stream_export::write(&r.read(), v2, &BTreeSet::new(), &mut second).unwrap();
    let p2 = dir.path().join("v2.rdysync");
    std::fs::write(&p2, &second).unwrap();
    let l2 = r.store().read_tag(&v2).unwrap().landing;
    holder.close().unwrap();
    r.close().unwrap();

    let mut handle = StoreHandle::open_holder(&dir.path().join("holder")).unwrap();
    let before = handle.snapshot().unwrap();
    let (g, _) = vector(false);
    let g = Signature { subject: l1, ..g };
    let c = before.check_context(&g).unwrap();
    assert!(matches!(c.subject, SignatureSubject::Landing { hash, .. } if hash == l1));
    // a take that imports a removal makes the older snapshot stale for content reads
    handle
        .take(
            &[std::fs::File::open(&p2).unwrap()],
            rdy_adapter::take::UploadTarget {
                tag: "v2.0.0".into(),
                landing: l2,
            },
        )
        .unwrap();
    let e = before.check_context(&g).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    assert_eq!(handle.snapshot().unwrap().check_context(&g).unwrap(), c);
    drop(before);
    handle.close().unwrap();
}

/// Fix round 1: a snapshot's `check_context` reads the subject and its relay answer as the log
/// stood when the snapshot was taken. A later take adds a tag and its answer without a removal,
/// so the epoch guard alone does not hide them: the older snapshot still finds the subject
/// missing, and a new snapshot finds the tag and its answer.
#[test]
fn ha16_check_context_keeps_the_snapshot_log_end() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let (repo, landing, t, answer) = relayed_project(&src);
    let r = Rdy::open(&src).unwrap();
    let mut batch = Vec::new();
    stream_export::write(&r.read(), t, &BTreeSet::new(), &mut batch).unwrap();
    r.close().unwrap();
    let path = dir.path().join("up.rdysync");
    std::fs::write(&path, &batch).unwrap();

    let mut handle = StoreHandle::create_holder(&dir.path().join("holder"), repo).unwrap();
    let before = handle.snapshot().unwrap();
    let (g, _) = vector(false);
    let tag_sig = Signature { subject: t, ..g };
    let landing_sig = Signature {
        subject: landing,
        ..tag_sig.clone()
    };
    let missing = |c: &CheckContext, want: Hash| {
        assert!(
            matches!(c.subject, SignatureSubject::Missing { hash } if hash == want),
            "{:?}",
            c.subject
        );
    };
    missing(&before.check_context(&tag_sig).unwrap(), t);
    missing(&before.check_context(&landing_sig).unwrap(), landing);

    handle
        .take(
            &[std::fs::File::open(&path).unwrap()],
            rdy_adapter::take::UploadTarget {
                tag: "v1.0.0".into(),
                landing,
            },
        )
        .unwrap();

    // the older snapshot: no tag, no landing, as its other reads say
    assert!(before.tags().unwrap().is_empty());
    let old = before.check_context(&tag_sig).unwrap();
    missing(&old, t);
    missing(&before.check_context(&landing_sig).unwrap(), landing);
    let old_report = check_signature(&tag_sig, &old, None);

    // a new snapshot: the tag with its relay answer, and the landing
    let after = handle.snapshot().unwrap();
    match after.check_context(&tag_sig).unwrap().subject {
        SignatureSubject::Tag {
            hash,
            answer: Some((a, rec)),
            ..
        } => {
            assert_eq!((hash, a), (t, answer));
            assert_eq!(rec.targets, vec![t]);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        after.check_context(&landing_sig).unwrap().subject,
        SignatureSubject::Landing { hash, .. } if hash == landing
    ));
    // the older snapshot's report is unchanged by the take
    assert_eq!(
        json_bytes(&check_signature(
            &tag_sig,
            &before.check_context(&tag_sig).unwrap(),
            None
        )),
        json_bytes(&old_report)
    );
    drop((before, after));
    handle.close().unwrap();
}
