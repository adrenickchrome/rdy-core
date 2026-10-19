//! sig6: one signature checked offline against its subject (D-1Z951, D-17AA8). Validity,
//! coverage, trust and presence are separate results; the allowed-signers parser is strict; a
//! signature may name the project's own repo id or the source id of any fork its log records
//! (ruling task-6); fsck checks every signature with no agent and no trust file.
//!
//! The signatures are made by a test-only software signer from fixed in-memory seeds (as in
//! `signing_crypto.rs`): no private key is read or committed.

use ed25519_dalek::Signer as _;
use rdy_engine::allowed_signers::{AllowedSigners, read_allowed_signers};
use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::signature_check::{
    CheckContext, SignatureSubject, check_signature, read_check_context, signature_checks,
};
use rdy_engine::signing_crypto::blob_fingerprint;
use rdy_engine::{
    E_BAD_INPUT, E_IO, E_LIMIT, E_SIG_INVALID, E_SIG_OBJECT_MISMATCH, Rdy, RdyError, Session,
    SessionKind, WriteRequest,
};
use rdy_model::signature::{Signature, StatementSource, statement};
use rdy_model::{
    ActorRun, Answer, AnswerAct, Assurance, EvidenceKind, Hash, LandingRecord, Provenance,
    RelayEvidence, RepoId, Role, Tag,
};
use rdy_schema::Severity;
use serde_json::{Value, json};
use ssh_encoding::Encode;
use ssh_encoding::base64::{Base64, Encoding};
use ssh_key::public::{Ed25519PublicKey, KeyData, SkEd25519};
use ssh_key::sha2::{Digest, Sha256};
use ssh_key::{Algorithm, HashAlg, SshSig};

// ---- the software signer (test only) ----

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Ed,
    Sk,
    Other,
}

const COUNTER: u32 = 7;

impl Key {
    fn seed(self) -> [u8; 32] {
        match self {
            Key::Ed => [0x61; 32],
            Key::Sk => [0x62; 32],
            Key::Other => [0x63; 32],
        }
    }

    fn dalek(self) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&self.seed())
    }

    fn data(self) -> KeyData {
        let point = Ed25519PublicKey(self.dalek().verifying_key().to_bytes());
        match self {
            Key::Ed | Key::Other => KeyData::Ed25519(point),
            Key::Sk => KeyData::SkEd25519(SkEd25519::new(point, "ssh:")),
        }
    }

    fn blob(self) -> Vec<u8> {
        let mut out = vec![];
        self.data().encode(&mut out).unwrap();
        out
    }

    fn key_type(self) -> &'static str {
        match self {
            Key::Ed | Key::Other => "ssh-ed25519",
            Key::Sk => "sk-ssh-ed25519@openssh.com",
        }
    }

    fn fp(self) -> String {
        blob_fingerprint(&self.blob())
    }

    /// The public-key line, as an allowed-signers file holds it.
    fn line(self) -> String {
        format!(
            "{} {}",
            self.key_type(),
            Base64::encode_string(&self.blob())
        )
    }
}

/// A binary SSHSIG of `msg` in `ns` by `key` (sk: `flags`, counter `COUNTER`).
fn sshsig(key: Key, ns: &str, msg: &[u8], flags: u8) -> Vec<u8> {
    let data = SshSig::signed_data(ns, HashAlg::Sha512, msg).unwrap();
    let sig = match key {
        Key::Ed | Key::Other => ssh_key::Signature::new(
            Algorithm::Ed25519,
            key.dalek().sign(&data).to_bytes().to_vec(),
        ),
        Key::Sk => {
            let mut m = Sha256::digest(b"ssh:").to_vec();
            m.push(flags);
            m.extend(COUNTER.to_be_bytes());
            m.extend(Sha256::digest(&data));
            let mut raw = key.dalek().sign(&m).to_bytes().to_vec();
            raw.push(flags);
            raw.extend(COUNTER.to_be_bytes());
            ssh_key::Signature::new(Algorithm::SkEd25519, raw)
        }
    }
    .unwrap();
    let mut out = vec![];
    SshSig::new(key.data(), ns, HashAlg::Sha512, sig)
        .unwrap()
        .encode(&mut out)
        .unwrap();
    out
}

// ---- records ----

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

const REPO: RepoId = RepoId([0x11; 16]);
const OTHER_REPO: RepoId = RepoId([0x22; 16]);
const SOURCE_REPO: RepoId = RepoId([0x33; 16]);

fn prov(actor: &str, run: Option<&str>, at_ms: i64) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: run.map(Into::into),
        role: Role::Approver,
        at_ms,
    }
}

/// A landing approved by `human:founder`; relayed by `agent:a/b` (run `run/1`) when
/// `assurance` is Relayed.
fn landing(assurance: Assurance) -> LandingRecord {
    let relayed = assurance == Assurance::Relayed;
    LandingRecord {
        proposal: h(1),
        prev: Some(h(2)),
        state: h(3),
        approved_by: prov("human:founder", None, 5),
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance,
        relayed_by: relayed.then(|| ActorRun {
            actor: "agent:a/b".into(),
            run: Some("run/1".into()),
        }),
        relay_evidence: relayed.then(|| RelayEvidence {
            quoted: "yes, land it".into(),
            session_ref: "chat-1".into(),
            question: Some("land P-1@1?".into()),
        }),
        approved_version: Some(h(4)),
        rebased_from: vec![],
        evidence: None,
    }
}

fn tag() -> Tag {
    Tag {
        name: "v1.0.0".into(),
        landing: h(6),
        message: None,
        tagged_by: prov("human:founder", None, 9),
    }
}

