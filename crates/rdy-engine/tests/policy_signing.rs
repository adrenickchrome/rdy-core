//! ap10 (D-DKAMT; O24): `rdy log --verify` and the verbose fsck report a policy landing as
//! `signed: policy`. The row adds `policy_check: {landing, signed}`: the landing of the policy
//! version and that landing's signature state (`valid`, `invalid` or `no`). Ordinary rows are
//! unchanged.
//!
//! The policy landings come from `Store::prepare_land_by_policy`. The valid signature is made by
//! a test-only software signer from a fixed in-memory seed: no private key is read or committed.

use ed25519_dalek::Signer as _;
use rdy_engine::Rdy;
use rdy_engine::signing_read::{signature_states, verified_lines, verified_log};
use rdy_model::assessment::*;
use rdy_model::signature::{Signature, StatementSource, parse_statement, statement};
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::plan::ProposeRequest;
use serde_json::{Value, json};
use ssh_encoding::Encode;
use ssh_key::public::{Ed25519PublicKey, KeyData};
use ssh_key::{Algorithm, HashAlg, SshSig};
use std::collections::BTreeMap;

// ---- the software signer (test only) ----

const SEED: [u8; 32] = [0x61; 32];

fn dalek() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&SEED)
}

fn key_data() -> KeyData {
    KeyData::Ed25519(Ed25519PublicKey(dalek().verifying_key().to_bytes()))
}

fn fingerprint() -> String {
    let mut blob = vec![];
    key_data().encode(&mut blob).unwrap();
    rdy_engine::signing_crypto::blob_fingerprint(&blob)
}

/// A binary SSHSIG of `msg` in `ns` by the test key.
fn sshsig(ns: &str, msg: &[u8]) -> Vec<u8> {
    let data = SshSig::signed_data(ns, HashAlg::Sha512, msg).unwrap();
    let sig = ssh_key::Signature::new(Algorithm::Ed25519, dalek().sign(&data).to_bytes().to_vec())
        .unwrap();
    let mut out = vec![];
    SshSig::new(key_data(), ns, HashAlg::Sha512, sig)
        .unwrap()
        .encode(&mut out)
        .unwrap();
    out
}

fn by_person() -> Provenance {
    prov("human:alice", Role::Approver)
}

// ---- records ----

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("run-1".into()),
        role,
        at_ms: 3,
    }
}

fn agent() -> Provenance {
    prov("agent:w", Role::Writer)
}

