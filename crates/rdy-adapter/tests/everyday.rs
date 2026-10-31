//! nb3 (D-YXBXH, D-2593P): the adapter carries an entity's `everyday` flag as an ordinary field.
//! It adds no write API and no record shape: the block's encoding, its hash, a take into a hub
//! holder and a captured snapshot all see the boolean as any other field value.

use rdy_adapter::stream::ObjectKey;
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{Block, BlockId, Hash, StoreHandle, encode_block, hash_bytes};
use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::{FieldValue, Role};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;

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
            message: "nb3".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, title: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

/// Tags canon's head `name`; returns the tag's hash.
fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn landing_of(r: &Rdy, tag: Hash) -> Hash {
    r.store().read_tag(&tag).unwrap().landing
}

/// The batch of `tag`'s whole history, as core's export writes it.
fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    let _: Vec<ObjectKey> = stream_export::write(&r.read(), tag, &BTreeSet::new(), std::io::sink())
        .unwrap()
        .keys;
    out
}

/// The block `id` as canon held it at `landing`.
fn block_at(r: &Rdy, landing: Hash, id: &str) -> Block {
    r.read()
        .block(&Scope::At(landing), &BlockId::parse(id).unwrap())
        .unwrap()
}

fn everyday(b: &Block) -> Option<&FieldValue> {
    b.fields.get("everyday")
}

fn upload(dir: &Path, name: &str, bytes: &[u8]) -> Vec<File> {
    let p = dir.join(format!("{name}.rdysync"));
    std::fs::write(&p, bytes).unwrap();
    vec![File::open(p).unwrap()]
}

#[test]
fn nb3_adapter_carries_everyday_as_an_ordinary_field() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut r = Rdy::init(&src, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "term", "id": "$on", "parent": "$d", "body": "A word the team uses.", "fields": {"name": "store", "everyday": true}},
            {"op": "create", "type": "term", "id": "$off", "parent": "$d", "body": "Another word.", "fields": {"name": "bundle", "everyday": false}},
            {"op": "create", "type": "entity", "id": "$none", "parent": "$d", "fields": {"name": "Ledger"}}
        ]),
    );
    let (on, off, none) = (
        w.created["$on"].clone(),
        w.created["$off"].clone(),
        w.created["$none"].clone(),
    );
    let first = land(&mut r, "first");
    let v1 = tag(&mut r, "v1.0.0");
    let l1 = landing_of(&r, v1);
    assert_eq!(l1, first);

    // the stored blocks keep true, false and absent apart
    let (b_on, b_off, b_none) = (
        block_at(&r, l1, &on),
        block_at(&r, l1, &off),
        block_at(&r, l1, &none),
    );
    assert_eq!(everyday(&b_on), Some(&FieldValue::Bool(true)));
    assert_eq!(everyday(&b_off), Some(&FieldValue::Bool(false)));
    assert_eq!(everyday(&b_none), None, "no default is stored");

    // encode and hash are the existing generic ones, and the value survives a decode
    for b in [&b_on, &b_off, &b_none] {
        let bytes = encode_block(b).unwrap();
        assert_eq!(bytes, postcard::to_allocvec(b).unwrap());
        assert_eq!(hash_bytes(&bytes), b.version_hash());
        let back: Block = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(&back, b);
        assert_eq!(everyday(&back), everyday(b));
    }
    let mut flipped = b_on.clone();
    flipped
        .fields
        .insert("everyday".into(), FieldValue::Bool(false));
    assert_ne!(
        hash_bytes(&encode_block(&flipped).unwrap()),
        hash_bytes(&encode_block(&b_on).unwrap()),
        "the flag is part of the block's version"
    );
    let mut dropped = b_off.clone();
    dropped.fields.remove("everyday");
    assert_ne!(
        hash_bytes(&encode_block(&dropped).unwrap()),
        hash_bytes(&encode_block(&b_off).unwrap()),
        "false is stored, not an absent default"
    );

    // take: the same generic record reaches a holder
    let id = r.store().repo_id().unwrap().unwrap();
    let mut handle = StoreHandle::create_holder(&dir.path().join("hub"), id).unwrap();
    handle
        .take(
            &upload(dir.path(), "u1", &export(&r, v1)),
            UploadTarget {
                tag: "v1.0.0".into(),
                landing: l1,
            },
        )
        .unwrap();
    // a snapshot captured now holds one tag
    let old = handle.snapshot().unwrap();
    assert_eq!(old.tags().unwrap().len(), 1);

    // the source flips both flags; a second take moves the holder, never the old snapshot
    write(
        &mut r,
        json!([
            {"op": "update", "id": on, "fields": {"everyday": false}},
            {"op": "update", "id": off, "fields": {"everyday": true}}
        ]),
    );
    land(&mut r, "second");
    let v2 = tag(&mut r, "v2.0.0");
    let l2 = landing_of(&r, v2);
    handle
        .take(
            &upload(dir.path(), "u2", &export(&r, v2)),
            UploadTarget {
                tag: "v2.0.0".into(),
                landing: l2,
            },
        )
        .unwrap();
    assert_eq!(old.tags().unwrap().len(), 1, "a captured snapshot is fixed");
    assert_eq!(old.tags().unwrap()[0].1.landing, l1);
    let fresh = handle.snapshot().unwrap();
    let tags = fresh.tags().unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[1].1.landing, l2);
    // the snapshots hold read states of the holder: release them before core opens it
    drop((old, fresh, tags));
    handle.close().unwrap();

    // the holder's history carries each version of the flag as written
    let holder = Rdy::open_holder(&dir.path().join("hub")).unwrap();
    assert_eq!(
        everyday(&block_at(&holder, l1, &on)),
        Some(&FieldValue::Bool(true))
    );
    assert_eq!(
        everyday(&block_at(&holder, l1, &off)),
        Some(&FieldValue::Bool(false))
    );
    assert_eq!(everyday(&block_at(&holder, l1, &none)), None);
    assert_eq!(
        everyday(&block_at(&holder, l2, &on)),
        Some(&FieldValue::Bool(false))
    );
    assert_eq!(
        everyday(&block_at(&holder, l2, &off)),
        Some(&FieldValue::Bool(true))
    );
    // and the holder's bytes equal the source's
    for (landing, ids) in [(l1, [&on, &off, &none]), (l2, [&on, &off, &none])] {
        for id in ids {
            assert_eq!(
                block_at(&holder, landing, id),
                block_at(&r, landing, id),
                "{id}"
            );
        }
    }
    holder.close().unwrap();
    r.close().unwrap();
}