/// The relay answer of `tag()` with hash `tag_hash`, relayed by `agent:t/x` (run `r9`).
fn tag_answer(tag_hash: Hash) -> Answer {
    Answer {
        item: "tag:v1.0.0".into(),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![tag_hash],
        person: prov("human:founder", None, 9),
        relayed_by: Some(ActorRun {
            actor: "agent:t/x".into(),
            run: Some("r9".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "tag it".into(),
            session_ref: "chat-2".into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Text),
        note: None,
    }
}

const TAG_HASH: Hash = Hash([0x77; 32]);
const LANDING_HASH: Hash = Hash([0x44; 32]);
const ANSWER_HASH: Hash = Hash([0x55; 32]);

fn landing_subject(rec: LandingRecord) -> SignatureSubject {
    SignatureSubject::Landing {
        hash: LANDING_HASH,
        record: rec,
    }
}

fn tag_subject(answer: Option<(Hash, Answer)>) -> SignatureSubject {
    SignatureSubject::Tag {
        hash: TAG_HASH,
        record: tag(),
        answer,
    }
}

fn ctx(subject: SignatureSubject) -> CheckContext {
    CheckContext {
        subject,
        current_repo: REPO,
        source_repos: vec![],
        holder: false,
    }
}

/// A signature of `subject` (its hash) over `stmt` in `ns`, by `key`, asked for by `agent:s`
/// (run `s/1`) at `at_ms`.
fn signature(subject: Hash, ns: &str, stmt: Vec<u8>, key: Key, flags: u8) -> Signature {
    let sig = sshsig(key, ns, &stmt, flags);
    Signature {
        subject,
        namespace: ns.into(),
        statement: stmt,
        sig,
        signed_by: prov("agent:s", Some("s/1"), 1_000),
    }
}

/// The signature of `source` for `repo` by `key`, in its statement kind's namespace.
fn signed(
    subject: Hash,
    source: StatementSource<'_>,
    repo: RepoId,
    key: Key,
    flags: u8,
) -> Signature {
    let stmt = statement(source, Some(repo), &key.fp()).unwrap();
    let ns = rdy_model::signature::parse_statement(&stmt)
        .unwrap()
        .namespace();
    signature(subject, ns, stmt, key, flags)
}

fn approval_sig(rec: &LandingRecord, key: Key) -> Signature {
    signed(LANDING_HASH, StatementSource::Approval(rec), REPO, key, 1)
}

fn relay_sig(rec: &LandingRecord, key: Key) -> Signature {
    signed(
        LANDING_HASH,
        StatementSource::RelayApproval(rec),
        REPO,
        key,
        1,
    )
}

fn tag_sig(key: Key) -> Signature {
    signed(
        TAG_HASH,
        StatementSource::Tag {
            hash: TAG_HASH,
            tag: &tag(),
        },
        REPO,
        key,
        1,
    )
}

fn relay_tag_sig(key: Key) -> Signature {
    signed(
        TAG_HASH,
        StatementSource::RelayTag {
            hash: TAG_HASH,
            tag: &tag(),
            answer_hash: ANSWER_HASH,
            answer: &tag_answer(TAG_HASH),
        },
        REPO,
        key,
        1,
    )
}

fn json_of(c: &rdy_engine::signature_check::SignatureCheck) -> Value {
    serde_json::to_value(c).unwrap()
}

fn trust(text: &str) -> AllowedSigners {
    AllowedSigners::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn statement_of(c: &rdy_engine::signature_check::SignatureCheck) -> (&str, Option<&str>) {
    (c.statement.as_str(), c.code.as_deref())
}

#[allow(non_snake_case)]
fn SIGNED_BY() -> Value {
    json!({"actor": "agent:s", "run": "s/1"})
}

// ---- the pure check ----

#[test]
fn sig6_crypto_coverage_and_trust_are_independent() {
    let rec = landing(Assurance::Local);
    let c = ctx(landing_subject(rec.clone()));
    let trusted = trust(&format!(
        "# the founder\nhuman:founder namespaces=\"rdy-approval-v1\" {}\n",
        Key::Ed.line()
    ));

    // valid, covered, trusted: no code, no warning
    let good = approval_sig(&rec, Key::Ed);
    assert_eq!(
        json_of(&check_signature(&good, &c, Some(&trusted))),
        json!({
            "kind": "approval", "key": Key::Ed.fp(), "key_type": "ssh-ed25519",
            "signed": "valid", "statement": "match", "trust": "trusted", "presence": "absent",
            "signed_by": SIGNED_BY(), "at_ms": 1000, "code": null, "warnings": []
        })
    );

    // valid, covered, unknown: no file, or a file that does not hold the key
    let unknown = json!({
        "kind": "approval", "key": Key::Ed.fp(), "key_type": "ssh-ed25519",
        "signed": "valid", "statement": "match", "trust": "unknown", "presence": "absent",
        "signed_by": SIGNED_BY(), "at_ms": 1000, "code": null,
        "warnings": [{
            "code": "W_SIG_UNKNOWN_KEY",
            "message": format!("key {} is not trusted for human:founder in rdy-approval-v1", Key::Ed.fp()),
            "fix": "add the key and principal to signing.allowed_signers"
        }]
    });
    assert_eq!(json_of(&check_signature(&good, &c, None)), unknown);
    let others = trust(&format!("human:founder {}\n", Key::Other.line()));
    assert_eq!(json_of(&check_signature(&good, &c, Some(&others))), unknown);

    // a valid sk signature reports its authenticated presence bit, trusted or not
    let sk = approval_sig(&rec, Key::Sk);
    let sk_trust = trust(&format!("human:founder {}\n", Key::Sk.line()));
    let got = check_signature(&sk, &c, Some(&sk_trust));
    assert_eq!(
        (
            got.signed.as_str(),
            got.trust.as_str(),
            got.presence.as_str()
        ),
        ("valid", "trusted", "present")
    );
    assert_eq!(got.key_type.as_deref(), Some("sk-ssh-ed25519@openssh.com"));
    let no_touch = signed(
        LANDING_HASH,
        StatementSource::Approval(&rec),
        REPO,
        Key::Sk,
        0,
    );
    assert_eq!(
        check_signature(&no_touch, &c, Some(&sk_trust)).presence,
        "absent"
    );

    // valid, not covered (another repo): mismatch, never trusted, no trust advice, presence kept
    let elsewhere = signed(
        LANDING_HASH,
        StatementSource::Approval(&rec),
        OTHER_REPO,
        Key::Sk,
        1,
    );
    assert_eq!(
        json_of(&check_signature(&elsewhere, &c, Some(&sk_trust))),
        json!({
            "kind": "approval", "key": Key::Sk.fp(), "key_type": "sk-ssh-ed25519@openssh.com",
            "signed": "valid", "statement": "mismatch", "trust": "unknown", "presence": "present",
            "signed_by": SIGNED_BY(), "at_ms": 1000, "code": "E_SIG_OBJECT_MISMATCH",
            "warnings": []
        })
    );

    // invalid crypto: the readable kind and key stay, everything else is the invalid shape
    let mut forged = sk.clone();
    let last = forged.sig.len() - 10;
    forged.sig[last] ^= 0x01;
    let invalid = |kind: Value, key: Value, key_type: Value| {
        json!({
            "kind": kind, "key": key, "key_type": key_type,
            "signed": "invalid", "statement": "mismatch", "trust": "unknown",
            "presence": "absent", "signed_by": SIGNED_BY(), "at_ms": 1000,
            "code": "E_SIG_INVALID", "warnings": []
        })
    };
    assert_eq!(
        json_of(&check_signature(&forged, &c, Some(&sk_trust))),
        invalid(
            json!("approval"),
            json!(Key::Sk.fp()),
            json!("sk-ssh-ed25519@openssh.com")
        )
    );
    // the presence bit flipped after signing is not authenticated
    let mut flipped = no_touch.clone();
    let flags_at = flipped.sig.len() - 5;
    flipped.sig[flags_at] |= 0x01;
    assert_eq!(
        json_of(&check_signature(&flipped, &c, Some(&sk_trust))),
        invalid(
            json!("approval"),
            json!(Key::Sk.fp()),
            json!("sk-ssh-ed25519@openssh.com")
        )
    );
    // an unreadable blob: kind readable, key and type null
    let mut garbage = good.clone();
    garbage.sig = b"not an sshsig".to_vec();
    assert_eq!(
        json_of(&check_signature(&garbage, &c, None)),
        invalid(json!("approval"), Value::Null, Value::Null)
    );
    // a valid signature over text outside the grammar: kind null
    let mut text = b"rdy-statement: 1\nkind: approval\n".to_vec();
    text.extend(b"repo: nope\n");
    let ungrammatical = signature(LANDING_HASH, "rdy-approval-v1", text, Key::Ed, 0);
    assert_eq!(
        json_of(&check_signature(&ungrammatical, &c, None)),
        invalid(Value::Null, json!(Key::Ed.fp()), json!("ssh-ed25519"))
    );
    // the statement names another key than the one that signed it
    let stmt = statement(
        StatementSource::Approval(&rec),
        Some(REPO),
        &Key::Other.fp(),
    )
    .unwrap();
    let wrong_key = signature(LANDING_HASH, "rdy-approval-v1", stmt.clone(), Key::Ed, 0);
    assert_eq!(
        json_of(&check_signature(&wrong_key, &c, Some(&trusted))),
        invalid(json!("approval"), json!(Key::Ed.fp()), json!("ssh-ed25519"))
    );
    // the record's namespace is not the statement kind's
    let stmt = statement(StatementSource::Approval(&rec), Some(REPO), &Key::Ed.fp()).unwrap();
    let wrong_ns = signature(LANDING_HASH, "rdy-tag-v1", stmt, Key::Ed, 0);
    assert_eq!(
        json_of(&check_signature(&wrong_ns, &c, Some(&trusted))),
        invalid(json!("approval"), json!(Key::Ed.fp()), json!("ssh-ed25519"))
    );
    // trust never rescues an invalid or mismatched signature, and adds no warning to either
    for g in [&forged, &garbage, &wrong_key, &elsewhere] {
        let got = check_signature(g, &c, Some(&trusted));
        assert_eq!((got.trust.as_str(), got.warnings.len()), ("unknown", 0));
    }
    // signed_by and at_ms are the record's, whatever the subject says
    let mut later = good.clone();
    later.signed_by = prov("human:other", None, -3);
    let got = json_of(&check_signature(&later, &c, Some(&trusted)));
    assert_eq!(
        (&got["signed_by"], &got["at_ms"], &got["statement"]),
        (
            &json!({"actor": "human:other", "run": null}),
            &json!(-3),
            &json!("match")
        )
    );
}

/// Asserts that `g` is covered by `base` and not by any of `changed` (labelled).
fn assert_coverage(g: &Signature, base: &CheckContext, changed: Vec<(&str, CheckContext)>) {
    let got = check_signature(g, base, None);
    assert_eq!(statement_of(&got), ("match", None), "the base context");
    for (label, c) in changed {
        let got = check_signature(g, &c, None);
        assert_eq!(
            (got.signed.as_str(), statement_of(&got)),
            ("valid", ("mismatch", Some(E_SIG_OBJECT_MISMATCH))),
            "{label}"
        );
    }
}

#[test]
fn sig6_every_subject_field_changes_coverage() {
    let landing_ctx = |f: &dyn Fn(&mut LandingRecord), base: Assurance| {
        let mut rec = landing(base);
        f(&mut rec);
        ctx(landing_subject(rec))
    };
    let with_repo = |mut c: CheckContext, repo: RepoId| {
        c.current_repo = repo;
        c
    };

    // an approval covers repo, approved_version, base, actor and assurance
    let rec = landing(Assurance::Local);
    let g = approval_sig(&rec, Key::Ed);
    let base = ctx(landing_subject(rec.clone()));
    assert_coverage(
        &g,
        &base,
        vec![
            ("repo", with_repo(base.clone(), OTHER_REPO)),
            (
                "approved_version",
                landing_ctx(&|r| r.approved_version = Some(h(40)), Assurance::Local),
            ),
            (
                "prev",
                landing_ctx(&|r| r.prev = Some(h(41)), Assurance::Local),
            ),
            (
                "first landing",
                landing_ctx(&|r| r.prev = None, Assurance::Local),
            ),
            (
                "actor",
                landing_ctx(
                    &|r| r.approved_by.actor = "human:x".into(),
                    Assurance::Local,
                ),
            ),
            (
                "assurance",
                landing_ctx(
                    &|r| {
                        r.assurance = Assurance::Relayed;
                        r.relayed_by = Some(ActorRun {
                            actor: "agent:a".into(),
                            run: None,
                        });
                    },
                    Assurance::Local,
                ),
            ),
            (
                "subject hash",
                CheckContext {
                    subject: SignatureSubject::Landing {
                        hash: h(42),
                        record: rec.clone(),
                    },
                    ..base.clone()
                },
            ),
        ],
    );
    // the proposal stands in for an absent approved_version, and only then
    let mut fallback = landing(Assurance::Local);
    fallback.approved_version = None;
    let g = approval_sig(&fallback, Key::Ed);
    let base = ctx(landing_subject(fallback.clone()));
    assert_coverage(
        &g,
        &base,
        vec![
            (
                "fallback proposal",
                landing_ctx(
                    &|r| {
                        r.approved_version = None;
                        r.proposal = h(43);
                    },
                    Assurance::Local,
                ),
            ),
            (
                "approved_version set",
                landing_ctx(&|r| r.approved_version = Some(h(51)), Assurance::Local),
            ),
        ],
    );
    // the approved_version decides when it is set: the proposal is not covered then
    let g = approval_sig(&rec, Key::Ed);
    let moved = landing_ctx(&|r| r.proposal = h(44), Assurance::Local);
    assert_eq!(
        statement_of(&check_signature(&g, &moved, None)),
        ("match", None)
    );

    // a relayed approval covers the relayer and every part of the relay evidence
    let relayed = landing(Assurance::Relayed);
    let g = relay_sig(&relayed, Key::Ed);
    let base = ctx(landing_subject(relayed.clone()));
    let r = |f: &dyn Fn(&mut LandingRecord)| landing_ctx(f, Assurance::Relayed);
    assert_coverage(
        &g,
        &base,
        vec![
            ("repo", with_repo(base.clone(), OTHER_REPO)),
            ("approved_version", r(&|l| l.approved_version = Some(h(45)))),
            ("prev", r(&|l| l.prev = None)),
            ("actor", r(&|l| l.approved_by.actor = "human:y".into())),
            (
                "relayer actor",
                r(&|l| l.relayed_by.as_mut().unwrap().actor = "agent:a".into()),
            ),
            (
                "relayer run",
                r(&|l| l.relayed_by.as_mut().unwrap().run = None),
            ),
            (
                "quoted",
                r(&|l| l.relay_evidence.as_mut().unwrap().quoted = "no".into()),
            ),
            (
                "session_ref",
                r(&|l| l.relay_evidence.as_mut().unwrap().session_ref = "chat-9".into()),
            ),
            (
                "question",
                r(&|l| l.relay_evidence.as_mut().unwrap().question = None),
            ),
            (
                "evidence",
                r(&|l| {
                    l.evidence = Some(rdy_model::ApprovalEvidence {
                        kind: EvidenceKind::Card,
                        options: vec!["approve".into()],
                        choice: Some("approve".into()),
                        card_hash: Some(h(46)),
                    })
                }),
            ),
            ("assurance", r(&|l| l.assurance = Assurance::Local)),
        ],
    );

    // a tag covers repo, its hash, name, landing and author
    let g = tag_sig(Key::Ed);
    let base = ctx(tag_subject(None));
    let tag_ctx = |f: &dyn Fn(&mut Tag)| {
        let mut t = tag();
        f(&mut t);
        ctx(SignatureSubject::Tag {
            hash: TAG_HASH,
            record: t,
            answer: None,
        })
    };
    assert_coverage(
        &g,
        &base,
        vec![
            ("repo", with_repo(base.clone(), OTHER_REPO)),
            (
                "tag hash",
                ctx(SignatureSubject::Tag {
                    hash: h(47),
                    record: tag(),
                    answer: None,
                }),
            ),
            ("name", tag_ctx(&|t| t.name = "v1.0.1".into())),
            ("landing", tag_ctx(&|t| t.landing = h(48))),
            ("actor", tag_ctx(&|t| t.tagged_by.actor = "human:z".into())),
        ],
    );

    // a tag relay covers the ANSWER: its hash, its exact targets, item, act, person, relayer
    let g = relay_tag_sig(Key::Ed);
    let base = ctx(tag_subject(Some((ANSWER_HASH, tag_answer(TAG_HASH)))));
    let answer_ctx = |hash: Hash, f: &dyn Fn(&mut Answer)| {
        let mut a = tag_answer(TAG_HASH);
        f(&mut a);
        ctx(tag_subject(Some((hash, a))))
    };
    assert_coverage(
        &g,
        &base,
        vec![
            ("repo", with_repo(base.clone(), OTHER_REPO)),
            ("no answer", ctx(tag_subject(None))),
            ("answer hash", answer_ctx(h(49), &|_| {})),
            (
                "targets another",
                answer_ctx(ANSWER_HASH, &|a| a.targets = vec![h(50)]),
            ),
            (
                "targets more",
                answer_ctx(ANSWER_HASH, &|a| a.targets = vec![TAG_HASH, h(50)]),
            ),
            (
                "targets none",
                answer_ctx(ANSWER_HASH, &|a| a.targets = vec![]),
            ),
            (
                "item",
                answer_ctx(ANSWER_HASH, &|a| a.item = "tag:v2.0.0".into()),
            ),
            (
                "act",
                answer_ctx(ANSWER_HASH, &|a| a.act = AnswerAct::Confirm),
            ),
            (
                "person",
                answer_ctx(ANSWER_HASH, &|a| a.person.actor = "human:q".into()),
            ),
            (
                "relayer",
                answer_ctx(ANSWER_HASH, &|a| {
                    a.relayed_by.as_mut().unwrap().actor = "agent:t".into()
                }),
            ),
            (
                "tag name",
                ctx(SignatureSubject::Tag {
                    hash: TAG_HASH,
                    record: Tag {
                        name: "v1.0.1".into(),
                        ..tag()
                    },
                    answer: Some((ANSWER_HASH, tag_answer(TAG_HASH))),
                }),
            ),
        ],
    );

    // a missing subject is a mismatch, never a panic
    for g in [approval_sig(&rec, Key::Ed), tag_sig(Key::Sk)] {
        let c = ctx(SignatureSubject::Missing { hash: g.subject });
        let got = check_signature(&g, &c, None);
        assert_eq!(
            (got.signed.as_str(), statement_of(&got)),
            ("valid", ("mismatch", Some(E_SIG_OBJECT_MISMATCH)))
        );
    }
}

#[test]
fn sig6_trust_uses_the_structured_relay_actor() {
    let relayed = landing(Assurance::Relayed);
    let c = ctx(landing_subject(relayed.clone()));
    let g = relay_sig(&relayed, Key::Ed);
    // the statement joins the relayer's actor and run with one `/`
    let text = String::from_utf8(g.statement.clone()).unwrap();
    assert!(text.contains("relayed_by: agent:a/b/run/1\n"), "{text}");
    let line = Key::Ed.line();
    let trusted = |file: String| check_signature(&g, &c, Some(&trust(&file))).trust;

    // the principal is relayed_by.actor exactly: never the joined text, never a cut prefix
    assert_eq!(trusted(format!("agent:a/b {line}")), "trusted");
    assert_eq!(trusted(format!("agent:a/b/run/1 {line}")), "unknown");
    assert_eq!(trusted(format!("agent:a {line}")), "unknown");
    assert_eq!(trusted(format!("human:founder {line}")), "unknown");
    // patterns: `*`, `?`, a negation that wins, at least one positive, case-sensitive
    assert_eq!(trusted(format!("agent:* {line}")), "trusted");
    assert_eq!(trusted(format!("agent:a?b {line}")), "trusted");
    assert_eq!(trusted(format!("x,agent:a/b {line}")), "trusted");
    assert_eq!(trusted(format!("agent:*,!agent:a/* {line}")), "unknown");
    assert_eq!(trusted(format!("!agent:x {line}")), "unknown");
    assert_eq!(trusted(format!("AGENT:a/b {line}")), "unknown");
    assert_eq!(
        trusted(format!("agent:a/b {}", Key::Other.line())),
        "unknown"
    );
    // namespaces lists, with patterns; time options are read and ignored
    assert_eq!(
        trusted(format!("agent:a/b namespaces=\"rdy-approval-v1\" {line}")),
        "unknown"
    );
    assert_eq!(
        trusted(format!(
            "agent:a/b namespaces=\"rdy-tag-v1,rdy-relay-v1\" {line}"
        )),
        "trusted"
    );
    assert_eq!(
        trusted(format!("agent:a/b namespaces=\"rdy-*\" {line}")),
        "trusted"
    );
    assert_eq!(
        trusted(format!(
            "agent:a/b namespaces=\"rdy-*,!rdy-relay-v1\" {line}"
        )),
        "unknown"
    );
    assert_eq!(
        trusted(format!(
            "agent:a/b\tvalid-after=\"19700101\",VALID-BEFORE=\"not a time\",namespaces=\"rdy-relay-v1\"\t{line}   a comment # here"
        )),
        "trusted"
    );
    assert_eq!(
        trusted(format!("\"agent:a/b\" {line}")),
        "trusted",
        "a quoted principals field"
    );
    // a later line can trust what an earlier one does not
    assert_eq!(
        trusted(format!("\n# c\nagent:x {line}\n\n  agent:a/b {line}\n")),
        "trusted"
    );

    // the approval in the same landing is the approver's, not the relayer's
    let a = approval_sig(&relayed, Key::Ed);
    let file = trust(&format!("agent:a/b {line}"));
    let got = check_signature(&a, &c, Some(&file));
    assert_eq!(
        (got.trust.as_str(), got.warnings[0].message.as_str()),
        (
            "unknown",
            format!(
                "key {} is not trusted for human:founder in rdy-approval-v1",
                Key::Ed.fp()
            )
            .as_str()
        )
    );
    assert_eq!(
        check_signature(&a, &c, Some(&trust(&format!("human:founder {line}")))).trust,
        "trusted"
    );
    // a tag relay's principal is the ANSWER's relayed_by.actor
    let tc = ctx(tag_subject(Some((ANSWER_HASH, tag_answer(TAG_HASH)))));
    let tg = relay_tag_sig(Key::Ed);
    assert_eq!(
        check_signature(&tg, &tc, Some(&trust(&format!("agent:t/x {line}")))).trust,
        "trusted"
    );
    assert_eq!(
        check_signature(&tg, &tc, Some(&trust(&format!("agent:t/x/r9 {line}")))).trust,
        "unknown"
    );
    assert_eq!(
        check_signature(&tg, &tc, Some(&trust(&format!("human:founder {line}")))).trust,
        "unknown"
    );
    // a tag's principal is its author
    let t = tag_sig(Key::Ed);
    assert_eq!(
        check_signature(&t, &tc, Some(&trust(&format!("human:founder {line}")))).trust,
        "trusted"
    );

    // the parser refuses what it does not accept, by line, in the shared words
    let refused = |file: String, line: usize, cause: &str| {
        let e = AllowedSigners::parse(file.as_bytes()).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                format!("signing.allowed_signers line {line} is invalid: {cause}").as_str(),
                format!("fix line {line} in signing.allowed_signers").as_str()
            ),
            "{file}"
        );
    };
    let ok = format!("agent:a/b {line}\n");
    refused(
        format!("{ok}agent:a/b cert-authority {line}\n"),
        2,
        "cert-authority is not accepted",
    );
    refused(
        format!("# x\n\n{ok}agent:a/b namespace=\"rdy-relay-v1\" {line}\n"),
        4,
        "unknown option namespace; did you mean namespaces?",
    );
    refused(
        format!("agent:a/b valid-afte=\"1\" {line}"),
        1,
        "unknown option valid-afte; did you mean valid-after?",
    );
    refused(
        format!("agent:a/b no-touch-required {line}"),
        1,
        "unknown option no-touch-required",
    );
    refused(
        format!("agent:a/b namespaces=\"a\",namespaces=\"b\" {line}"),
        1,
        "duplicate option",
    );
    refused(
        format!("agent:a/b namespaces=\"a,,b\" {line}"),
        1,
        "invalid option",
    );
    refused(
        format!("agent:a/b namespaces=\"a {line}"),
        1,
        "invalid option",
    );
    refused(
        format!("agent:a/b namespaces=a {line}"),
        1,
        "invalid option",
    );
    refused(
        format!("agent:a/b namespaces=\"a\", {line}"),
        1,
        "invalid option",
    );
    refused(format!("a,,b {line}"), 1, "invalid principal list");
    refused(format!("a,! {line}"), 1, "invalid principal list");
    refused("agent:a/b\n".into(), 1, "invalid key");
    refused("agent:a/b ssh-ed25519\n".into(), 1, "invalid key");
    refused("agent:a/b ssh-ed25519 AAAA\n".into(), 1, "invalid key");
    refused(
        format!(
            "agent:a/b ssh-ed25519 {}",
            Base64::encode_string(&Key::Sk.blob())
        ),
        1,
        "invalid key",
    );
    let rsa = include_str!("fixtures/sshsig/id_rsa_3072.pub");
    refused(format!("agent:a/b {rsa}"), 1, "invalid key");
    let cert = include_str!("fixtures/sshsig/id_ed25519-cert.pub");
    refused(format!("agent:a/b {cert}"), 1, "invalid key");
}

/// The `E_BAD_INPUT` of `file`, as (message, fix).
fn trust_refusal(file: &[u8]) -> (String, String) {
    let e = AllowedSigners::parse(file).expect_err("the file is refused");
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    (e.message, e.fix)
}

fn invalid_line(line: usize, cause: &str) -> (String, String) {
    (
        format!("signing.allowed_signers line {line} is invalid: {cause}"),
        format!("fix line {line} in signing.allowed_signers"),
    )
}

/// OpenSSH reads a line as a C string, so a NUL ends it there; it then refuses the file. With
/// a key from `ssh-keygen -t ed25519` and a `rdy-approval-v1` signature by it, OpenSSH 10.3p1
/// `ssh-keygen -Y verify -I human:founder -n rdy-approval-v1` printed `a1:1: invalid line` for
/// `*,!human:founder<NUL> <key>`, `a1b:2: invalid line` for that line after `agent:x <key>`,
/// and `a1c:1: invalid options` for `human:founder namespaces="rdy-*,!rdy-approval-v1<NUL>"
/// <key>`; each time `Could not verify signature.` (exit 255). No NUL can make a negative
/// pattern miss its principal.
#[test]
fn sig6_a_nul_in_a_trust_line_refuses_the_whole_file() {
    let rec = landing(Assurance::Local);
    let c = ctx(landing_subject(rec.clone()));
    let a = approval_sig(&rec, Key::Ed);
    let line = Key::Ed.line();
    // without the NUL the negation excludes the founder, and the file parses
    let excluded = trust(&format!("*,!human:founder {line}"));
    assert_eq!(check_signature(&a, &c, Some(&excluded)).trust, "unknown");

    assert_eq!(
        trust_refusal(format!("*,!human:founder\0 {line}").as_bytes()),
        invalid_line(1, "invalid principal list")
    );
    // a good line first does not survive a refused later line
    assert_eq!(
        trust_refusal(format!("agent:x {line}\n*,!human:founder\0 {line}\n").as_bytes()),
        invalid_line(2, "invalid principal list")
    );
    assert_eq!(
        trust_refusal(format!("\"*,!human:founder\0\" {line}").as_bytes()),
        invalid_line(1, "invalid principal list")
    );
    // in an option value or name
    assert_eq!(
        trust_refusal(
            format!("human:founder namespaces=\"rdy-*,!rdy-approval-v1\0\" {line}").as_bytes()
        ),
        invalid_line(1, "invalid option")
    );
    assert_eq!(
        trust_refusal(format!("human:founder valid-after\0=\"1\" {line}").as_bytes()),
        invalid_line(1, "invalid option")
    );
}

/// OpenSSH finds the end of the options field with `sshkey_advance_past_options`: a `\` skips
/// the next byte only when that byte is `"`, and the value is read with `opt_dequote`, the same
/// rule. With the key and signature above, OpenSSH 10.3p1 accepted
/// `human:founder namespaces="rdy-approval-v1,unused\\" text" <key>`: `Good "rdy-approval-v1"
/// signature for human:founder with ED25519 key ...` (exit 0).
#[test]
fn sig6_quoted_options_end_where_openssh_ends_them() {
    let rec = landing(Assurance::Local);
    let c = ctx(landing_subject(rec.clone()));
    let a = approval_sig(&rec, Key::Ed);
    let line = Key::Ed.line();
    // two backslashes, a quote, a space, `text`, the closing quote
    let file = format!("human:founder namespaces=\"rdy-approval-v1,unused\\\\\" text\" {line}");
    assert_eq!(
        check_signature(&a, &c, Some(&trust(&file))).trust,
        "trusted"
    );
    // the value holds `unused\" text`: an escaped quote, then the space, inside the quotes
    let file = format!("human:founder namespaces=\"unused\\\\\" text,rdy-approval-v1\" {line}");
    assert_eq!(
        check_signature(&a, &c, Some(&trust(&file))).trust,
        "trusted"
    );
    // a `\` before any other byte is itself
    let file = format!("human:founder namespaces=\"a\\b,rdy-approval-v1\" {line}");
    assert_eq!(
        check_signature(&a, &c, Some(&trust(&file))).trust,
        "trusted"
    );
    let file = format!("human:founder namespaces=\"rdy-approval-v\\1\" {line}");
    assert_eq!(
        check_signature(&a, &c, Some(&trust(&file))).trust,
        "unknown"
    );
}

/// The pure checker refuses a record over the shared bounds before it verifies or copies any
/// field: `E_SIG_INVALID`, never a valid match, with no field of the record in the report.
#[test]
fn sig6_the_checker_refuses_records_over_the_shared_bounds() {
    use rdy_model::signature::{MAX_NAMESPACE, MAX_SIG, MAX_SIGNER_TEXT, MAX_STATEMENT};
    let rec = landing(Assurance::Local);
    let c = ctx(landing_subject(rec.clone()));
    let good = approval_sig(&rec, Key::Ed);
    let refused = |g: &Signature, what: &str| {
        let got = check_signature(g, &c, None);
        assert_eq!(
            json_of(&got),
            json!({
                "kind": null, "key": null, "key_type": null,
                "signed": "invalid", "statement": "mismatch", "trust": "unknown",
                "presence": "absent", "signed_by": {"actor": "", "run": null},
                "at_ms": 1000, "code": E_SIG_INVALID, "warnings": []
            }),
            "{what}"
        );
    };
    let matched = |g: &Signature, what: &str| {
        let got = check_signature(g, &c, None);
        assert_eq!(statement_of(&got), ("match", None), "{what}");
        assert_eq!(got.signed_by.actor, g.signed_by.actor, "{what}");
        assert_eq!(got.signed_by.run, g.signed_by.run, "{what}");
    };
    // signed_by is not signed: the actor and run at their bound still match
    let mut g = good.clone();
    g.signed_by.actor = "a".repeat(MAX_SIGNER_TEXT);
    matched(&g, "actor at its bound");
    g.signed_by.actor.push('a');
    refused(&g, "actor one past its bound");
    g.signed_by.actor = "a".repeat(70_000);
    refused(&g, "a 70000-byte actor");
    let mut g = good.clone();
    g.signed_by.run = Some("r".repeat(MAX_SIGNER_TEXT));
    matched(&g, "run at its bound");
    g.signed_by.run = Some("r".repeat(MAX_SIGNER_TEXT + 1));
    refused(&g, "run one past its bound");
    // the namespace, statement and SSHSIG blob
    let mut g = good.clone();
    g.namespace = "n".repeat(MAX_NAMESPACE + 1);
    refused(&g, "namespace");
    let mut g = good.clone();
    g.statement.resize(MAX_STATEMENT + 1, b'x');
    refused(&g, "statement");
    let mut g = good.clone();
    g.sig.resize(MAX_SIG + 1, 0);
    refused(&g, "sig");
    let mut g = good.clone();
    g.sig.resize(70_000, 0);
    refused(&g, "a 70000-byte sig");
    // every field at its bound encodes within the 65536-byte record bound (the store's bounded
    // encoder accepts it), so the field bounds imply the total bound
    let mut g = good.clone();
    g.namespace = "n".repeat(MAX_NAMESPACE);
    g.statement = vec![b'x'; MAX_STATEMENT];
    g.sig = vec![0; MAX_SIG];
    g.signed_by.actor = "a".repeat(MAX_SIGNER_TEXT);
    g.signed_by.run = Some("r".repeat(MAX_SIGNER_TEXT));
    g.signed_by.at_ms = i64::MIN;
    rdy_store::signature::frames(std::slice::from_ref(&g)).unwrap();
    let got = check_signature(&g, &c, None);
    assert_eq!(
        (got.signed.as_str(), got.signed_by.actor.len()),
        ("invalid", MAX_SIGNER_TEXT),
        "a record within the bounds keeps its signer in the report"
    );
}

// ---- projects ----

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

/// Writes one doc to view `work`, proposes and lands it; returns the landing's hash.
fn land_one(r: &mut Rdy, title: &str) -> Hash {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", title, None)
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

/// The approval signature of stored landing `l` for `repo`, by `key`, at `at_ms`.
fn stored_approval(r: &Rdy, l: &Hash, repo: RepoId, key: Key, at_ms: i64) -> Signature {
    let rec = r.store().reader().landing(l).unwrap();
    assert_eq!(rec.assurance, Assurance::Local);
    let mut g = signed(*l, StatementSource::Approval(&rec), repo, key, 1);
    g.signed_by.at_ms = at_ms;
    g
}

/// Stores the signature `make` builds from `r`, and publishes it.
fn put(r: &mut Rdy, make: impl FnOnce(&Rdy) -> Signature) -> Hash {
    let g = make(r);
    let id = r.store_mut().put_signature(&g).unwrap();
    r.seal_open().unwrap();
    id
}

fn statements(r: &Rdy, l: &Hash) -> Vec<(String, Option<String>)> {
    signature_checks(&r.read(), l, None)
        .unwrap()
        .into_iter()
        .map(|c| (c.statement, c.code))
        .collect()
}

fn matched(n: usize) -> Vec<(String, Option<String>)> {
    vec![("match".to_string(), None); n]
}

/// The repo ids a signature may name: the project's own, and the source of every fork its log
/// records (ruling task-6: every `meta/origin` with `fork: true`, current or earlier).
fn lineage(r: &Rdy, g: &Signature) -> (RepoId, Vec<RepoId>) {
    let c = read_check_context(&r.read(), g).unwrap();
    (c.current_repo, c.source_repos)
}

#[test]
fn sig6_fork_sources_follow_the_logged_fork_lineage() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    let mut src = Rdy::init(&d.path().join("src"), "rdy-standard").unwrap();
    let l1 = land_one(&mut src, "First");
    let source_id = src.read().repo_id().unwrap();
    // two signatures that differ only in signed_by.at_ms: both covered
    put(&mut src, |x| stored_approval(x, &l1, source_id, Key::Ed, 1));
    put(&mut src, |x| {
        stored_approval(x, &l1, source_id, Key::Ed, 9_999_999)
    });
    // a later landing leaves the first one historical, and still checkable
    let l2 = land_one(&mut src, "Second");
    assert_ne!(src.read().canon_head(), Some(l1));
    assert_eq!(statements(&src, &l1), matched(2));
    let st = src.read();
    let g = src
        .store()
        .reader()
        .signature(&src.store().reader().signatures_of(&l1).unwrap()[0])
        .unwrap();
    let c = read_check_context(&st, &g).unwrap();
    assert!(matches!(c.subject, SignatureSubject::Landing { hash, .. } if hash == l1));
    // a project that was never forked accepts only its own repo id
    assert_eq!(lineage(&src, &g), (source_id, vec![]));

    // an ordinary clone keeps the source's repo id: no alternate id is needed or allowed
    let plain = d.path().join("clone");
    clone(CloneSource::State(&st), &plain, false).unwrap();
    let mut cl = Rdy::open(&plain).unwrap();
    assert_eq!(lineage(&cl, &g), (source_id, vec![]));
    assert_eq!(statements(&cl, &l1), matched(2));

    // a fork has a new repo id; its origin's id covers the retained source signatures
    let forked = d.path().join("fork");
    clone(CloneSource::State(&st), &forked, true).unwrap();
    drop(st);
    let mut fk = Rdy::open(&forked).unwrap();
    let fork_id = fk.read().repo_id().unwrap();
    assert_ne!(fork_id, source_id);
    assert_eq!(lineage(&fk, &g), (fork_id, vec![source_id]));
    assert_eq!(statements(&fk, &l1), matched(2));
    assert_eq!(
        fk.fsck().unwrap().signatures,
        2,
        "the fork's fsck checks them through its origin"
    );
    // a new signature in the fork uses the fork's own id
    put(&mut fk, |x| stored_approval(x, &l2, fork_id, Key::Sk, 2));
    assert_eq!(statements(&fk, &l2), matched(1));

    // original -> fork -> ordinary clone: the clone keeps the fork's id, its log keeps the
    // fork's origin batch, so the source's signatures still verify, and the clone's own fsck
    // (run by the clone itself) passes
    let fst = fk.read();
    let fork_clone = d.path().join("fork-clone");
    clone(CloneSource::State(&fst), &fork_clone, false).unwrap();
    let fc = Rdy::open(&fork_clone).unwrap();
    assert_eq!(fc.read().origin().unwrap().map(|o| o.fork), Some(false));
    assert_eq!(lineage(&fc, &g), (fork_id, vec![source_id]));
    assert_eq!(statements(&fc, &l1), matched(2));
    assert_eq!(statements(&fc, &l2), matched(1));
    assert_eq!(fc.fsck().unwrap().signatures, 3);

    // original -> fork -> fork: both ancestors' ids are accepted
    let fork_fork = d.path().join("fork-fork");
    clone(CloneSource::State(&fst), &fork_fork, true).unwrap();
    drop(fst);
    let mut ff = Rdy::open(&fork_fork).unwrap();
    let ff_id = ff.read().repo_id().unwrap();
    assert!(ff_id != fork_id && ff_id != source_id);
    let mut ancestors = vec![source_id, fork_id];
    ancestors.sort();
    assert_eq!(lineage(&ff, &g), (ff_id, ancestors.clone()));
    assert_eq!(statements(&ff, &l1), matched(2));
    assert_eq!(statements(&ff, &l2), matched(1));
    assert_eq!(ff.fsck().unwrap().signatures, 3);
    put(&mut ff, |x| stored_approval(x, &l2, ff_id, Key::Ed, 5));
    assert_eq!(statements(&ff, &l2), matched(2));

    // a forged signature naming an unrelated repo id is still refused, in a fork and in a
    // fork of a fork
    let forged = |r: &mut Rdy, at_ms: i64| {
        put(r, |x| stored_approval(x, &l2, OTHER_REPO, Key::Sk, at_ms));
        let mut got = statements(r, &l2);
        got.sort();
        assert_eq!(
            got.last().unwrap(),
            &(
                "mismatch".to_string(),
                Some(E_SIG_OBJECT_MISMATCH.to_string())
            )
        );
        assert_eq!(got.iter().filter(|s| s.1.is_some()).count(), 1);
        assert_eq!(r.fsck().unwrap_err().code, E_SIG_OBJECT_MISMATCH);
    };
    forged(&mut fk, 3);
    forged(&mut ff, 6);

    // the pure check accepts exactly the ids its context names; crypto does not run again
    let fork_ctx = read_check_context(&ff.read(), &g).unwrap();
    assert_eq!(
        statement_of(&check_signature(&g, &fork_ctx, None)),
        ("match", None)
    );
    let with = |source_repos: Vec<RepoId>| CheckContext {
        source_repos,
        ..fork_ctx.clone()
    };
    for (repos, want) in [
        (vec![], ("mismatch", Some(E_SIG_OBJECT_MISMATCH))),
        (vec![OTHER_REPO], ("mismatch", Some(E_SIG_OBJECT_MISMATCH))),
        (vec![fork_id], ("mismatch", Some(E_SIG_OBJECT_MISMATCH))),
        (vec![OTHER_REPO, source_id], ("match", None)),
    ] {
        assert_eq!(
            statement_of(&check_signature(&g, &with(repos.clone()), None)),
            want,
            "{repos:?}"
        );
    }
    // a clone of a project that was never forked, holding another id's signature, gets no
    // fallback
    put(&mut cl, |x| {
        stored_approval(x, &l2, SOURCE_REPO, Key::Ed, 4)
    });
    assert_eq!(
        statements(&cl, &l2),
        vec![(
            "mismatch".to_string(),
            Some(E_SIG_OBJECT_MISMATCH.to_string())
        )]
    );
    for r in [cl, fk, fc, ff, src] {
        r.close().unwrap();
    }
}

#[test]
fn sig6_policy_and_keybind_are_not_stored_signing_subjects() {
    // a policy landing has no approval statement: bytes that claim one do not cover it
    let mut policy = landing(Assurance::Local);
    let local_stmt = statement(
        StatementSource::Approval(&policy),
        Some(REPO),
        &Key::Ed.fp(),
    )
    .unwrap();
    policy.assurance = Assurance::Policy;
    assert!(
        statement(
            StatementSource::Approval(&policy),
            Some(REPO),
            &Key::Ed.fp()
        )
        .is_err()
    );
    let c = ctx(landing_subject(policy.clone()));
    let g = signature(LANDING_HASH, "rdy-approval-v1", local_stmt, Key::Ed, 0);
    let got = check_signature(
        &g,
        &c,
        Some(&trust(&format!("human:founder {}", Key::Ed.line()))),
    );
    assert_eq!(
        (got.signed.as_str(), statement_of(&got), got.trust.as_str()),
        (
            "valid",
            ("mismatch", Some(E_SIG_OBJECT_MISMATCH)),
            "unknown"
        )
    );
    // `assurance: policy` is outside the grammar
    let text = String::from_utf8(g.statement.clone())
        .unwrap()
        .replace("assurance: local", "assurance: policy");
    let g = signature(
        LANDING_HASH,
        "rdy-approval-v1",
        text.into_bytes(),
        Key::Ed,
        0,
    );
    assert_eq!(
        statement_of(&check_signature(&g, &c, None)),
        ("mismatch", Some(E_SIG_INVALID))
    );

    // a valid key binding proof covers no landing or tag
    let kb = statement(
        StatementSource::Keybind {
            account: "acct-1",
            challenge: "nonce-1",
        },
        None,
        &Key::Ed.fp(),
    )
    .unwrap();
    let rec = landing(Assurance::Local);
    for (subject, hash) in [
        (landing_subject(rec.clone()), LANDING_HASH),
        (tag_subject(None), TAG_HASH),
    ] {
        let g = signature(hash, "rdy-keybind-v1", kb.clone(), Key::Ed, 0);
        let got = check_signature(&g, &ctx(subject), None);
        assert_eq!(
            (got.kind.as_deref(), got.signed.as_str(), statement_of(&got)),
            (
                Some("keybind"),
                "valid",
                ("mismatch", Some(E_SIG_OBJECT_MISMATCH))
            )
        );
    }
    // in another namespace, the proof is not even valid
    let g = signature(LANDING_HASH, "rdy-approval-v1", kb.clone(), Key::Ed, 0);
    assert_eq!(
        statement_of(&check_signature(
            &g,
            &ctx(landing_subject(rec.clone())),
            None
        )),
        ("mismatch", Some(E_SIG_INVALID))
    );
    // the source form must fit the subject: no approval on a tag, no tag on a landing
    let on_tag = Signature {
        subject: TAG_HASH,
        ..approval_sig(&rec, Key::Ed)
    };
    let on_landing = Signature {
        subject: LANDING_HASH,
        ..tag_sig(Key::Ed)
    };
    let tag_relay_on_landing = Signature {
        subject: LANDING_HASH,
        ..relay_tag_sig(Key::Ed)
    };
    let tc = ctx(tag_subject(Some((ANSWER_HASH, tag_answer(TAG_HASH)))));
    let lc = ctx(landing_subject(landing(Assurance::Relayed)));
    for (g, c) in [
        (&on_tag, &tc),
        (&on_landing, &lc),
        (&tag_relay_on_landing, &lc),
    ] {
        assert_eq!(
            statement_of(&check_signature(g, c, None)),
            ("mismatch", Some(E_SIG_OBJECT_MISMATCH))
        );
    }

    // the store never holds a key binding proof
    let d = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(d.path(), "rdy-standard").unwrap();
    let l = land_one(&mut r, "Doc");
    let g = signature(l, "rdy-keybind-v1", kb, Key::Ed, 0);
    let e = RdyError::from(r.store_mut().put_signature(&g).unwrap_err());
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "key binding signatures are not stored")
    );
}

