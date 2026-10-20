//! hpb8 (D-Z8Q4N, D-075QD, D-3M81W, D-KDFZ3): a resolved selection's identity hashes each
//! selected block's id and version hash, and nothing else. A removal keeps a block's recorded
//! version, so it keeps the identity while the selection hash moves; a new title or blob field
//! is a new version and so a new identity. The applied removal requests a resolution names are
//! bounded before each one is kept.
mod common;

use rdy_engine::publication::{self, PublicationSource, Resolved};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::*;
use rdy_schema::Pack;
use rdy_store::kinds::BlobManifest;
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------------------------
// helpers

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(
            &session(Role::Writer),
            view,
            "publication",
            Some(common::TEST_SUMMARY),
        )
        .unwrap();
    common::confirm_exceptions(r, &session(Role::Approver), &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

/// Records one removal request of `targets` and lands it with a person's approval: the real
/// path. Returns the request.
fn redact(r: &mut Rdy, targets: &[String]) -> Hash {
    let res = r
        .redact_in(
            &session(Role::Writer),
            "redact",
            RedactInput {
                targets: targets.to_vec(),
                reason_class: "secret".into(),
                reason: "a leaked value".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let request = res.redaction.expect("a request is recorded");
    let p = r
        .propose(&session(Role::Writer), "redact", "Remove", None)
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    request
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn blk(id: &str, ty: &str, parent: Option<&str>, order: &str) -> Block {
    Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: order.into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn with_field(mut b: Block, k: &str, v: FieldValue) -> Block {
    b.fields.insert(k.into(), v);
    b
}

fn doc(id: &str, parent: Option<&str>, order: &str, name: &str) -> Block {
    with_field(blk(id, "doc", parent, order), "name", s(name))
}

fn figure(id: &str, parent: &str, order: &str, blob: Hash) -> Block {
    with_field(
        with_field(
            blk(id, "figure", Some(parent), order),
            "blob",
            s(&blob.to_string()),
        ),
        "media_type",
        s("image/png"),
    )
}

fn profile_block(id: &str, key: &str, select: &[&str]) -> Block {
    let mut b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    b.fields.insert("key".into(), s(key));
    b.fields.insert("title".into(), s("Profile title"));
    b.fields.insert("audience".into(), s("org"));
    b.fields.insert(
        "select".into(),
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    );
    b
}

fn synth(blocks: Vec<Block>) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

/// The identity bytes of `entries`, written here from the recipe alone: the ASCII tag, one NUL,
/// u32 LE 1, u32 LE count, then per entry in id byte order its u32 LE id length, its id bytes
/// and its 32 raw version bytes.
fn identity_bytes(entries: &[(&str, Hash)]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut out = b"rdy-publication-selection-identity".to_vec();
    out.push(0);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(sorted.len()).unwrap().to_le_bytes());
    for (id, v) in sorted {
        out.extend_from_slice(&u32::try_from(id.len()).unwrap().to_le_bytes());
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&v.0);
    }
    out
}

/// BLAKE3 of `bytes`, straight from the hash crate.
fn blake3_of(bytes: &[u8]) -> Hash {
    Hash(*blake3::hash(bytes).as_bytes())
}

/// The identity of `r`'s entries by the recipe.
fn expected_identity(r: &Resolved) -> Hash {
    let pairs: Vec<(&str, Hash)> = r
        .entries
        .iter()
        .map(|e| (e.id.as_str(), e.version))
        .collect();
    blake3_of(&identity_bytes(&pairs))
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb8_identity_has_fixed_vectors() {
    // the recipe's bytes, pinned
    let zero = identity_bytes(&[]);
    assert_eq!(
        zero,
        b"rdy-publication-selection-identity\0\x01\x00\x00\x00\x00\x00\x00\x00".to_vec()
    );
    let one = identity_bytes(&[("d-0NE01", Hash([0x11; 32]))]);
    let mut want = b"rdy-publication-selection-identity\0\x01\x00\x00\x00\x01\x00\x00\x00".to_vec();
    want.extend_from_slice(&7u32.to_le_bytes());
    want.extend_from_slice(b"d-0NE01");
    want.extend_from_slice(&[0x11; 32]);
    assert_eq!(one, want);
    // several entries are written in id byte order, whatever order they come in
    let several = identity_bytes(&[
        ("p-B0001", Hash([2; 32])),
        ("d-A0001", Hash([1; 32])),
        ("fd-C0001", Hash([3; 32])),
    ]);
    let mut want = b"rdy-publication-selection-identity\0\x01\x00\x00\x00\x03\x00\x00\x00".to_vec();
    for (id, v) in [("d-A0001", 1u8), ("fd-C0001", 3), ("p-B0001", 2)] {
        want.extend_from_slice(&u32::try_from(id.len()).unwrap().to_le_bytes());
        want.extend_from_slice(id.as_bytes());
        want.extend_from_slice(&[v; 32]);
    }
    assert_eq!(several, want);

    // the independent BLAKE3 results of those bytes, pinned; Hash::of is the same function
    for (bytes, hex) in [(&zero, ZERO_HEX), (&one, ONE_HEX), (&several, SEVERAL_HEX)] {
        assert_eq!(blake3_of(bytes).to_string(), hex);
        assert_eq!(Hash::of(bytes), blake3_of(bytes));
    }

    let (_dir, r) = project();
    let st = r.read();
    // zero entries: the implicit profile over an empty state
    let empty =
        publication::resolve_in_for_tests(&st, &synth(vec![]), publication::IMPLICIT_ORG).unwrap();
    assert!(empty.entries.is_empty());
    assert_eq!(empty.selection_identity.to_string(), ZERO_HEX);
    // one entry: the implicit profile over one doc
    let single = publication::resolve_in_for_tests(
        &st,
        &synth(vec![doc("d-0NE01", None, "a", "one")]),
        publication::IMPLICIT_ORG,
    )
    .unwrap();
    assert_eq!(single.entries.len(), 1);
    assert_eq!(single.selection_identity, expected_identity(&single));
    assert_eq!(
        single.selection_identity,
        blake3_of(&identity_bytes(&[(
            "d-0NE01",
            doc("d-0NE01", None, "a", "one").version_hash()
        )]))
    );

    // several entries, with figures: exactly the digest of the (id, version) list; no
    // attachment row, availability or profile fact enters it
    let blocks = vec![
        doc("d-ADM1N", None, "zz", "admin"),
        profile_block("pf-AAAA1", "site", &["d-PAGE1"]),
        doc("d-PAGE1", None, "a", "page"),
        figure("fg-F1G01", "d-PAGE1", "a", Hash::of(b"one")),
        figure("fg-F1G02", "d-PAGE1", "b", Hash::of(b"two")),
    ];
    let named = publication::resolve_in_for_tests(&st, &synth(blocks.clone()), "site").unwrap();
    assert_eq!(named.entries.len(), 3);
    assert_eq!(named.attachments.len(), 2);
    let pairs: Vec<(&str, Hash)> = ["d-PAGE1", "fg-F1G01", "fg-F1G02"]
        .into_iter()
        .map(|id| {
            let b = blocks.iter().find(|b| b.id.as_str() == id).unwrap();
            (id, b.version_hash())
        })
        .collect();
    assert_eq!(named.selection_identity, blake3_of(&identity_bytes(&pairs)));
    assert_ne!(named.selection_identity, named.selection_hash);
    // a profile title changes the profile's version, not the identity of what it selects
    let retitled = publication::resolve_in_for_tests(
        &st,
        &synth(
            blocks
                .iter()
                .cloned()
                .map(|b| {
                    if b.id.as_str() == "pf-AAAA1" {
                        with_field(b, "title", s("Another title"))
                    } else {
                        b
                    }
                })
                .collect(),
        ),
        "site",
    )
    .unwrap();
    assert_ne!(retitled.profile.version, named.profile.version);
    assert_eq!(retitled.selection_identity, named.selection_identity);
    assert_ne!(retitled.selection_hash, named.selection_hash);
}

/// BLAKE3 of the zero-entry bytes.
const ZERO_HEX: &str = "9230b0aa2486bd0655af576f154b1ebd335afaf05d31b72d3f60e3ec81bb91da";
/// BLAKE3 of the one-entry bytes (`d-0NE01`, version 0x11 × 32).
const ONE_HEX: &str = "2ff8c716a34dc2ae9f531012bcd4ae6473b9dccd4af097603e54d5df25b30998";
/// BLAKE3 of the three-entry bytes.
const SEVERAL_HEX: &str = "799f0256ec8d8bd0a02b51860a5406b2b03780d87e16eb346565ee293c2102c2";

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

#[test]
fn hpb8_redaction_keeps_identity_but_changes_selection_hash() {
    let (_dir, mut r) = project();
    let blob_a = r.store_mut().put_blob(b"blob of figure a").unwrap();
    let blob_b = r.store_mut().put_blob(b"blob of figure b").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "SENTINEL-PROSE text."},
            {"op": "create", "type": "figure", "id": "$fa", "parent": "$d", "fields": {"blob": blob_a.to_string(), "media_type": "application/octet-stream", "alt": "a"}},
            {"op": "create", "type": "figure", "id": "$fb", "parent": "$d", "fields": {"blob": blob_b.to_string(), "media_type": "application/octet-stream", "alt": "b"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = |k: &str| w.created[k].clone();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": id("$admin"),
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id("$d")]}}]),
    );
    land(&mut r, "work");
    let resolve = |r: &Rdy| publication::resolve(&r.read(), &Scope::Canon, "site").unwrap();
    let base = resolve(&r);
    assert_eq!(base.selection_identity, expected_identity(&base));
    assert_eq!(base.attachments.len(), 2);
    assert!(base.attachments.iter().all(|a| a.available));
    assert!(base.redactions.is_empty());
    let mut last = base.selection_hash;

    // 1. a selected prose block's content
    let prose = redact(&mut r, &[id("$p")]);
    let now = resolve(&r);
    assert_eq!(now.selection_identity, base.selection_identity, "prose");
    assert_eq!(now.entries.len(), base.entries.len());
    assert_ne!(now.selection_hash, last, "prose");
    assert_eq!(now.redactions, vec![prose]);
    last = now.selection_hash;

    // 2. a figure's blob content only: the figure block keeps its fields and version
    let blob = redact(&mut r, &[format!("blob:{blob_a}")]);
    let now = resolve(&r);
    assert_eq!(now.selection_identity, base.selection_identity, "blob");
    assert_ne!(now.selection_hash, last, "blob");
    let row = now
        .attachments
        .iter()
        .find(|a| a.blob == blob_a)
        .expect("the figure keeps its row");
    assert!(!row.available);
    let mut want = vec![prose, blob];
    want.sort();
    assert_eq!(now.redactions, want);
    last = now.selection_hash;

    // 3. a whole figure block: its stub has no blob field and no attachment row, and keeps its
    // recorded version, so the identity stays (D-075QD)
    let whole = redact(&mut r, &[id("$fb")]);
    let now = resolve(&r);
    assert_eq!(now.selection_identity, base.selection_identity, "figure");
    assert_ne!(now.selection_hash, last, "figure");
    assert!(now.attachments.iter().all(|a| a.blob != blob_b));
    assert_eq!(now.attachments.len(), 1);
    let fb = bid(&id("$fb"));
    assert_eq!(
        now.entries.iter().find(|e| e.id == fb).unwrap().version,
        base.entries.iter().find(|e| e.id == fb).unwrap().version,
        "the stub keeps its recorded version"
    );
    let mut want = vec![prose, blob, whole];
    want.sort();
    assert_eq!(now.redactions, want);
    assert_eq!(now.selection_identity, expected_identity(&now));

    // a changed blob field is a new version of the figure, so a new identity
    let blob_c = r.store_mut().put_blob(b"blob of figure c").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$fa"), "fields": {"blob": blob_c.to_string()}}]),
    );
    land(&mut r, "work");
    let swapped = resolve(&r);
    assert_ne!(swapped.selection_identity, base.selection_identity);
    assert_eq!(swapped.selection_identity, expected_identity(&swapped));
}