fn block(i: &str, ty: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(i).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: agent(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn propose(s: &mut rdy_store::Store, title: &str) -> Hash {
    let req = ProposeRequest {
        view: "work",
        title,
        summary: Some("edits a claim"),
        by: agent(),
        changes: s.included("work").unwrap().as_ref().clone(),
        alternative: None,
        prev: None,
        rekey: None,
        answer: None,
    };
    let prepared = s.prepare_propose(req).unwrap();
    let v = prepared.version();
    s.commit_propose(prepared).unwrap();
    v
}

fn scores(n: u32) -> AssessmentScores {
    AssessmentScores {
        overall: n,
        grounded: n,
        consistent: n,
        current: n,
        connected: n,
        complete: n,
        governed: n,
    }
}

/// An assessment of `proposal` on `base` that the policy `policy` accepts.
fn assessment(proposal: Hash, base: Hash, policy: PolicyRef, blocks: Vec<BlockId>) -> Assessment {
    Assessment {
        proposal,
        base: Some(base),
        method: 1,
        pack: AssessmentPack {
            name: "rdy-qa".into(),
            version: "1.0.0".into(),
            digest: Hash::of(b"pack digest"),
        },
        config: Hash::of(b"config"),
        policy: Some(AssessedPolicy {
            policy: Some(policy),
            verdict: PolicyVerdict::Accepts {
                rule: "drafts".into(),
            },
        }),
        facts: AssessmentFacts {
            changes: 1,
            consequences: 0,
            plan: AssessmentPlanCounts {
                open: 0,
                closed: 0,
                deferred: 0,
                by_kind: vec![],
            },
            supersedes: vec![],
            reaffirms: vec![],
            reversibility: vec![],
            exceptions: vec![],
        },
        tensions: vec![],
        tensions_omitted: 0,
        suspects: vec![],
        suspects_omitted: 0,
        coverage: AssessmentCoverage {
            pages: vec![],
            corpus: AssessmentScoreChange {
                before: scores(50),
                after: scores(60),
            },
        },
        confidence: AssessmentConfidence {
            level: AssessmentLevel::High,
            reasons: vec![],
        },
        reach: blocks.clone(),
        card: AssessmentCard {
            hash: Hash::of(b"card"),
            facts: vec!["Changes: 1".into()],
        },
        scope: AssessmentScope { proposal, blocks },
        issues: vec![],
    }
}

// ---- the project ----

/// A project with the policy block `ap-00001` landed by a person (`first`), then an agent's edit
/// of a claim landed by the policy (`policy_landing`). With `unlanded`, the policy version is
/// instead a version in a view that no landing carried.
struct Fixture {
    _dir: tempfile::TempDir,
    r: Rdy,
    policy: PolicyRef,
    first: Hash,
    policy_landing: Hash,
}

fn fixture(unlanded: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let s = r.store_mut();
    let p = block("ap-00001", "authority_policy", "The authority policy.");
    let b = block("b-00001", "claim", "A claim.");
    let mut first_ops = vec![Op::Create { block: b.clone() }];
    if !unlanded {
        first_ops.push(Op::Create { block: p.clone() });
    }
    s.commit_to_view("work", change("work", first_ops)).unwrap();
    let v1 = propose(s, "first");
    let first = s.land(&v1, by_person()).unwrap();
    if unlanded {
        s.commit_to_view(
            "side",
            change("side", vec![Op::Create { block: p.clone() }]),
        )
        .unwrap();
    }
    let mut b2 = b.clone();
    b2.body = "A claim, edited.".into();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Update {
                base: b.version_hash(),
                block: b2,
            }],
        ),
    )
    .unwrap();
    let version = propose(s, "second");
    let policy = PolicyRef {
        block: p.id.clone(),
        version: p.version_hash(),
    };
    let held = s
        .put_assessment(&assessment(
            version,
            first,
            policy.clone(),
            vec![b.id.clone()],
        ))
        .unwrap();
    let req = LandRequest {
        proposal: version,
        accept: None,
        approver: agent(),
        alt: None,
        relay: None,
        evidence: None,
        assessment: Some(held),
    };
    let prepared = s.prepare_land_by_policy(req, policy.clone()).unwrap();
    let policy_landing = s.commit_land(prepared, &[]).unwrap().landing;
    r.invalidate();
    r.seal_open().unwrap();
    Fixture {
        _dir: dir,
        r,
        policy,
        first,
        policy_landing,
    }
}

impl Fixture {
    /// The verified row of landing `l`.
    fn row(&self, l: &Hash) -> Value {
        let log = verified_log(&self.r.read(), None, None, None).unwrap();
        find(&log, l)
    }

    /// The verbose fsck state of landing `l`.
    fn state(&self, l: &Hash) -> Value {
        find(&signature_states(&self.r.read()).unwrap(), l)
    }

    /// Stores `g` and publishes it.
    fn put(&mut self, g: &Signature) {
        self.r.store_mut().put_signature(g).unwrap();
        self.r.seal_open().unwrap();
    }

    fn damaged(&self) -> Signature {
        Signature {
            subject: self.first,
            namespace: "rdy-approval-v1".into(),
            statement: b"rdy-statement: 1\n".to_vec(),
            sig: b"damaged".to_vec(),
            signed_by: by_person(),
        }
    }

    /// A valid approval signature of the first landing.
    fn valid(&self) -> Signature {
        let st = self.r.read();
        let rec = st.reader().landing(&self.first).unwrap();
        let stmt = statement(
            StatementSource::Approval(&rec),
            Some(st.repo_id().unwrap()),
            &fingerprint(),
        )
        .unwrap();
        let ns = parse_statement(&stmt).unwrap().namespace();
        Signature {
            subject: self.first,
            namespace: ns.into(),
            sig: sshsig(ns, &stmt),
            statement: stmt,
            signed_by: by_person(),
        }
    }
}