#[test]
fn sig6_hostile_trust_and_report_sizes_are_bounded() {
    let limit = |r: Result<AllowedSigners, RdyError>, what: &str| {
        let e = r.err().unwrap_or_else(|| panic!("{what} is refused"));
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                format!("signing input exceeds {what}").as_str(),
                "reduce the input to the stated limit"
            )
        );
    };
    let line = Key::Ed.line();
    // the file: 1048576 bytes at most
    let mut comment = vec![b'#'; 1023];
    comment.push(b'\n');
    let mut file = comment.repeat(1024);
    assert_eq!(file.len(), 1_048_576);
    assert!(AllowedSigners::parse(&file).unwrap().is_empty());
    file.push(b'#');
    limit(AllowedSigners::parse(&file), "1048576 bytes");
    // one line: 16384 bytes at most, its LF not counted
    let mut long = vec![b'#'; 16_384];
    long.push(b'\n');
    assert!(AllowedSigners::parse(&long).is_ok());
    let mut longer = vec![b'#'; 16_385];
    longer.push(b'\n');
    limit(AllowedSigners::parse(&longer), "16384 bytes");
    // entries: 4096 at most; comments and blank lines are not entries
    let entries = |n: usize| format!("# c\n\n{}", format!("p {line}\n").repeat(n));
    assert_eq!(
        AllowedSigners::parse(entries(4096).as_bytes())
            .unwrap()
            .len(),
        4096
    );
    limit(
        AllowedSigners::parse(entries(4097).as_bytes()),
        "4096 entries",
    );
    // one pattern: 1024 bytes at most, a negation's `!` included
    let p = "a".repeat(1024);
    assert!(AllowedSigners::parse(format!("{p} {line}").as_bytes()).is_ok());
    limit(
        AllowedSigners::parse(format!("{p}a {line}").as_bytes()),
        "1024 bytes",
    );
    limit(
        AllowedSigners::parse(format!("!{p} {line}").as_bytes()),
        "1024 bytes",
    );
    limit(
        AllowedSigners::parse(format!("p namespaces=\"{p}a\" {line}").as_bytes()),
        "1024 bytes",
    );
    // patterns per entry: principals and namespaces together, 128 at most
    let list = |n: usize| vec!["p"; n].join(",");
    let entry = |principals: usize, namespaces: usize| {
        format!(
            "{} namespaces=\"{}\" {line}",
            list(principals),
            list(namespaces)
        )
    };
    assert!(AllowedSigners::parse(entry(100, 28).as_bytes()).is_ok());
    limit(
        AllowedSigners::parse(entry(100, 29).as_bytes()),
        "128 patterns",
    );
    limit(
        AllowedSigners::parse(entry(129, 0).replace(" namespaces=\"\"", "").as_bytes()),
        "128 patterns",
    );
    // hostile patterns match without recursion or blow-up
    let star = format!("{}b", "*a".repeat(500));
    let file = trust(&format!("{star},{} {line}", "?".repeat(1024)));
    let rec = landing(Assurance::Local);
    let mut long_actor = rec.clone();
    long_actor.approved_by.actor = "a".repeat(4000);
    let g = approval_sig(&long_actor, Key::Ed);
    let got = check_signature(&g, &ctx(landing_subject(long_actor)), Some(&file));
    assert_eq!(
        (got.statement.as_str(), got.trust.as_str()),
        ("match", "unknown")
    );
    // non-UTF-8 and control bytes refuse the line, never panic
    let e = AllowedSigners::parse(b"\xff\xfe ssh-ed25519 AAAA\n").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);

    // the configured file: unset is no trust; unreadable is E_IO; too large is E_LIMIT
    assert!(read_allowed_signers(None).unwrap().is_none());
    let d = tempfile::tempdir().unwrap();
    let e = read_allowed_signers(Some(&d.path().join("absent"))).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_IO,
            "cannot read signing.allowed_signers",
            "check the configured path and its permissions"
        )
    );
    let big = d.path().join("big");
    std::fs::write(&big, vec![b'#'; 1_048_577]).unwrap();
    limit(
        read_allowed_signers(Some(&big)).map(Option::unwrap),
        "1048576 bytes",
    );
    let ok = d.path().join("ok");
    std::fs::write(&ok, format!("human:founder {line}\n")).unwrap();
    assert_eq!(read_allowed_signers(Some(&ok)).unwrap().unwrap().len(), 1);

    // one read response holds at most 4096 signatures of one subject
    std::fs::create_dir_all(d.path().join("p")).unwrap();
    let mut r = Rdy::init(&d.path().join("p"), "rdy-standard").unwrap();
    let l = land_one(&mut r, "Doc");
    let junk = |i: i64| Signature {
        subject: l,
        namespace: "rdy-approval-v1".into(),
        statement: b"x".to_vec(),
        sig: b"y".to_vec(),
        signed_by: prov("agent:s", None, i),
    };
    let first: Vec<Signature> = (0..4096).map(junk).collect();
    let frames = rdy_store::signature::frames(&first).unwrap();
    r.store_mut().write(frames).unwrap();
    r.seal_open().unwrap();
    let got = signature_checks(&r.read(), &l, None).unwrap();
    assert_eq!(got.len(), 4096);
    assert!(got.iter().all(|c| c.code.as_deref() == Some(E_SIG_INVALID)));
    put(&mut r, |_| junk(4096));
    let e = signature_checks(&r.read(), &l, None).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "signing input exceeds 4096 signatures per subject",
            "reduce the input to the stated limit"
        )
    );
    // a subject with no signatures, or no record at all, answers an empty list
    assert!(signature_checks(&r.read(), &h(9), None).unwrap().is_empty());
}