/// One request per index: distinct, cheap, and never a payload.
fn request(i: u64) -> Hash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&i.to_be_bytes());
    h[31] = 0xA5;
    Hash(h)
}

/// A source that reports `n` removal requests for any blob, each `twice` when asked, and counts
/// each report; it holds no blob byte and allocates no request list of its own.
struct Counting {
    pack: Pack,
    n: u64,
    twice: bool,
    visits: Cell<u64>,
    blobs: Cell<u64>,
}

impl PublicationSource for Counting {
    fn pack(&self) -> &Pack {
        &self.pack
    }
    fn title(&self) -> &str {
        "Counting"
    }
    fn description(&self) -> Option<&str> {
        None
    }
    fn manifest(&self, _: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        Ok(Some(BlobManifest {
            len: 4,
            chunks: vec![],
        }))
    }
    fn blob_available(&self, _: &Hash) -> Result<bool, RdyError> {
        Ok(self.n == 0)
    }
    fn blob(&self, _: &Hash) -> Result<Vec<u8>, RdyError> {
        self.blobs.set(self.blobs.get() + 1);
        Ok(vec![])
    }
    fn removed_blob(&self, _: &Hash) -> Result<Option<Hash>, RdyError> {
        Ok((self.n > 0).then(|| request(0)))
    }
    fn visit_blob_redactions(
        &self,
        _: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        for i in 0..self.n {
            self.visits.set(self.visits.get() + 1);
            visit(request(i))?;
            if self.twice {
                self.visits.set(self.visits.get() + 1);
                visit(request(i))?;
            }
        }
        Ok(())
    }
}

