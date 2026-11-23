//! Counted fork origins in a holder (D-HX6BX, D-QHHFK, D-CDQF7): only the holder's creating
//! batch supplies the origins its lineage counts, for signatures and baselines alike. A
//! signature made in a counted ancestor is checked as the project checks it, with the same
//! trust results. A signature of any other repo stays `foreign_repo`, and a later
//! `meta/origin` widens nothing. The holders here hold a real project's objects.
use crate::common;

use ed25519_dalek::Signer as _;
use rdy_engine::allowed_signers::AllowedSigners;
use rdy_engine::publication::{self, IMPLICIT_ORG};
use rdy_engine::signature_check::{SignatureCheck, read_check_context, signature_checks};
use rdy_engine::signing_crypto::blob_fingerprint;
use rdy_engine::*;
use rdy_model::signature::{Signature, StatementSource, parse_statement, statement};
use rdy_model::*;
use rdy_store::kinds;
use rdy_store::meta::Origin;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use ssh_encoding::Encode;
use ssh_encoding::base64::{Base64, Encoding};
use ssh_key::public::{Ed25519PublicKey, KeyData};
use ssh_key::{Algorithm, HashAlg, SshSig};
use std::path::Path;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};
/// The intermediate fork the holder was forked from: its immediate source.
const SOURCE: RepoId = RepoId([0x33; 16]);
/// The forked holder's own repo id.
const FORK: RepoId = RepoId([0x44; 16]);
/// A repo outside every lineage here.
const OTHER: RepoId = RepoId([0x55; 16]);
/// The repo id of the ordinary holder.
const PLAIN: RepoId = RepoId([0x66; 16]);

// ---------------------------------------------------------------------------------------------
// the source project

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: Value) {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "hfp1".into(),
        },
    )
    .unwrap();
}

/// A project with one landed doc tagged `v1.0.0`. Returns it, the landing and the tag.
fn source(dir: &Path) -> (Rdy, Hash, Hash) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    );
    let p = r
        .propose(&writer(), "work", "First", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut r, &founder(), &p);
    let landing: Hash = r.land(&founder(), &p.id).unwrap().parse().unwrap();
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let t = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap()
        .1;
    (r, landing, t)
}

/// The kinds a copy carries into a holder: what a tag's history reaches.
const COPIED: [u8; 10] = [
    kinds::BLOCK,
    kinds::ASSESSMENT,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::SNAPSHOT_DELTA,
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

// ---------------------------------------------------------------------------------------------
// signatures (a software ed25519 signer from a fixed seed, test only)

fn signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x61; 32])
}

fn key_data() -> KeyData {
    KeyData::Ed25519(Ed25519PublicKey(signing_key().verifying_key().to_bytes()))
}

fn key_blob() -> Vec<u8> {
    let mut blob = vec![];
    key_data().encode(&mut blob).unwrap();
    blob
}

fn sshsig(ns: &str, msg: &[u8]) -> Vec<u8> {
    let data = SshSig::signed_data(ns, HashAlg::Sha512, msg).unwrap();
    let sig = ssh_key::Signature::new(
        Algorithm::Ed25519,
        signing_key().sign(&data).to_bytes().to_vec(),
    )
    .unwrap();
    let mut out = vec![];
    SshSig::new(key_data(), ns, HashAlg::Sha512, sig)
        .unwrap()
        .encode(&mut out)
        .unwrap();
    out
}

/// The signature of `subject` over `source` as made in repo `repo`.
fn signed(subject: Hash, source: StatementSource<'_>, repo: RepoId) -> Signature {
    let stmt = statement(source, Some(repo), &blob_fingerprint(&key_blob())).unwrap();
    let ns = parse_statement(&stmt).unwrap().namespace().to_string();
    Signature {
        subject,
        namespace: ns.clone(),
        sig: sshsig(&ns, &stmt),
        statement: stmt,
        signed_by: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 7,
        },
    }
}

/// The founder's key, trusted for every namespace.
fn trust() -> AllowedSigners {
    let line = format!(
        "human:founder ssh-ed25519 {}\n",
        Base64::encode_string(&key_blob())
    );
    AllowedSigners::parse(line.as_bytes()).unwrap()
}

/// The verdict of a check: statement, trust, code, foreign repo and warning codes.
type Verdict = (String, String, Option<String>, Option<String>, Vec<String>);

