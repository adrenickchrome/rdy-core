//! hpt2/hpr2 bundles (D-3DG3R, D-A3H7S): a hub port bundle is made from one holder snapshot. It
//! takes landings of that snapshot's hub canon only, once each, oldest first. It counts every
//! byte against 64 MiB, and it verifies whole before anything reads it. The helpers here build
//! the source and target lines that the port tests share.
use crate::common;

use rdy_engine::brief::{Briefing, Coverage};
use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::port::{self, PortBundle, PreparedPort};
use rdy_engine::port_wire::{
    self, BundleBlob, BundleChange, BundleChunk, BundleContent, BundleLanding,
};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::foreign::{ForeignId, ForeignLink, ForeignParts, ForeignPin};
use rdy_model::*;
use rdy_store::hub_canon::{CanonMove, Moved, replaced_ref};
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::take_batch::UploadTarget;
use rdy_store::{BlobState, ViewOpKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

pub(crate) const T0: i64 = 1_000_000;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

// ---------------------------------------------------------------------------------------------
// shared helpers

pub(crate) fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

pub(crate) fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

/// The person a carry names: role Writer, run `hub-carry`, at a fixed time.
pub(crate) fn person() -> Provenance {
    Provenance {
        actor: "human:qa".into(),
        run: Some("hub-carry".into()),
        role: Role::Writer,
        at_ms: T0,
    }
}

pub(crate) fn ops(v: Value) -> Vec<OpInput> {
    parse_ops(serde_json::from_value(v).unwrap()).unwrap()
}

pub(crate) fn editorial(reason: &str) -> Option<Justification> {
    Some(Justification::Editorial {
        class: EditorialClass::Clarify,
        reason: reason.into(),
    })
}

/// A registered briefing of `ids` in `scope`.
pub(crate) fn brief(r: &Rdy, scope: &Scope, ids: &[&str]) -> Briefing {
    let b = r
        .read()
        .brief(
            scope,
            &Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap_or_else(|e| panic!("brief {ids:?}: {e}"));
    b.register(&r.briefs());
    b
}

/// Writes `v` to `view` as one change with `message` and `justification`. With `brief_ids`, the
/// write cites a fresh canon briefing of them.
pub(crate) fn write_as(
    r: &mut Rdy,
    view: &str,
    v: Value,
    brief_ids: &[&str],
    message: &str,
    justification: Option<Justification>,
) -> WriteResult {
    let token = (!brief_ids.is_empty()).then(|| brief(r, &Scope::Canon, brief_ids).base.token);
    r.write_briefed(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: ops(v),
            justification,
            message: message.into(),
        },
        token,
    )
    .unwrap_or_else(|e| panic!("write {view}: {e}"))
}

pub(crate) fn write(r: &mut Rdy, view: &str, v: Value) -> WriteResult {
    write_as(r, view, v, &[], &format!("write to {view}"), None)
}

/// Proposes and lands `view`; returns the landing.
pub(crate) fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e}"))
        .parse()
        .unwrap()
}

/// Updates the body of `id` in `view` and lands it; returns the landing.
pub(crate) fn edit(r: &mut Rdy, view: &str, id: &str, body: &str) -> Hash {
    write(r, view, json!([{"op": "update", "id": id, "body": body}]));
    land(r, view)
}

pub(crate) fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

pub(crate) fn refusal(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

/// The version of block `id` in `scope` of `r`, if it holds the block.
pub(crate) fn version_in(r: &Rdy, scope: &Scope, id: &str) -> Option<Hash> {
    r.read()
        .materialized(scope)
        .unwrap()
        .get(&bid(id))
        .map(|b| b.version_hash())
}

/// The source line `s` and a fork of it, `t`, that share landing `seed`. The seed holds a stable
/// doc `spec` (a section with claim `c`) and a draft doc `notes` with claim `dc` and the two
/// sections `a` and `b`.
pub(crate) struct World {
    pub root: tempfile::TempDir,
    pub s: Rdy,
    pub t: Rdy,
    pub c: String,
    pub notes: String,
    pub dc: String,
    pub a: String,
    pub b: String,
    pub seed: Hash,
}

pub(crate) fn world() -> World {
    let root = tempfile::tempdir().unwrap();
    let mut s = Rdy::init(&root.path().join("s"), "rdy-standard").unwrap();
    let w = write_as(
        &mut s,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Refunds"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Refunds take five days."},
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "claim", "id": "$dc", "parent": "$n", "body": "A draft note."},
            {"op": "create", "type": "section", "id": "$a", "parent": "$n", "fields": {"title": "Section A"}},
            {"op": "create", "type": "section", "id": "$b", "parent": "$n", "fields": {"title": "Section B"}}
        ]),
        &[],
        "seed the line",
        editorial("seed the test world"),
    );
    let seed = land(&mut s, "seed");
    clone(CloneSource::State(&s.read()), &root.path().join("t"), true).unwrap();
    let t = Rdy::open(&root.path().join("t")).unwrap();
    let id = |k: &str| w.created[k].clone();
    World {
        c: id("$c"),
        notes: id("$n"),
        dc: id("$dc"),
        a: id("$a"),
        b: id("$b"),
        seed,
        s,
        t,
        root,
    }
}