#[test]
fn sig6_fsck_checks_all_signatures_without_agent_or_trust_file() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(d.path(), "rdy-standard").unwrap();
    let l = land_one(&mut r, "Doc");
    let repo = r.read().repo_id().unwrap();
    let unsigned = serde_json::to_value(r.fsck().unwrap()).unwrap();
    assert!(unsigned.get("signatures").is_none(), "{unsigned}");

    // a valid signature by a key no file trusts passes: trust is advice, not an fsck issue
    put(&mut r, |x| stored_approval(x, &l, repo, Key::Ed, 1));
    let f = r.fsck().unwrap();
    assert_eq!(f.signatures, 1);
    assert_eq!(serde_json::to_value(&f).unwrap()["signatures"], 1);

    // an invalid and a mismatched signature fail, in subject then hash order
    let bad = put(&mut r, |x| Signature {
        sig: b"damaged".to_vec(),
        ..stored_approval(x, &l, repo, Key::Ed, 2)
    });
    let wrong = put(&mut r, |x| stored_approval(x, &l, OTHER_REPO, Key::Ed, 3));
    let mut want = [
        (bad, E_SIG_INVALID, format!("signature {bad} is invalid")),
        (
            wrong,
            E_SIG_OBJECT_MISMATCH,
            format!("signature {wrong} does not match its subject"),
        ),
    ];
    want.sort_by_key(|w| w.0);
    let e = r.fsck().unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (want[0].1, want[0].2.as_str())
    );
    let got: Vec<(&str, &str, bool, Severity)> = e
        .issues
        .iter()
        .map(|i| (i.code, i.message.as_str(), i.block.is_none(), i.severity))
        .collect();
    assert_eq!(
        got,
        want.iter()
            .map(|w| (w.1, w.2.as_str(), true, Severity::Error))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        e.issues
            .iter()
            .find(|i| i.code == E_SIG_INVALID)
            .unwrap()
            .fix,
        "check the source of this signature; it may be damaged or forged"
    );
    assert_eq!(
        e.issues
            .iter()
            .find(|i| i.code == E_SIG_OBJECT_MISMATCH)
            .unwrap()
            .fix,
        "run rdy sign for this subject, or refuse the record"
    );

    // past 1000 issues: the first 999, then one E_LIMIT issue that counts the rest
    let l2 = land_one(&mut r, "Doc 2");
    let junk: Vec<Signature> = (0..1001)
        .map(|i| Signature {
            subject: l2,
            namespace: "rdy-approval-v1".into(),
            statement: b"x".to_vec(),
            sig: b"y".to_vec(),
            signed_by: prov("agent:s", None, i),
        })
        .collect();
    let frames = rdy_store::signature::frames(&junk).unwrap();
    r.store_mut().write(frames).unwrap();
    r.seal_open().unwrap();
    let e = r.fsck().unwrap_err();
    assert_eq!(e.issues.len(), 1000);
    let last = e.issues.last().unwrap();
    assert_eq!(
        (
            last.code,
            last.message.as_str(),
            last.fix.as_str(),
            last.severity
        ),
        (
            E_LIMIT,
            "fsck omitted 4 signature issues",
            "fix the reported signatures, then run rdy fsck again",
            Severity::Error
        ),
        "1003 issues: 999 shown, the thousandth and three more omitted"
    );
    assert!(e.issues[..999].iter().all(|i| i.code != E_LIMIT));
    let first = &e.issues[0];
    assert_eq!(
        (e.code, e.message.as_str()),
        (first.code, first.message.as_str())
    );
}