fn verdict(c: &SignatureCheck) -> Verdict {
    (
        c.statement.clone(),
        c.trust.clone(),
        c.code.clone(),
        c.foreign_repo.clone(),
        c.warnings.iter().map(|w| w.code.clone()).collect(),
    )
}

/// The verdicts of `subject`'s signatures in `h`, each keyed by the repo its statement names,
/// in repo order.
fn verdicts(h: &Rdy, subject: &Hash, trust: Option<&AllowedSigners>) -> Vec<(String, Verdict)> {
    let st = h.read();
    let mut out: Vec<(String, Verdict)> = signature_checks(&st, subject, trust)
        .unwrap()
        .iter()
        .zip(h.store().reader().signatures_of(subject).unwrap())
        .map(|(c, id)| {
            let g = h.store().reader().signature(&id).unwrap();
            let p = parse_statement(&g.statement).unwrap();
            (p.field("repo").unwrap().to_string(), verdict(c))
        })
        .collect();
    out.sort();
    out
}

fn matched(trusted: bool) -> Verdict {
    if trusted {
        ("match".into(), "trusted".into(), None, None, vec![])
    } else {
        (
            "match".into(),
            "unknown".into(),
            None,
            None,
            vec!["W_SIG_UNKNOWN_KEY".into()],
        )
    }
}

fn foreign(repo: RepoId) -> Verdict {
    (
        "foreign_repo".into(),
        "unknown".into(),
        None,
        Some(repo.to_string()),
        vec![],
    )
}

// ---------------------------------------------------------------------------------------------
// holders

/// A holder at `root` made by the store with `origins` (none: an ordinary holder), holding
/// every copied object of `src`, its `v1.0.0` tag ref and `sigs`; closed and opened again.
fn holder(root: &Path, repo: RepoId, origins: &[Origin], src: &Rdy, sigs: &[Signature]) -> Rdy {
    let t = src.store().get_ref("tag/v1.0.0").unwrap().unwrap();
    std::fs::create_dir_all(root.join(".rdy")).unwrap();
    let s = rdy_store::Store::create_holder_with_origins(&root.join(".rdy/store"), repo, origins)
        .unwrap();
    drop(s);
    let mut h = Rdy::open_holder(root).unwrap();
    h.store_mut().write(objects(src)).unwrap();
    h.store_mut()
        .set_refs(vec![("tag/v1.0.0".into(), t)])
        .unwrap();
    for g in sigs {
        h.store_mut().put_signature(g).unwrap();
    }
    h.seal_open().unwrap();
    h.close().unwrap();
    Rdy::open_holder(root).unwrap()
}

/// Writes a later `meta/origin` into the closed holder at `root` through the store's own origin
/// batch, and opens it again.
fn later_origin(h: Rdy, root: &Path, o: &Origin) -> Rdy {
    h.close().unwrap();
    let mut s = rdy_store::Store::open(&root.join(".rdy/store")).unwrap();
    s.put_origin(o, None).unwrap();
    drop(s);
    Rdy::open_holder(root).unwrap()
}

fn sorted(mut ids: Vec<RepoId>) -> Vec<RepoId> {
    ids.sort();
    ids
}

