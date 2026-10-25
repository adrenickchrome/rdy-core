//! rf4 (D-4WC6R, D-69AY3, D-F0904): store format 18 appends `Justification::Reflects` and moves
//! in dump format 3 as the pair (3, 18), with exactly the frame kinds of (3, 17). Every released
//! pair stays readable; a native store of format 17 is refused, with the dump-and-load fix.
use rdy_model::*;
use rdy_store::dump::{DUMP_FORMAT, READS, frame_kinds};
use rdy_store::format::{FORMAT_FILE, LAST_SCALAR_STORE_FORMAT};
use rdy_store::{Store, StoreError};
use std::collections::BTreeMap;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn claim(block: &str) -> Block {
    Block {
        id: id(block),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "The claim reflects its source.".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(op: Op, j: Justification) -> Change {
    Change {
        view: "work".into(),
        ops: vec![op],
        deps: vec![],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: Some("r1".into()),
            role: Role::Writer,
            at_ms: 1,
        },
        justification: Some(j),
        message: "reflects".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

#[test]
fn rf4_pair18_keeps_all_released_pairs() {
    assert_eq!((DUMP_FORMAT, LAST_SCALAR_STORE_FORMAT), (4, 18));
    assert_eq!(
        READS,
        &[
            (1, 10),
            (1, 11),
            (1, 12),
            (2, 13),
            (2, 14),
            (2, 15),
            (3, 16),
            (3, 17),
            (3, 18)
        ]
    );
    // the same kind set as (3, 17): a new enum variant adds no kind
    let want: &[u8] = &[
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
    ];
    assert_eq!(frame_kinds(3, 18), Some(want));
    assert_eq!(frame_kinds(3, 17), Some(want));
    for unassigned in [0u8, 12, 14, 25, 255] {
        assert!(
            !frame_kinds(3, 18).unwrap().contains(&unassigned),
            "kind {unassigned} is unassigned"
        );
    }
    // no other pair is read
    assert_eq!(frame_kinds(3, 19), None);
    assert_eq!(frame_kinds(4, 18), None);
    assert_eq!(frame_kinds(2, 18), None);
    // a native store of format 17 is refused before it is read, with the dump-and-load fix
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    Store::create(&path).unwrap().close().unwrap();
    std::fs::write(
        path.join(FORMAT_FILE),
        "rdy store format\nstore_format 17\n",
    )
    .unwrap();
    match Store::open(&path) {
        Err(StoreError::Format { message, fix }) => {
            assert_eq!(message, "store format 17 predates feature-based stores");
            assert_eq!(
                fix,
                "dump with the rdy that wrote this store, then load into a directory with no store"
            );
        }
        Err(e) => panic!("another error: {e:?}"),
        Ok(_) => panic!("a store of format 17 opened"),
    }
}

#[test]
fn rf4_reflects_changes_keep_their_bytes_in_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let b = claim("c-00001");
    let source_only = change(
        Op::Create { block: b.clone() },
        Justification::Reflects {
            sources: vec![id("S-00002"), id("S-00001")],
            code: false,
        },
    );
    let h1 = s.commit_to_view("work", source_only.clone()).unwrap();
    let mut next = b.clone();
    next.body = "The claim reflects its code.".into();
    let code_only = change(
        Op::Update {
            base: b.version_hash(),
            block: next,
        },
        Justification::Reflects {
            sources: vec![],
            code: true,
        },
    );
    let h2 = s.commit_to_view("work", code_only.clone()).unwrap();
    s.close().unwrap();
    // reopened: each change reads back with its justification, under its own hash, through the
    // bounded decoder (the store fills in a change's deps, so the record is read back)
    let s = Store::open(&path).unwrap();
    for (h, c) in [(h1, &source_only), (h2, &code_only)] {
        let got = s.change(&h).unwrap();
        assert_eq!(got.justification, c.justification);
        assert_eq!((&got.ops, &got.message), (&c.ops, &c.message));
        assert_eq!(got.id(), h);
        let bytes = postcard::to_allocvec(&got).unwrap();
        // the latest encoding: the hash covers the version prefix (ok4)
        assert_eq!(rdy_model::encoding::encoded_hash(1, &bytes), h);
        assert_eq!(rdy_store::bounded::decode::<Change>(&bytes).unwrap(), got);
    }
    let check = s.check_objects().unwrap();
    assert_eq!(check.landed_changes, 0);
    s.close().unwrap();
}
