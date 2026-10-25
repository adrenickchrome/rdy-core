//! ok2: a block residue's content hash is checked by the one derivation its own encoding names
//! (D-TX3KH, D-WWAK7, D-69AY3). A residue of encoding 1, the one every new removal writes, holds
//! the content-2 hash of the removed block; a residue of encoding 0, which an rdy before
//! content-2 wrote, holds the content-1 hash (the order key kept). The checked preparation, the
//! landing's apply and the open's authorization bootstrap compare the whole residue, its content
//! hash included, so no residue can give a removed block an invented identity.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::encoding::legacy_content_1_hash;
use rdy_model::redaction::{FramePlace, RedactTarget, RedactionResidue};
use rdy_model::*;
use rdy_store::kinds::BLOCK;
use rdy_store::{LandRequest, Store, StoreError};
use redaction_fixture::{
    Draft, FixtureOptions, Proof, RequestSpec, authorized_batch_fixture, build_request, relink,
    relink_in,
};
use std::collections::BTreeMap;
use std::path::Path;

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

fn approver() -> Provenance {
    Provenance {
        actor: "human:alice".into(),
        run: None,
        role: Role::Approver,
        at_ms: 50,
    }
}

/// The secret block every case removes.
fn secret() -> Block {
    block("b-00001", "canary residue secret")
}

/// A store at `path` whose view `work` creates the secret block.
fn seeded(path: &Path) -> Store {
    let mut s = Store::create(path).unwrap();
    s.commit_to_view("work", change("work", vec![Op::Create { block: secret() }]))
        .unwrap();
    s
}

/// The content hash an invented residue claims.
fn invented() -> Hash {
    Hash::of(b"an invented content identity")
}

/// Sets every block residue's content hash of `d` to `h`.
fn set_content(d: &mut Draft, h: Hash) {
    for r in &mut d.residues {
        if let RedactionResidue::Block(b) = r {
            b.content_hash = h;
        }
    }
}

/// The place of the draft's block span.
fn block_place(d: &Draft) -> FramePlace {
    d.request
        .manifest
        .spans
        .iter()
        .find(|s| s.kind == BLOCK)
        .unwrap()
        .place
}

fn corrupt_at(e: &StoreError, at: FramePlace) -> bool {
    matches!(e, StoreError::RedactionCorrupt { segment, offset }
        if *segment == at.segment && *offset == at.offset)
}

/// The redacted canon block of the store at `path`, if the secret block is removed.
fn removed_identity(path: &Path) -> Option<Hash> {
    let s = Store::open(path).unwrap();
    let canon = s.canon_state().unwrap();
    let blocks = s.blocks(&canon).unwrap();
    let b = blocks.get(&secret().id)?;
    b.is_redacted().then(|| b.content_hash())
}

/// The prepared-only fixture whose draft `hook` alters, landed through `land_atomic`: its
/// refusal, if any.
fn land_prepared(path: &Path, hook: impl Fn(&mut Draft) + 'static, encoding: u16) -> StoreError {
    drop(seeded(path));
    let opts = FixtureOptions {
        on_draft: Some(Box::new(move |_, d| hook(d))),
        residue_encoding: encoding,
        ..FixtureOptions::prepared_only()
    };
    authorized_batch_fixture(
        path,
        &[RequestSpec::new("work", vec![version(&secret())])],
        &opts,
    );
    let mut s = Store::open(path).unwrap();
    let p = s.propose("work", "Remove content", writer()).unwrap();
    let end = s.log_end();
    let e = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: None,
            approver: approver(),
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap_err();
    assert_eq!(s.log_end(), end, "a refused landing writes nothing");
    e
}

/// The approved fixture whose draft `hook` alters: the open's refusal, if any. The landing and
/// its apply are written raw, so only the open's bootstrap checks the residue.
fn open_landed(
    path: &Path,
    hook: impl Fn(&mut Draft) + 'static,
    encoding: u16,
) -> Result<Store, StoreError> {
    drop(seeded(path));
    let opts = FixtureOptions {
        on_draft: Some(Box::new(move |_, d| hook(d))),
        residue_encoding: encoding,
        ..FixtureOptions::approved(Proof::local_human())
    };
    authorized_batch_fixture(
        path,
        &[RequestSpec::new("work", vec![version(&secret())])],
        &opts,
    );
    Store::open(path)
}