/// The sync frames of `read`'s log: what a push of everything brings to a hub store.
pub(crate) fn frames(read: &ReadState) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    read.reader()
        .for_each_frame_from(ORIGIN, read.seq(), |_, raw, _, payload, _| {
            if rdy_store::sync_stream::is_sync_kind(rdy_store::codec::logical_kind(raw)) {
                out.push((raw, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

/// A hub store of `s`'s line at `dir`: a holder of its repo whose hub canon is `s`'s canon.
pub(crate) fn hub(s: &Rdy, dir: &Path) -> Rdy {
    let read = s.read();
    let head = read.canon_head().unwrap();
    let mut h = Rdy::create_holder(dir, read.repo_id().unwrap()).unwrap();
    let st = h.store_mut();
    let b = st.reader().check_move_batch(frames(&read), head).unwrap();
    assert_eq!(
        st.move_canon(&b, &CanonMove::Seed, None, T0).unwrap(),
        Moved::Moved { tip: head }
    );
    h.close().unwrap();
    Rdy::open_holder(dir).unwrap()
}

/// Moves the hub store `h` at `dir` by `mv` to `from`'s canon head, with `from`'s frames;
/// reopened.
pub(crate) fn move_hub(mut h: Rdy, dir: &Path, from: &Rdy, mv: CanonMove) -> Rdy {
    let read = from.read();
    let head = read.canon_head().unwrap();
    let tip = h.read().canon_head();
    let st = h.store_mut();
    let b = st.reader().check_move_batch(frames(&read), head).unwrap();
    assert_eq!(
        st.move_canon(&b, &mv, tip, T0).unwrap(),
        Moved::Moved { tip: head }
    );
    h.close().unwrap();
    Rdy::open_holder(dir).unwrap()
}

pub(crate) fn make(h: &Rdy, landings: &[Hash]) -> Result<PortBundle, RdyError> {
    port::make_bundle(&h.read(), landings)
}

/// The take checks a hub port runs on the target `t` before its writer: the family, then the
/// preparation on canon's tip.
pub(crate) fn prepare(t: &Rdy, b: &PortBundle, view: &str) -> Result<PreparedPort, RdyError> {
    let st = t.read();
    port::check_family(&st, b)?;
    port::prepare_port(&st, b, view, &person(), &t.briefs())
}

/// Writes the prepared port `p` into `t` as a hub write would: its objects the target lacks,
/// then a new view on its base that includes the changes, in port order.
pub(crate) fn apply(t: &mut Rdy, p: &PreparedPort) {
    let reader = t.store().reader();
    let new: Vec<(u8, Vec<u8>)> = p
        .object_frames()
        .unwrap()
        .into_iter()
        .filter(|(raw, payload)| {
            !reader
                .has_kind(rdy_store::codec::logical_kind(*raw), &Hash::of(payload))
                .unwrap()
        })
        .collect();
    drop(reader);
    let s = t.store_mut();
    s.write(new).unwrap();
    s.view_create(&p.view, p.base, None, false, &person())
        .unwrap();
    if !p.changes.is_empty() {
        s.view_append(
            &p.view,
            vec![ViewOpKind::Include {
                changes: p.changes.iter().map(Change::id).collect(),
            }],
            &person(),
        )
        .unwrap();
    }
    t.seal_open().unwrap();
    t.invalidate();
}

/// The landings a bundle holds, in its order.
pub(crate) fn landings(b: &PortBundle) -> Vec<Hash> {
    b.facts().landings.iter().map(|l| l.landing).collect()
}

/// The changes of landing `l` in `r`.
pub(crate) fn changes_of(r: &Rdy, l: &Hash) -> Vec<Hash> {
    r.store().reader().landing(l).unwrap().changes
}

pub(crate) fn not_on_canon(l: &Hash) -> (String, String) {
    (
        format!("landing {l} is not on this store's hub canon"),
        "carry a landing from this line's hub canon".into(),
    )
}

fn assert_not_on_canon(r: Result<PortBundle, RdyError>, l: &Hash) {
    let e = r.unwrap_err();
    let (m, f) = not_on_canon(l);
    assert_eq!(refusal(&e), (E_BAD_INPUT, m.as_str(), f.as_str()));
}

fn assert_unverified(r: Result<BundleContent, RdyError>, what: &str) {
    let e = r.expect_err(what);
    assert_eq!(
        refusal(&e),
        (
            E_BAD_INPUT,
            "the port bundle does not verify",
            "make the bundle again in the source line"
        ),
        "{what}"
    );
}

fn assert_too_large<T: std::fmt::Debug>(r: Result<T, RdyError>, what: &str) {
    let e = r.expect_err(what);
    assert_eq!(
        refusal(&e),
        (
            E_BAD_INPUT,
            "the port bundle is too large",
            "carry fewer landings at a time"
        ),
        "{what}"
    );
}

/// `n` bytes no two runs of this file differ on.
fn figure_bytes(n: usize, salt: u8) -> Vec<u8> {
    (0..n).map(|i| ((i % 251) as u8) ^ salt).collect()
}

/// Lands a figure of `bytes` under `notes` in `r` through view `view`; returns the landing, the
/// blob and the figure's id.
pub(crate) fn land_figure(
    r: &mut Rdy,
    view: &str,
    notes: &str,
    bytes: &[u8],
) -> (Hash, Hash, String) {
    let blob = r.store_mut().put_blob(bytes).unwrap();
    let w = write(
        r,
        view,
        json!([{"op": "create", "type": "figure", "id": "$f", "parent": notes,
            "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}}]),
    );
    (land(r, view), blob, w.created["$f"].clone())
}

/// Removes every version of block `target` of `r` and lands the removal through `view`.
pub(crate) fn remove(r: &mut Rdy, view: &str, target: &str) -> Hash {
    let res = r
        .redact_in(
            &writer(),
            view,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked figure".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(res.redaction.is_some());
    land(r, view)
}

/// Takes `s`'s tag `name` (its whole history) into the holder `h`, as a tag upload does: the
/// holder takes the removals that history carries.
pub(crate) fn take_tag(h: &mut Rdy, s: &Rdy, dir: &Path, name: &str) {
    let tag = s
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1;
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&s.read(), tag, &Default::default(), &mut out).unwrap();
    let p = dir.join(format!("{name}.rdysync"));
    std::fs::write(&p, out).unwrap();
    rdy_engine::stream_take::take(
        h,
        &[File::open(&p).unwrap()],
        UploadTarget {
            tag: name.into(),
            landing: s.store().read_tag(&tag).unwrap().landing,
        },
    )
    .unwrap();
}

// ---------------------------------------------------------------------------------------------
// the captured canon

#[test]
fn hpt2_bundle_uses_only_captured_canon() {
    let World {
        root,
        mut s,
        dc,
        seed,
        ..
    } = world();
    let twin_dump = root.path().join("twin.rdydump");
    s.read().dump(&twin_dump).unwrap();
    let l2 = edit(&mut s, "a", &dc, "Second note.");
    let hdir = root.path().join("hub");
    let h = hub(&s, &hdir);
    // canon landings, in any input order, come out oldest first, each with its changes
    let b = make(&h, &[l2, seed]).unwrap();
    assert_eq!(landings(&b), [seed, l2]);
    assert_eq!(
        b.facts().landings[1].changes,
        changes_of(&s, &l2),
        "landing order"
    );
    assert_eq!(
        b.facts().changes,
        changes_of(&s, &seed).len() + changes_of(&s, &l2).len()
    );
    // a later landing the snapshot never held
    let l3 = edit(&mut s, "b", &dc, "Third note.");
    s.tag(&approver(), "v3.0.0", None, None).unwrap();
    assert_not_on_canon(make(&h, &[l3]), &l3);
    // held beside canon, and tagged there: still not canon
    let h = move_hub(h, &hdir, &s, CanonMove::Hold);
    let tag = s.store().tags().unwrap()[0].1;
    let mut h = h;
    h.store_mut()
        .set_refs(vec![("tag/v3.0.0".into(), tag.0.to_vec())])
        .unwrap();
    h.close().unwrap();
    let h = Rdy::open_holder(&hdir).unwrap();
    assert!(h.store().reader().landing(&l3).is_ok(), "held: stored");
    assert_eq!(h.read().canon_head(), Some(l2));
    assert_not_on_canon(make(&h, &[l3]), &l3);
    assert_not_on_canon(make(&h, &[l2, l3]), &l3);
    // stored only: a landing whose objects the holder holds with no ref
    let l4 = edit(&mut s, "c", &dc, "Fourth note.");
    let mut h = h;
    let have: std::collections::HashSet<Hash> =
        frames(&h.read()).iter().map(|(_, p)| Hash::of(p)).collect();
    let landed = [
        kinds::BLOCK,
        kinds::CHANGE,
        kinds::PROPOSAL,
        kinds::LANDING,
        kinds::SNAPSHOT,
        kinds::SNAPSHOT_DELTA,
        kinds::ASSESSMENT,
    ];
    let extra: Vec<(u8, Vec<u8>)> = frames(&s.read())
        .into_iter()
        .filter(|(raw, p)| {
            landed.contains(&rdy_store::codec::logical_kind(*raw)) && !have.contains(&Hash::of(p))
        })
        .collect();
    h.store_mut().write(extra).unwrap();
    h.close().unwrap();
    let h = Rdy::open_holder(&hdir).unwrap();
    assert!(h.store().reader().landing(&l4).is_ok(), "stored");
    assert_not_on_canon(make(&h, &[l4]), &l4);
    // replaced: a twin of the line at the seed lands its own edit, and a forced move puts it on
    // canon; l2 stays stored, marked replaced, and is no longer eligible
    let twin_dir = root.path().join("twin");
    load::load(&twin_dump, &twin_dir).unwrap();
    let mut twin = Rdy::open(&twin_dir).unwrap();
    let ld = edit(&mut twin, "d", &dc, "A divergent note.");
    let h = move_hub(h, &hdir, &twin, CanonMove::Force);
    assert!(
        h.store()
            .reader()
            .get_ref(&replaced_ref(&l2))
            .unwrap()
            .is_some()
    );
    assert!(h.store().reader().landing(&l2).is_ok());
    assert_not_on_canon(make(&h, &[l2]), &l2);
    assert_not_on_canon(make(&h, &[seed, l2]), &l2);
    let b = make(&h, &[ld, seed]).unwrap();
    assert_eq!(landings(&b), [seed, ld]);
}

// ---------------------------------------------------------------------------------------------
// canonical bytes and bounds

/// A small valid content: one landing of `n` empty changes, no blob.
fn empty_changes(n: usize) -> BundleContent {
    BundleContent {
        source: RepoId([2; 16]),
        fork_origins: vec![RepoId([1; 16])],
        landings: vec![BundleLanding {
            landing: Hash([9; 32]),
            changes: (0..n)
                .map(|i| BundleChange {
                    hash: Hash::of(&(i as u64).to_le_bytes()),
                    ops: vec![],
                    justification: None,
                    message: String::new(),
                })
                .collect(),
        }],
        blobs: vec![],
        chunks: vec![],
    }
}

/// The record lengths of `bytes`, read in order: the exact byte rule's terms.
fn record_lengths(bytes: &[u8]) -> Vec<usize> {
    let mut at = port_wire::PORT_BUNDLE_FORM.len() + 8;
    let end = bytes.len() - 32;
    let mut out = vec![];
    while at < end {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        out.push(len);
        at += 4 + len;
    }
    assert_eq!(at, end);
    out
}

#[test]
fn hpt2_bundle_is_canonical_and_bounded() {
    let World {
        root,
        mut s,
        notes,
        dc,
        seed,
        ..
    } = world();
    let (lf, blob, _) = land_figure(&mut s, "f", &notes, &figure_bytes((1 << 20) + 7, 3));
    let l3 = edit(&mut s, "e", &dc, "An edit.");
    let h = hub(&s, &root.path().join("hub"));
    let b = make(&h, &[seed, lf, l3]).unwrap();
    // the same snapshot makes the same bytes; the facts are the bytes'
    assert_eq!(make(&h, &[l3, lf, seed]).unwrap().to_bytes(), b.to_bytes());
    let bytes = b.to_bytes().to_vec();
    assert_eq!(b.facts().bytes, bytes.len());
    assert_eq!(
        b.facts().bundle_hash,
        Hash(bytes[bytes.len() - 32..].try_into().unwrap())
    );
    assert_eq!(&bytes[..18], b"rdy-port-bundle-1\n");
    assert_eq!(
        u64::from_le_bytes(bytes[18..26].try_into().unwrap()) as usize,
        bytes.len() - port_wire::BUNDLE_OVERHEAD
    );
    // the exact byte rule: 58 bytes, then 4 plus each record's length
    let lens = record_lengths(&bytes);
    assert_eq!(bytes.len(), 58 + lens.iter().map(|l| 4 + l).sum::<usize>());
    // the blob travels once, with its two chunks in hash order
    let c = b.content();
    assert_eq!(
        c.blobs.iter().map(|x| x.hash).collect::<Vec<_>>(),
        vec![blob]
    );
    assert_eq!(c.chunks.len(), 2);
    assert!(c.chunks[0].hash < c.chunks[1].hash);
    // verify gives the same value back
    let v = PortBundle::from_bytes(&bytes).unwrap();
    assert_eq!(v.facts(), b.facts());
    assert_eq!(v.content(), c);
    assert_eq!(port_wire::encode_bundle(c).unwrap(), bytes);

    // every covered member: a changed byte, a wrong length or a wrong hash does not verify
    let flip = |at: usize| {
        let mut x = bytes.clone();
        x[at] ^= 1;
        x
    };
    assert_unverified(port_wire::decode_bundle(&flip(0)), "form");
    assert_unverified(port_wire::decode_bundle(&flip(18)), "length");
    assert_unverified(port_wire::decode_bundle(&flip(40)), "payload");
    assert_unverified(
        port_wire::decode_bundle(&flip(bytes.len() - 40)),
        "chunk byte",
    );
    assert_unverified(port_wire::decode_bundle(&flip(bytes.len() - 1)), "hash");
    assert_unverified(
        port_wire::decode_bundle(&bytes[..bytes.len() - 1]),
        "truncated",
    );
    let mut longer = bytes.clone();
    longer.push(0);
    assert_unverified(port_wire::decode_bundle(&longer), "one byte more");
    assert_unverified(port_wire::decode_bundle(&[]), "empty");

    // a re-hashed value with a bad structure does not verify either
    let bad = |f: &dyn Fn(&mut BundleContent)| {
        let mut x = c.clone();
        f(&mut x);
        port_wire::decode_bundle(&port_wire::encode_bundle(&x).unwrap())
    };
    assert_unverified(bad(&|x| x.landings.clear()), "no landing");
    assert_unverified(
        bad(&|x| {
            let l = x.landings[0].clone();
            x.landings.push(l);
        }),
        "a landing twice",
    );
    assert_unverified(
        bad(&|x| {
            let ch = x.landings[0].changes[0].clone();
            x.landings[2].changes.push(ch);
        }),
        "a change twice",
    );
    assert_unverified(bad(&|x| x.chunks.reverse()), "chunk order");
    assert_unverified(bad(&|x| x.chunks[0].bytes[0] ^= 1), "chunk hash");
    assert_unverified(bad(&|x| x.chunks.truncate(1)), "a chunk missing");
    assert_unverified(bad(&|x| x.blobs[0].len += 1), "blob length");
    assert_unverified(
        bad(&|x| {
            let b0 = x.blobs[0].clone();
            x.blobs[0] = BundleBlob {
                len: b0.len,
                chunks: b0.chunks.clone(),
                hash: Hash([0xee; 32]),
            };
        }),
        "blob hash",
    );
    assert_unverified(bad(&|x| x.blobs.clear()), "a shown blob missing");
    assert_unverified(
        bad(&|x| {
            let bytes = vec![1u8, 2, 3];
            let k = BundleChunk {
                hash: BundleChunk::hash_of(&bytes),
                bytes,
            };
            x.chunks.push(k);
            x.chunks.sort_by_key(|k| k.hash);
        }),
        "an unused chunk",
    );
    assert_unverified(
        bad(&|x| x.fork_origins = vec![x.source]),
        "the source as its own origin",
    );
    assert_unverified(
        bad(&|x| x.fork_origins = vec![RepoId([5; 16]), RepoId([4; 16])]),
        "origins out of order",
    );

    // 1,000 changes are one bundle; 1,001 are not, on make or on take
    let thousand = port_wire::encode_bundle(&empty_changes(1_000)).unwrap();
    assert_eq!(
        port_wire::decode_bundle(&thousand).unwrap().landings[0]
            .changes
            .len(),
        1_000
    );
    let over = port_wire::encode_bundle(&empty_changes(1_001)).unwrap();
    assert_unverified(port_wire::decode_bundle(&over), "1,001 changes");
    let mut bulk_changes = Vec::new();
    {
        let st = s.read();
        let mut at = st
            .materialized(&Scope::Canon)
            .unwrap()
            .get(&bid(&dc))
            .unwrap()
            .clone();
        let store = s.store_mut();
        for i in 0..1_000u32 {
            let mut next = at.clone();
            next.body = format!("Note {i}.");
            let c = Change {
                view: "bulk".into(),
                ops: vec![Op::Update {
                    base: at.version_hash(),
                    block: next.clone(),
                }],
                deps: vec![],
                provenance: Provenance {
                    actor: "human:qa".into(),
                    run: None,
                    role: Role::Writer,
                    at_ms: T0,
                },
                justification: None,
                message: format!("note {i}"),
                links: ChangeLinks::default(),
                read_identity: None,
            };
            bulk_changes.push(store.commit_to_view("bulk", c).unwrap());
            at = next;
        }
    }
    s.seal_open().unwrap();
    s.invalidate();
    let la = land(&mut s, "bulk");
    assert_eq!(changes_of(&s, &la).len(), 1_000);
    let lb = edit(&mut s, "one-more", &dc, "One more.");
    let h2 = hub(&s, &root.path().join("hub2"));
    assert_eq!(make(&h2, &[la]).unwrap().facts().changes, 1_000);
    assert_too_large(make(&h2, &[la, lb]), "1,001 changes on make");
    assert_too_large(
        make(&h2, &[lb, la]),
        "1,001 changes on make, any input order",
    );
}

#[test]
fn hpr2_bundle_input_order_and_duplicates() {
    let World {
        root,
        mut s,
        dc,
        seed,
        ..
    } = world();
    let l2 = edit(&mut s, "a", &dc, "Second note.");
    let l3 = edit(&mut s, "b", &dc, "Third note.");
    let h = hub(&s, &root.path().join("hub"));
    // no landing
    let e = make(&h, &[]).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BAD_INPUT,
            "name at least one landing to carry",
            "name a landing from this line's hub canon"
        )
    );
    // the entry bound counts before repeats collapse
    let b = make(&h, &[l2; 1_000]).unwrap();
    assert_eq!(landings(&b), [l2]);
    assert_too_large(make(&h, &[l2; 1_001]), "1,001 entries");
    // reversed and repeated inputs make the same bytes, each landing and change once
    let one = make(&h, &[l2, l3]).unwrap();
    for input in [vec![l3, l2], vec![l2, l3, l2], vec![l3, l3, l2, l2]] {
        assert_eq!(
            make(&h, &input).unwrap().to_bytes(),
            one.to_bytes(),
            "{input:?}"
        );
    }
    let all = make(&h, &[l3, seed, l2]).unwrap();
    assert_eq!(landings(&all), [seed, l2, l3]);
    let changes: Vec<Hash> = all
        .facts()
        .landings
        .iter()
        .flat_map(|l| l.changes.clone())
        .collect();
    let mut once = changes.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), changes.len());
    // a take of a bundle that names a landing or a change twice refuses
    let c = all.content().clone();
    let mut twice = c.clone();
    twice.landings.push(c.landings[1].clone());
    assert_unverified(
        PortBundle::from_bytes(&port_wire::encode_bundle(&twice).unwrap())
            .map(|b| b.content().clone()),
        "a landing twice",
    );
    let mut twice = c.clone();
    let ch = c.landings[1].changes[0].clone();
    twice.landings[2].changes.push(ch);
    assert_unverified(
        PortBundle::from_bytes(&port_wire::encode_bundle(&twice).unwrap())
            .map(|b| b.content().clone()),
        "a change twice",
    );
}

#[test]
fn hpr2_bundle_counts_every_byte() {
    // take: the length before anything is read
    let e = PortBundle::from_bytes(&vec![0u8; port_wire::MAX_PORT_BUNDLE_BYTES + 1]).unwrap_err();
    assert_eq!(e.message, "the port bundle does not verify");
    // a record length past what is left does not verify, before the record is read
    let mut x = port_wire::encode_bundle(&empty_changes(1)).unwrap();
    x[26..30].copy_from_slice(&u32::MAX.to_le_bytes());
    let end = x.len() - 32;
    let hash = Hash::of(&x[..end]);
    x[end..].copy_from_slice(&hash.0);
    assert_unverified(port_wire::decode_bundle(&x), "a record past the end");
    // the hub's own limits only: a change of over 10,000 ops verifies
    let mut many = empty_changes(1);
    many.landings[0].changes[0].ops = (0..10_001)
        .map(|i| Op::Delete {
            base: Hash([7; 32]),
            id: bid(&format!("c-{:05}", i % 100_000)),
        })
        .collect();
    let bytes = port_wire::encode_bundle(&many).unwrap();
    assert_eq!(port_wire::decode_bundle(&bytes).unwrap(), many);

    // make: exactly 64 MiB is a bundle, one byte more is not. A reference figure of 64 chunks
    // fixes every other byte; two figures of the same chunk count then land on the bound and
    // one byte past it.
    let World {
        root, mut s, notes, ..
    } = world();
    const MIB: usize = 1 << 20;
    let n0 = 63 * MIB + 1;
    let (lr, _, _) = land_figure(&mut s, "r", &notes, &figure_bytes(n0, 1));
    let h = hub(&s, &root.path().join("hub-r"));
    let b0 = make(&h, &[lr]).unwrap().facts().bytes;
    drop(h);
    let n = n0 + port_wire::MAX_PORT_BUNDLE_BYTES - b0;
    assert!(n < 64 * MIB, "still 64 chunks");
    let (lx, _, _) = land_figure(&mut s, "x", &notes, &figure_bytes(n, 2));
    let (ly, _, _) = land_figure(&mut s, "y", &notes, &figure_bytes(n + 1, 2));
    let h = hub(&s, &root.path().join("hub-x"));
    let b = make(&h, &[lx]).unwrap();
    assert_eq!(b.facts().bytes, port_wire::MAX_PORT_BUNDLE_BYTES);
    assert_eq!(b.to_bytes().len(), port_wire::MAX_PORT_BUNDLE_BYTES);
    // over 16 MiB, one change: the hub takes it whole
    assert!(PortBundle::from_bytes(b.to_bytes()).is_ok());
    assert_too_large(make(&h, &[ly]), "one byte past 64 MiB");
}

#[test]
fn hpr2_make_first_fault_wins() {
    let World {
        root,
        mut s,
        notes,
        dc,
        seed,
        ..
    } = world();
    let (lf, blob, _) = land_figure(&mut s, "f", &notes, &figure_bytes(MIB_PLUS, 4));
    let l3 = edit(&mut s, "e", &dc, "An edit.");
    let hdir = root.path().join("hub");
    let mut h = hub(&s, &hdir);
    let later = edit(&mut s, "later", &dc, "A later edit.");
    // a project is no holder: that wins over an empty input
    let e = make(&s, &[]).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BAD_INPUT,
            "a hub port needs a holder",
            "open the line's hub store"
        )
    );
    // empty wins over nothing else; the entry bound wins over eligibility
    assert_eq!(
        make(&h, &[]).unwrap_err().message,
        "name at least one landing to carry"
    );
    let mut entries = vec![later];
    entries.extend([seed; 1_000]);
    assert_too_large(make(&h, &entries), "the entry bound first");
    // eligibility in input order, before any walk fault
    assert_not_on_canon(make(&h, &[later, Hash([1; 32])]), &later);
    assert_not_on_canon(make(&h, &[Hash([1; 32]), later]), &Hash([1; 32]));
    assert_not_on_canon(make(&h, &[l3, lf, later]), &later);

    // the source removes the figure; the hub store takes that history under a tag
    let held = h.read();
    remove(&mut s, "rm", &format!("blob:{blob}"));
    s.tag(&approver(), "v9.0.0", None, None).unwrap();
    take_tag(&mut h, &s, root.path(), "v9.0.0");
    // a snapshot read before the removal is refused before the input is read
    let e = port::make_bundle(&held, &[]).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_REDACTED_STATE,
            "This read state predates content removal.",
            "Read the project again, or run the dump again."
        )
    );
    drop(held);
    h.close().unwrap();
    let h = Rdy::open_holder(&hdir).unwrap();
    assert!(matches!(
        h.store().reader().blob_state(&blob).unwrap(),
        BlobState::Removed(_)
    ));
    // the walk: the removed blob, oldest first, wins over a later landing's faults
    let e = make(&h, &[l3, lf]).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_REDACTED_STATE,
            "a source blob of the ported change was removed",
            "port the change after its figure is replaced"
        )
    );
    // a landing with no removed blob makes a bundle
    assert_eq!(landings(&make(&h, &[l3]).unwrap()), [l3]);

    // an epoch change during make: the last check refuses the bundle
    let s2 = edit(&mut s, "x", &dc, "Yet another edit.");
    let _ = s2;
    remove(&mut s, "rm2", &dc);
    s.tag(&approver(), "v10.0.0", None, None).unwrap();
    let st = h.read();
    let cell = std::rc::Rc::new(std::cell::RefCell::new(Some(h)));
    let in_hook = std::rc::Rc::clone(&cell);
    let src_dir = root.path().to_path_buf();
    let s_cell = std::rc::Rc::new(s);
    let s_hook = std::rc::Rc::clone(&s_cell);
    port::after_make_walk_for_tests(Box::new(move || {
        let mut guard = in_hook.borrow_mut();
        let h = guard.as_mut().unwrap();
        take_tag(h, &s_hook, &src_dir, "v10.0.0");
    }));
    let e = port::make_bundle(&st, &[seed]).unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e}");
    assert_eq!(e.message, "This read state predates content removal.");
}