/// ruling task-13: in a holder only, a signature that is valid over its subject but whose
/// statement names a repo id other than the holder's own is `foreign_repo`, with that repo id:
/// no error code, no trust and no warning. A signature that fails its own checks stays as it
/// is, and a project keeps D-1ZTBP's rule.
#[test]
fn ha13_a_holder_shows_another_projects_signature_as_foreign() {
    let rec = landing(Assurance::Local);
    let subjects = [
        (
            ctx(tag_subject(Some((ANSWER_HASH, tag_answer(TAG_HASH))))),
            vec![tag_sig(Key::Ed), relay_tag_sig(Key::Ed)],
        ),
        (
            ctx(landing_subject(rec.clone())),
            vec![approval_sig(&rec, Key::Ed)],
        ),
    ];
    for (base, sigs) in subjects {
        let holder = CheckContext {
            current_repo: OTHER_REPO,
            holder: true,
            ..base.clone()
        };
        for g in &sigs {
            // the holder of its own repo verifies it as the project does
            let own = CheckContext {
                holder: true,
                ..base.clone()
            };
            assert_eq!(
                statement_of(&check_signature(g, &own, None)),
                ("match", None)
            );
            // a holder of another repo id: foreign, naming the signature's repo
            let c = check_signature(g, &holder, None);
            assert_eq!(
                (
                    c.signed.as_str(),
                    c.statement.as_str(),
                    c.trust.as_str(),
                    c.code.as_deref(),
                    c.foreign_repo.as_deref(),
                ),
                (
                    "valid",
                    "foreign_repo",
                    "unknown",
                    None,
                    Some(REPO.to_string().as_str())
                )
            );
            assert!(c.warnings.is_empty());
            assert_eq!(json_of(&c)["foreign_repo"], REPO.to_string());
            // a project of another repo id: a mismatch, as before
            let project = CheckContext {
                holder: false,
                ..holder.clone()
            };
            let c = check_signature(g, &project, None);
            assert_eq!(statement_of(&c), ("mismatch", Some(E_SIG_OBJECT_MISMATCH)));
            assert!(json_of(&c).get("foreign_repo").is_none());
            // another subject: a mismatch in a holder too
            let mut other = g.clone();
            other.subject = h(99);
            assert_eq!(
                statement_of(&check_signature(&other, &holder, None)),
                ("mismatch", Some(E_SIG_OBJECT_MISMATCH))
            );
            // a signature that does not verify: invalid
            let mut bad = g.clone();
            let last = bad.sig.len() - 1;
            bad.sig[last] ^= 1;
            let c = check_signature(&bad, &holder, None);
            assert_eq!(
                (
                    c.signed.as_str(),
                    c.code.as_deref(),
                    c.foreign_repo.as_deref()
                ),
                ("invalid", Some(E_SIG_INVALID), None)
            );
        }
    }
}