#[test]
fn ok2_new_residue_with_a_fabricated_content_hash_is_refused() {
    let dir = tempfile::tempdir().unwrap();

    // the reviewer's case: a request of this rdy whose block residue claims an invented content
    // hash, relinked, is refused at preparation
    let path = dir.path().join("prepare");
    let mut s = seeded(&path);
    let d = build_request(&s, &RequestSpec::new("work", vec![version(&secret())]));
    let mut x = d.clone();
    set_content(&mut x, invented());
    relink(&mut x.request, &x.residues);
    let end = s.log_end();
    let e = s
        .prepare_redaction(&x.carrier, &x.request, &x.residues)
        .unwrap_err();
    assert!(corrupt_at(&e, block_place(&x)), "{e:?}");
    assert_eq!(s.log_end(), end, "a refused preparation writes nothing");
    // the request with the true residue still prepares, commits, lands and removes the block,
    // which keeps its true content-2 identity after removal, reopen and reindex
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    s.commit_redaction(p).unwrap();
    let prop = s.propose("work", "Remove content", writer()).unwrap();
    s.land_atomic(LandRequest {
        proposal: prop,
        accept: None,
        approver: approver(),
        alt: None,
        relay: None,
        evidence: None,
    })
    .unwrap();
    drop(s);
    let truth = secret().content_hash();
    assert_eq!(removed_identity(&path), Some(truth));
    let mut s = Store::open(&path).unwrap();
    s.reindex().unwrap();
    drop(s);
    assert_eq!(removed_identity(&path), Some(truth), "after reindex");

    // recorded raw, it is refused at the landing's apply
    let e = land_prepared(
        &dir.path().join("apply"),
        |d| {
            set_content(d, invented());
            relink(&mut d.request, &d.residues);
        },
        1,
    );
    assert!(matches!(e, StoreError::RedactionCorrupt { .. }), "{e:?}");

    // and with its apply written raw, the open's authorization bootstrap refuses the store
    let path = dir.path().join("bootstrap");
    let e = open_landed(
        &path,
        |d| {
            set_content(d, invented());
            relink(&mut d.request, &d.residues);
        },
        1,
    )
    .err()
    .expect("the open refuses the invented residue");
    assert!(matches!(e, StoreError::RedactionCorrupt { .. }), "{e:?}");
    // the control: the same raw landing with the true residue opens, and the block is removed
    // with its true identity
    let path = dir.path().join("control");
    let s = open_landed(&path, |_| {}, 1).unwrap();
    drop(s);
    assert_eq!(removed_identity(&path), Some(secret().content_hash()));
}

/// The content-1 residue an rdy before content-2 wrote for the secret block, as `hook` sets it.
fn as_content_1(d: &mut Draft) {
    set_content(d, legacy_content_1_hash(&secret()));
    relink_in(&mut d.request, &d.residues, 0);
}

#[test]
fn ok2_old_residue_with_a_fabricated_content_hash_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    // the control: an encoding-0 residue with the content-1 hash it recorded opens, and the
    // removed block returns that recorded hash
    let path = dir.path().join("control");
    let s = open_landed(&path, as_content_1, 0).unwrap();
    drop(s);
    let old = legacy_content_1_hash(&secret());
    assert_ne!(old, secret().content_hash());
    assert_eq!(removed_identity(&path), Some(old));
    let mut s = Store::open(&path).unwrap();
    s.reindex().unwrap();
    drop(s);
    assert_eq!(removed_identity(&path), Some(old), "after reindex");
    // an encoding-0 residue's request applied by this rdy checks against content-1 too
    let path = dir.path().join("apply-control");
    drop(seeded(&path));
    let opts = FixtureOptions {
        on_draft: Some(Box::new(|_, d| as_content_1(d))),
        residue_encoding: 0,
        ..FixtureOptions::prepared_only()
    };
    authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![version(&secret())])],
        &opts,
    );
    let mut s = Store::open(&path).unwrap();
    let p = s.propose("work", "Remove content", writer()).unwrap();
    s.land_atomic(LandRequest {
        proposal: p,
        accept: None,
        approver: approver(),
        alt: None,
        relay: None,
        evidence: None,
    })
    .unwrap();
    drop(s);
    assert_eq!(removed_identity(&path), Some(old));

    // the same old residue with an invented content hash is refused at apply and at the open
    let fabricated = |d: &mut Draft| {
        set_content(d, invented());
        relink_in(&mut d.request, &d.residues, 0);
    };
    let e = land_prepared(&dir.path().join("apply"), fabricated, 0);
    assert!(matches!(e, StoreError::RedactionCorrupt { .. }), "{e:?}");
    let e = open_landed(&dir.path().join("bootstrap"), fabricated, 0)
        .err()
        .expect("the open refuses the invented old residue");
    assert!(matches!(e, StoreError::RedactionCorrupt { .. }), "{e:?}");
}

#[test]
fn ok2_residue_encoding_picks_one_derivation() {
    let dir = tempfile::tempdir().unwrap();
    // an encoding-1 residue that holds the content-1 hash
    let one_with_1 = |d: &mut Draft| {
        set_content(d, legacy_content_1_hash(&secret()));
        relink_in(&mut d.request, &d.residues, 1);
    };
    // an encoding-0 residue that holds the content-2 hash
    let zero_with_2 = |d: &mut Draft| {
        set_content(d, secret().content_hash());
        relink_in(&mut d.request, &d.residues, 0);
    };
    let e = land_prepared(&dir.path().join("apply-1"), one_with_1, 1);
    assert!(matches!(e, StoreError::RedactionCorrupt { .. }), "{e:?}");
    let e = land_prepared(&dir.path().join("apply-0"), zero_with_2, 0);
    assert!(matches!(e, StoreError::RedactionCorrupt { .. }), "{e:?}");
    for (name, enc) in [("open-1", 1), ("open-0", 0)] {
        let hook: Box<dyn Fn(&mut Draft)> = if enc == 1 {
            Box::new(one_with_1)
        } else {
            Box::new(zero_with_2)
        };
        let e = open_landed(&dir.path().join(name), hook, enc)
            .err()
            .unwrap_or_else(|| panic!("{name}: the open refuses the other derivation"));
        assert!(
            matches!(e, StoreError::RedactionCorrupt { .. }),
            "{name}: {e:?}"
        );
    }
    // a preparation takes no encoding-0 residue: every supplied residue is written and checked
    // in the latest encoding, so a content-1 residue is refused there
    let path = dir.path().join("prepare");
    let mut s = seeded(&path);
    let mut d = build_request(&s, &RequestSpec::new("work", vec![version(&secret())]));
    set_content(&mut d, legacy_content_1_hash(&secret()));
    relink(&mut d.request, &d.residues);
    let e = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap_err();
    assert!(corrupt_at(&e, block_place(&d)), "{e:?}");
}