#[test]
fn hpb8_removed_request_list_is_bounded() {
    const LIMIT: u64 = 1_000_000;
    let (_dir, r) = project();
    let st = r.read();
    let m = synth(vec![
        doc("d-ADM1N", None, "zz", "admin"),
        profile_block("pf-AAAA1", "site", &["d-PAGE1"]),
        doc("d-PAGE1", None, "a", "page"),
        figure("fg-F1G01", "d-PAGE1", "a", Hash::of(b"one")),
    ]);
    let source = |n: u64, twice: bool| Counting {
        pack: st.pack().clone(),
        n,
        twice,
        visits: Cell::new(0),
        blobs: Cell::new(0),
    };

    // exactly the limit, each request reported twice: kept once each, sorted
    let src = source(LIMIT, true);
    let res = publication::resolve_source(&src, &m, "site").unwrap();
    assert_eq!(res.redactions.len(), 1_000_000);
    assert!(res.redactions.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(src.visits.get(), 2 * LIMIT);
    assert_eq!(src.blobs.get(), 0, "no payload is read");
    let distinct: BTreeSet<Hash> = (0..LIMIT).map(request).collect();
    assert!(res.redactions.iter().copied().eq(distinct.into_iter()));
    drop(res);

    // one past the limit: refused before the extra request is kept, and the walk stops there
    let src = source(LIMIT + 1, false);
    let e = publication::resolve_source(&src, &m, "site").unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "Publication redaction request list exceeds the limit of 1000000 requests.",
            "Reduce the selection or split the source project."
        )
    );
    assert_eq!(src.visits.get(), LIMIT + 1);
    assert_eq!(src.blobs.get(), 0, "no payload is read");
}