const MIB_PLUS: usize = (1 << 20) + 9;

// ---------------------------------------------------------------------------------------------
// the golden vector

fn plain(id: &str, body: &str) -> Block {
    Block {
        id: bid(id),
        block_type: "claim".into(),
        parent: Some(bid("d-00001")),
        order: "V".into(),
        fields: BTreeMap::from([("title".to_string(), FieldValue::Str("A claim".into()))]),
        body: body.into(),
        links: BTreeMap::from([(
            "refines".to_string(),
            vec![Link {
                target: bid("c-00009"),
                part: None,
            }],
        )]),
        pins: BTreeMap::from([(bid("c-00009"), Hash([0x31; 32]))]),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn foreign(id: &str, body: &str) -> Block {
    let target = ForeignId::new(RepoId([7; 16]), bid("d-00002"));
    Block {
        foreign: ForeignParts {
            links: BTreeMap::from([(
                "depends_on".to_string(),
                vec![ForeignLink {
                    target: target.clone(),
                    part: None,
                }],
            )]),
            pins: BTreeMap::from([(
                target,
                ForeignPin {
                    content: Hash([0x44; 32]),
                    encoding: 0,
                },
            )]),
        },
        ..plain(id, body)
    }
}

fn golden() -> BundleContent {
    BundleContent {
        source: RepoId([0x22; 16]),
        fork_origins: vec![RepoId([0x11; 16])],
        landings: vec![BundleLanding {
            landing: Hash([0x10; 32]),
            changes: vec![
                BundleChange {
                    hash: Hash([0x01; 32]),
                    ops: vec![
                        Op::Create {
                            block: plain("c-00001", "Plain."),
                        },
                        Op::Create {
                            block: foreign("c-00002", "Foreign."),
                        },
                    ],
                    justification: editorial("why"),
                    message: "create".into(),
                },
                BundleChange {
                    hash: Hash([0x02; 32]),
                    ops: vec![
                        Op::Update {
                            base: Hash([0x51; 32]),
                            block: plain("c-00001", "Plain, again."),
                        },
                        Op::Update {
                            base: Hash([0x52; 32]),
                            block: foreign("c-00002", "Foreign, again."),
                        },
                    ],
                    justification: None,
                    message: "update".into(),
                },
                BundleChange {
                    hash: Hash([0x03; 32]),
                    ops: vec![Op::Delete {
                        base: Hash([0x53; 32]),
                        id: bid("c-00003"),
                    }],
                    justification: None,
                    message: "delete".into(),
                },
            ],
        }],
        blobs: vec![],
        chunks: vec![],
    }
}

/// The bytes of [`golden`], pinned: `rdy-port-bundle-1` with one landing of a create, an update
/// and a delete, plain and foreign blocks in both.
const GOLDEN_HEX: &str = concat!(
    "7264792d706f72742d62756e646c652d310ace030000000000002200000000222222222222222222",
    "22222222222222011111111111111111111111111111111121000000011010101010101010101010",
    "1010101010101010101010101010101010101010106e010000020101010101010101010101010101",
    "010101010101010101010101010101010101020007632d303030303105636c61696d0107642d3030",
    "303031015601057469746c6503074120636c61696d06506c61696e2e0107726566696e6573010763",
    "2d3030303039000107632d3030303039313131313131313131313131313131313131313131313131",
    "313131313131313100000007632d303030303205636c61696d0107642d3030303031015601057469",
    "746c6503074120636c61696d08466f726569676e2e0107726566696e65730107632d303030303900",
    "0107632d303030303931313131313131313131313131313131313131313131313131313131313131",
    "31010a646570656e64735f6f6e010707070707070707070707070707070707642d30303030320001",
    "0707070707070707070707070707070707642d303030303244444444444444444444444444444444",
    "44444444444444444444444444444444000101020377687906637265617465b60100000202020202",
    "02020202020202020202020202020202020202020202020202020202020151515151515151515151",
    "5151515151515151515151515151515151515151515107632d303030303105636c61696d0107642d",
    "3030303031015601057469746c6503074120636c61696d0d506c61696e2c20616761696e2e010772",
    "6566696e65730107632d3030303039000107632d3030303039313131313131313131313131313131",
    "31313131313131313131313131313131310000015252525252525252525252525252525252525252",
    "52525252525252525252525207632d303030303205636c61696d0107642d30303030310156010574",
    "69746c6503074120636c61696d0f466f726569676e2c20616761696e2e0107726566696e65730107",
    "632d3030303039000107632d30303030393131313131313131313131313131313131313131313131",
    "313131313131313131010a646570656e64735f6f6e01070707070707070707070707070707070764",
    "2d303030303200010707070707070707070707070707070707642d30303030324444444444444444",
    "44444444444444444444444444444444444444444444444400000675706461746553000000020303",
    "03030303030303030303030303030303030303030303030303030303030301025353535353535353",
    "53535353535353535353535353535353535353535353535307632d3030303033000664656c657465",
    "b474e74ce8e5728e2bbab32d9ec0050c5f709f29122b0299c0d7babf04aef68b",
);

#[test]
fn hpt2_bundle_golden_vector() {
    let c = golden();
    let bytes = port_wire::encode_bundle(&c).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, GOLDEN_HEX);
    let back = port_wire::decode_bundle(&bytes).unwrap();
    assert_eq!(back, c);
    let ops = &back.landings[0].changes;
    // the foreign part survives in Create and Update; local links, pins and bases are intact
    for (n, op) in ops[0].ops.iter().chain(&ops[1].ops).enumerate() {
        let (Op::Create { block } | Op::Update { block, .. }) = op else {
            panic!("{op:?}")
        };
        assert_eq!(block.links["refines"][0].target, bid("c-00009"));
        assert_eq!(block.pins[&bid("c-00009")], Hash([0x31; 32]));
        assert_eq!(block.foreign.links.is_empty(), n % 2 == 0, "{n}");
        assert_eq!(block.foreign.pins.len(), n % 2, "{n}");
    }
    assert_eq!(ops[1].ops[0].base(), Some(Hash([0x51; 32])));
    assert_eq!(ops[1].ops[1].base(), Some(Hash([0x52; 32])));
    assert_eq!(ops[2].ops[0].base(), Some(Hash([0x53; 32])));
    assert_eq!(PortBundle::from_bytes(&bytes).unwrap().facts().changes, 3);
}