fn find(rows: &Value, landing: &Hash) -> Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|r| r["landing"] == landing.to_string())
        .unwrap_or_else(|| panic!("no row for {landing} in {rows:#}"))
        .clone()
}

#[test]
fn ap10_a_policy_landing_reads_signed_policy() {
    let f = fixture(false);
    let want = json!({"landing": f.first.to_string(), "signed": "no"});
    for row in [f.row(&f.policy_landing), f.state(&f.policy_landing)] {
        assert_eq!(row["signed"], "policy", "{row:#}");
        assert_eq!(row["policy_check"], want, "{row:#}");
        assert_eq!(
            row["signatures"],
            json!([]),
            "a policy landing is not signed"
        );
        assert_eq!(row["tag_signatures"], json!({}));
        assert_eq!(
            row["policy"],
            json!({"block": "ap-00001", "version": f.policy.version.to_string()}),
            "the row names the policy version for the text"
        );
    }
    // the text lines
    let short = |h: &Hash| h.short();
    assert_eq!(
        verified_lines(&f.row(&f.policy_landing)),
        vec![
            "    signed: policy".to_string(),
            format!(
                "    policy ap-00001@{} landed in {}: signed no",
                short(&f.policy.version),
                short(&f.first)
            ),
        ]
    );
}

#[test]
fn ap10_policy_check_names_the_policy_versions_landing_and_its_state() {
    let mut f = fixture(false);
    let check = |f: &Fixture| {
        let (row, state) = (f.row(&f.policy_landing), f.state(&f.policy_landing));
        assert_eq!(row["policy_check"], state["policy_check"]);
        row["policy_check"].clone()
    };
    // the policy version's landing holds no signature
    assert_eq!(
        check(&f),
        json!({"landing": f.first.to_string(), "signed": "no"})
    );
    // it has checks and none is valid
    let bad = f.damaged();
    f.put(&bad);
    assert_eq!(
        check(&f),
        json!({"landing": f.first.to_string(), "signed": "invalid"})
    );
    // one of its checks is valid
    let good = f.valid();
    f.put(&good);
    assert_eq!(
        check(&f),
        json!({"landing": f.first.to_string(), "signed": "valid"})
    );
    // the text names the state
    let lines = verified_lines(&f.row(&f.policy_landing));
    assert_eq!(
        lines[1],
        format!(
            "    policy ap-00001@{} landed in {}: signed valid",
            f.policy.version.short(),
            f.first.short()
        )
    );
    // the policy landing itself still reads policy, and the landing it names keeps its own checks
    assert_eq!(f.row(&f.policy_landing)["signed"], "policy");
    assert_eq!(f.row(&f.first)["signatures"].as_array().unwrap().len(), 2);
}

#[test]
fn ap10_a_policy_version_no_landing_holds_reads_null() {
    let f = fixture(true);
    let want = json!({"landing": null, "signed": "no"});
    for row in [f.row(&f.policy_landing), f.state(&f.policy_landing)] {
        assert_eq!(row["signed"], "policy", "{row:#}");
        assert_eq!(row["policy_check"], want, "{row:#}");
    }
    assert_eq!(
        verified_lines(&f.row(&f.policy_landing)),
        vec![
            "    signed: policy".to_string(),
            format!(
                "    policy ap-00001@{}: no landing holds it",
                f.policy.version.short()
            ),
        ]
    );
}

#[test]
fn ap10_an_ordinary_row_is_unchanged() {
    let f = fixture(false);
    // a guard: a landing by a person reads as before, in the log and in the fsck states
    let plain = serde_json::to_value(f.r.read().log_page(None, None).unwrap()).unwrap();
    let mut want = find(&plain, &f.first);
    want["signatures"] = json!([]);
    want["tag_signatures"] = json!({});
    assert_eq!(f.row(&f.first), want);
    assert_eq!(
        f.state(&f.first),
        json!({"landing": f.first.to_string(), "signatures": [], "tag_signatures": {}})
    );
    assert_eq!(
        verified_lines(&f.row(&f.first)),
        vec!["    signed: no".to_string()]
    );
}