#[test]
fn hfp1_ancestor_signatures_use_counted_origins() {
    let dir = tempfile::tempdir().unwrap();
    let (src, landing, t) = source(&dir.path().join("src"));
    let ancestor = src.read().repo_id().unwrap();
    let rec = src.store().reader().landing(&landing).unwrap();
    let tag = src.store().read_tag(&t).unwrap();
    // the landing signed in the ancestor, in the fork itself and in an unrelated repo; the tag
    // signed in the immediate source and in the unrelated repo
    let sigs = [
        signed(landing, StatementSource::Approval(&rec), ancestor),
        signed(landing, StatementSource::Approval(&rec), FORK),
        signed(landing, StatementSource::Approval(&rec), OTHER),
        signed(t, StatementSource::Tag { hash: t, tag: &tag }, SOURCE),
        signed(t, StatementSource::Tag { hash: t, tag: &tag }, OTHER),
    ];
    // the fork's creation origins: the source's own fork origin, then the fork's
    let origins = [
        Origin {
            fork: true,
            repo_id: ancestor,
            tag: None,
            landing: None,
        },
        Origin {
            fork: true,
            repo_id: SOURCE,
            tag: Some("v1.0.0".into()),
            landing: Some(landing),
        },
    ];
    let root = dir.path().join("fork");
    let mut h = holder(&root, FORK, &origins, &src, &sigs);
    let lineage = sorted(vec![ancestor, SOURCE]);
    let trusted = trust();

    // the counted origins name the lineage a signature may use; the holder rules still apply
    let check = |h: &Rdy| {
        let c = read_check_context(&h.read(), &sigs[0]).unwrap();
        assert_eq!(
            (c.current_repo, c.source_repos.clone(), c.holder),
            (FORK, lineage.clone(), true)
        );
        // normal verification and the existing trust results for each counted repo; the
        // unrelated repo stays foreign, with no trust and no warning
        let mut want_landing = vec![
            (ancestor.to_string(), matched(false)),
            (FORK.to_string(), matched(false)),
            (OTHER.to_string(), foreign(OTHER)),
        ];
        want_landing.sort();
        assert_eq!(verdicts(h, &landing, None), want_landing);
        let mut want_trusted = vec![
            (ancestor.to_string(), matched(true)),
            (FORK.to_string(), matched(true)),
            (OTHER.to_string(), foreign(OTHER)),
        ];
        want_trusted.sort();
        assert_eq!(verdicts(h, &landing, Some(&trusted)), want_trusted);
        let mut want_tag = vec![
            (SOURCE.to_string(), matched(true)),
            (OTHER.to_string(), foreign(OTHER)),
        ];
        want_tag.sort();
        assert_eq!(verdicts(h, &t, Some(&trusted)), want_tag);
        // fsck checks every signature; only the unrelated two are foreign
        let report = h.fsck().unwrap();
        assert_eq!((report.signatures, report.foreign_signatures), (5, 2));
        // the baseline lists the same counted origins, in its sorted form
        let st = h.read();
        let b = publication::make_baseline_at(&st, "v1.0.0", landing, IMPLICIT_ORG).unwrap();
        assert_eq!(b.source().repo_id, FORK);
        assert_eq!(b.source().fork_origins, lineage);
    };
    check(&h);

    // a baseline from the ancestor shares the lineage; one from an unrelated project does not
    let from_ancestor =
        publication::make_baseline(&src.read(), "tag:v1.0.0", IMPLICIT_ORG).unwrap();
    publication::check_baseline_lineage(&h.read(), &from_ancestor).unwrap();
    let (unrelated, _, _) = source(&dir.path().join("unrelated"));
    let outside =
        publication::make_baseline(&unrelated.read(), "tag:v1.0.0", IMPLICIT_ORG).unwrap();
    let e = publication::check_baseline_lineage(&h.read(), &outside).unwrap_err();
    assert_eq!(e.code, "E_PUBLICATION_BASELINE");

    // later origins, one of the unrelated repo and one a repeat of a creation value, neither
    // widen nor erase the lineage
    h = later_origin(
        h,
        &root,
        &Origin {
            fork: true,
            repo_id: OTHER,
            tag: None,
            landing: None,
        },
    );
    h = later_origin(h, &root, &origins[0]);
    assert_eq!(h.read().origin().unwrap(), Some(origins[0].clone()));
    check(&h);
    publication::check_baseline_lineage(&h.read(), &outside).unwrap_err();

    // an ordinary holder that later takes the ancestor's origin counts none: every signature
    // of another repo stays foreign, and its baseline lists no origin
    let plain_root = dir.path().join("plain");
    let plain = holder(&plain_root, PLAIN, &[], &src, &sigs[..1]);
    let plain = later_origin(
        plain,
        &plain_root,
        &Origin {
            fork: true,
            repo_id: ancestor,
            tag: None,
            landing: None,
        },
    );
    let c = read_check_context(&plain.read(), &sigs[0]).unwrap();
    assert_eq!(
        (c.current_repo, c.source_repos, c.holder),
        (PLAIN, vec![], true)
    );
    assert_eq!(
        verdicts(&plain, &landing, Some(&trusted)),
        [(ancestor.to_string(), foreign(ancestor))]
    );
    let report = plain.fsck().unwrap();
    assert_eq!((report.signatures, report.foreign_signatures), (1, 1));
    let b = publication::make_baseline_at(&plain.read(), "v1.0.0", landing, IMPLICIT_ORG).unwrap();
    assert!(b.source().fork_origins.is_empty());
    publication::check_baseline_lineage(&plain.read(), &from_ancestor).unwrap_err();

    for r in [h, plain, src, unrelated] {
        r.close().unwrap();
    }
}
