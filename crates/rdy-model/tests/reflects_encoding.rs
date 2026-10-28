//! rf4 (D-4WC6R, D-F0904): store format 18 appends `Justification::Reflects` after Editorial. The
//! existing variants keep their discriminants and bytes, so every stored change keeps its hash.
//! The literal encodings below were captured at the base (store format 17) before the variant
//! was appended.
use rdy_model::encoding::{EncodedIdentity, LegacyChangeV0, encoded_hash};
use rdy_model::*;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A change envelope with one op, one dep, a run and a message, around `j`.
fn change(j: Option<Justification>) -> Change {
    Change {
        view: "work".into(),
        ops: vec![Op::Delete {
            base: Hash([7; 32]),
            id: id("c-7KQ2M"),
        }],
        deps: vec![Hash([8; 32])],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: Some("r1".into()),
            role: Role::Writer,
            at_ms: 1_760_000_000_000,
        },
        justification: j,
        message: "m".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

const ENVELOPE_HEAD: &str = "04776f726b0102070707070707070707070707070707070707070707070707070707070707070707632d374b51324d010808080808080808080808080808080808080808080808080808080808080808076167656e743a6101027231018080e682b966";
const ENVELOPE_TAIL: &str = "016d00000000";
/// The latest encoding's tail: the links' two more absent options.
const ENVELOPE_TAIL_V1: &str = "016d000000000000";

fn editorial(class: EditorialClass) -> Justification {
    Justification::Editorial {
        class,
        reason: "wording".into(),
    }
}

#[test]
fn rf4_appended_variant_preserves_existing_bytes() {
    // (justification, its base bytes, the base hash of the change around it)
    let base: [(Option<Justification>, &str, &str); 5] = [
        (
            Some(Justification::Implements(vec![
                id("D-W2HXR"),
                id("D-00002"),
            ])),
            "000207442d573248585207442d3030303032",
            "1979158509bfd8bb4bf3cca017e470b0d9a0a949730be09b9632cb6dd2cd007a",
        ),
        (
            Some(editorial(EditorialClass::Typo)),
            "010007776f7264696e67",
            "560641ed8c8b45d726e0a12af624cb0184e6c561d41162ebf2e8de69b36765df",
        ),
        (
            Some(editorial(EditorialClass::Restructure)),
            "010107776f7264696e67",
            "8baa369fe7ff2dd72fa4a461683421098e3daf28428e28f356ca463d7621c568",
        ),
        (
            Some(editorial(EditorialClass::Clarify)),
            "010207776f7264696e67",
            "42cf525605ec4e3a9f62611165baca341b7e13617de262fc93f7d308c12c41b9",
        ),
        (
            None,
            "",
            "9c3a7b092e89cc0ef1a2ea9b0ab6afbd7f2b556d48987f332298954eaa4ff390",
        ),
    ];
    for (j, bytes, hash) in base {
        if let Some(j) = &j {
            let got = postcard::to_allocvec(j).unwrap();
            assert_eq!(hex(&got), bytes, "{j:?}");
            let back: Justification = postcard::from_bytes(&got).unwrap();
            assert_eq!(&back, j);
        }
        // the change envelope: Some(j) is 01 then j's bytes, None is 00
        let option = if j.is_some() { "01" } else { "00" };
        let c = change(j.clone());
        // encoding 0, frozen: the base bytes and hash
        let v0 = LegacyChangeV0::from_current(&c).unwrap();
        let got = postcard::to_allocvec(&v0).unwrap();
        assert_eq!(
            hex(&got),
            format!("{ENVELOPE_HEAD}{option}{bytes}{ENVELOPE_TAIL}"),
            "{j:?}"
        );
        assert_eq!(Hash::of(&got).to_string(), hash, "{j:?}");
        let back = postcard::from_bytes::<LegacyChangeV0>(&got)
            .unwrap()
            .into_current(EncodedIdentity {
                hash: Hash::of(&got),
                version: 0,
            });
        assert_eq!(back, c);
        assert_eq!(back.id().to_string(), hash);
        // encoding 1, the latest: the same justification bytes in the longer envelope
        let got = postcard::to_allocvec(&c).unwrap();
        assert_eq!(
            hex(&got),
            format!("{ENVELOPE_HEAD}{option}{bytes}{ENVELOPE_TAIL_V1}"),
            "{j:?}"
        );
        assert_eq!(c.id(), encoded_hash(1, &got), "{j:?}");
        assert_eq!(postcard::from_bytes::<Change>(&got).unwrap(), c);
    }
    // Reflects is discriminant 2: its sources in first-occurrence order, then the code flag
    let r = Justification::Reflects {
        sources: vec![id("S-00002"), id("S-00001")],
        code: true,
    };
    let got = postcard::to_allocvec(&r).unwrap();
    assert_eq!(
        hex(&got),
        "020207532d303030303207532d303030303101",
        "discriminant 2, two sources in order, code true"
    );
    assert_eq!(got[0], 2);
    assert_eq!(postcard::from_bytes::<Justification>(&got).unwrap(), r);
    let code_only = Justification::Reflects {
        sources: vec![],
        code: true,
    };
    let got = postcard::to_allocvec(&code_only).unwrap();
    assert_eq!(hex(&got), "020001");
    assert_eq!(
        postcard::from_bytes::<Justification>(&got).unwrap(),
        code_only
    );
    let source_only = Justification::Reflects {
        sources: vec![id("S-00001")],
        code: false,
    };
    let got = postcard::to_allocvec(&source_only).unwrap();
    assert_eq!(hex(&got), "020107532d303030303100");
    // a change around it round-trips and keeps the envelope around the new bytes
    let c = change(Some(r));
    let got = postcard::to_allocvec(&c).unwrap();
    assert_eq!(
        hex(&got),
        format!("{ENVELOPE_HEAD}01020207532d303030303207532d303030303101{ENVELOPE_TAIL_V1}")
    );
    assert_eq!(postcard::from_bytes::<Change>(&got).unwrap(), c);
    let v0 = postcard::to_allocvec(&LegacyChangeV0::from_current(&c).unwrap()).unwrap();
    assert_eq!(
        hex(&v0),
        format!("{ENVELOPE_HEAD}01020207532d303030303207532d303030303101{ENVELOPE_TAIL}")
    );
    // an unassigned discriminant does not decode
    assert!(postcard::from_bytes::<Justification>(&[3, 0, 0]).is_err());
}
