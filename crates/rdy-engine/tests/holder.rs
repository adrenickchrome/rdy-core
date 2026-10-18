//! Pack-free holders (D-BPYN8, D-DAGNB, O5, O10, O11): a holder keeps uploaded objects and tags
//! with no canon. It opens with no config and no pack files, reads at tag and landing scopes
//! only, and its fsck checks each tag's own history. These tests copy real projects' objects
//! into a holder, so two tags can name divergent histories.
mod common;

use rdy_engine::publication::{self, Audience, IMPLICIT_ORG, Resolved};
use rdy_engine::*;
use rdy_model::*;
use rdy_store::holder::StoreMode;
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};
const NEEDS_TAG: (&str, &str) = (
    "holder reads need a tag or landing",
    "name tag:<version> or landing:<hash>",
);
const DAMAGED_TAG_FIX: &str =
    "a tag ref is damaged or was written outside rdy tag: restore the store from a backup";

// ---------------------------------------------------------------------------------------------
// helpers

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer)
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver)
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "write to work".into(),
        },
    )
    .unwrap()
}

/// Proposes view `work` under `title` and lands it. Returns the landing.
fn land(r: &mut Rdy, title: &str) -> Hash {
    let p = r
        .propose(&writer(), "work", title, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&founder(), name, None, None).unwrap();
}

fn project(dir: &Path) -> Rdy {
    Rdy::init(dir, "rdy-standard").unwrap()
}

/// A project loaded from a dump of `r`'s current state: the same history, now free to diverge.
fn fork(r: &Rdy, root: &Path, name: &str) -> Rdy {
    let file = root.join(format!("{name}.rdydump"));
    r.read().dump(&file).unwrap();
    let dir = root.join(name);
    load::load(&file, &dir).unwrap();
    Rdy::open(&dir).unwrap()
}

/// The kinds a copy carries into a holder: what a tag's history reaches.
const COPIED: [u8; 8] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::TAG,
];

/// Every object of `src`'s log of a copied kind that `keep` keeps, in log order.
fn objects(src: &Rdy, keep: &dyn Fn(u8, &Hash) -> bool) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    src.store()
        .for_each_frame_from(
            ORIGIN,
            src.store().log_end(),
            |_, kind, hash, payload, _| {
                if COPIED.contains(&kind) && keep(kind, &hash) {
                    out.push((kind, payload.to_vec()));
                }
                Ok(())
            },
        )
        .unwrap();
    out
}

/// The `tag/` refs of `src` named in `names`.
fn tag_refs(src: &Rdy, names: &[&str]) -> Vec<(String, Vec<u8>)> {
    src.store()
        .tags()
        .unwrap()
        .into_iter()
        .filter(|(n, _, _)| names.contains(&n.as_str()))
        .map(|(n, h, _)| (format!("tag/{n}"), h.0.to_vec()))
        .collect()
}

/// A new holder at `dir` holding `items` and `refs`, closed and opened again.
fn holder_with(dir: &Path, items: Vec<(u8, Vec<u8>)>, refs: Vec<(String, Vec<u8>)>) -> Rdy {
    let mut h = Rdy::create_holder(dir, RepoId([1; 16])).unwrap();
    h.store_mut().write(items).unwrap();
    h.store_mut().set_refs(refs).unwrap();
    h.seal_open().unwrap();
    h.close().unwrap();
    Rdy::open_holder(dir).unwrap()
}

fn all(_: u8, _: &Hash) -> bool {
    true
}

/// Two projects that share a first landing (`l1`, tagged v1.0.0), then diverge: the left one
/// lands `left` (v1.1.0), the right one `right` (v2.0.0), each updating the prose block.
struct Divergent {
    root: tempfile::TempDir,
    left: Rdy,
    right: Rdy,
    prose: BlockId,
    l1: Hash,
    l_left: Hash,
    l_right: Hash,
}

fn divergent() -> Divergent {
    divergent_with("left", "right", None)
}

/// [`Divergent`] with these bodies; with `pin`, each side's update is written at that pinned
/// clock, so equal bodies give one change.
fn divergent_with(left_body: &str, right_body: &str, pin: Option<i64>) -> Divergent {
    let root = tempfile::tempdir().unwrap();
    let mut left = project(&root.path().join("left"));
    let w = write(
        &mut left,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "base"}
        ]),
    );
    let prose = BlockId::parse(&w.created["$p"]).unwrap();
    let l1 = land(&mut left, "first");
    tag(&mut left, "v1.0.0");
    let mut right = fork(&left, root.path(), "right");
    let update = |r: &mut Rdy, body: &str| {
        let _clock = pin.map(pin_clock);
        write(
            r,
            json!([{"op": "update", "id": prose.to_string(), "body": body}]),
        );
    };
    update(&mut left, left_body);
    let l_left = land(&mut left, "left");
    tag(&mut left, "v1.1.0");
    update(&mut right, right_body);
    let l_right = land(&mut right, "right");
    tag(&mut right, "v2.0.0");
    Divergent {
        root,
        left,
        right,
        prose,
        l1,
        l_left,
        l_right,
    }
}

impl Divergent {
    /// A holder of both sides' objects and their three tags, where `keep` keeps each object.
    fn holder_keeping(&self, keep: &dyn Fn(u8, &Hash) -> bool) -> Rdy {
        let mut items = objects(&self.left, keep);
        items.extend(objects(&self.right, keep));
        let mut refs = tag_refs(&self.left, &["v1.0.0", "v1.1.0"]);
        refs.extend(tag_refs(&self.right, &["v2.0.0"]));
        holder_with(&self.root.path().join("holder"), items, refs)
    }

    fn holder(&self) -> Rdy {
        self.holder_keeping(&all)
    }
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

fn assert_needs_tag(e: &RdyError) {
    assert_err(e, E_BAD_INPUT, NEEDS_TAG.0, NEEDS_TAG.1);
}

/// The prose body each tag reads, and the landing each names.
fn assert_reads(h: &Rdy, d: &Divergent, bodies: [&str; 3]) {
    let st = h.read();
    for (scope, body) in [
        ("tag:v1.0.0", bodies[0]),
        ("tag:v1.1.0", bodies[1]),
        ("tag:v2.0.0", bodies[2]),
    ] {
        let s = st.resolve_scope(scope).unwrap();
        assert_eq!(st.block(&s, &d.prose).unwrap().body, body, "{scope}");
    }
    assert_eq!(
        st.resolve_scope("tag:v2.0.0").unwrap(),
        Scope::At(d.l_right)
    );
    assert_eq!(st.resolve_scope("tag:v1.1.0").unwrap(), Scope::At(d.l_left));
}

/// Every frame kind of `r`'s log.
fn frame_kinds(r: &Rdy) -> BTreeSet<u8> {
    let mut out = BTreeSet::new();
    r.store()
        .for_each_frame_from(ORIGIN, r.store().log_end(), |_, kind, _, _, _| {
            out.insert(kind);
            Ok(())
        })
        .unwrap();
    out
}

fn ref_names(r: &Rdy) -> Vec<String> {
    r.store()
        .refs_with_prefix("")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// The distinct landings and landed changes of `tags`' chains in `r`.
fn chains(r: &Rdy, tags: &[Hash]) -> (BTreeSet<Hash>, BTreeSet<Hash>) {
    let (mut landings, mut changes) = (BTreeSet::new(), BTreeSet::new());
    for t in tags {
        for (h, rec) in r.store().tag_chain(t).unwrap() {
            landings.insert(h);
            changes.extend(rec.changes);
        }
    }
    (landings, changes)
}

fn put_tag(h: &mut Rdy, name: &str, landing: Hash) -> Hash {
    let t = Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 9,
        },
    };
    let hash = h.store_mut().put(kinds::TAG, &t).unwrap();
    h.store_mut()
        .set_refs(vec![(format!("tag/{name}"), hash.0.to_vec())])
        .unwrap();
    hash
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn ha7_holder_reads_two_divergent_tags() {
    let d = divergent();
    let h = d.holder();
    assert_reads(&h, &d, ["base", "left", "right"]);
    let st = h.read();
    assert_eq!(st.canon_head(), None, "a holder has no canon");
    assert!(st.view_names().is_empty());
    // a landing of either history, by full hash or prefix
    for l in [d.l1, d.l_left, d.l_right] {
        assert_eq!(
            st.resolve_scope(&format!("landing:{l}")).unwrap(),
            Scope::At(l)
        );
        assert_eq!(
            st.resolve_scope(&format!("landing:{}", &l.to_string()[..10]))
                .unwrap(),
            Scope::At(l)
        );
    }
    let right = Scope::At(d.l_right);
    assert_eq!(st.block(&right, &d.prose).unwrap().body, "right");
    // positions follow the scope's own chain
    let pos = st.version_position(&right).unwrap();
    assert_eq!(pos.version.unwrap().name, "v2.0.0");
    assert_eq!(pos.landings_since, 0);
    assert!(pos.frozen);
    // every scope that needs canon is refused, with no fallback
    for text in ["canon", "work", "view:work", "inflight", "proposal:P-1"] {
        assert_needs_tag(&st.resolve_scope(text).unwrap_err());
    }
    for scope in [
        Scope::Canon,
        Scope::View("work".into()),
        Scope::Inflight,
        Scope::Proposal(Hash([5; 32])),
    ] {
        assert_needs_tag(&st.materialized(&scope).unwrap_err());
        assert_needs_tag(&st.version_position(&scope).unwrap_err());
    }
}

#[test]
fn ha7_holder_fsck_checks_both_histories() {
    let d = divergent();
    let mut h = d.holder();
    // the highest version on the oldest landing: a project's chronological order does not apply
    put_tag(&mut h, "v3.0.0", d.l1);
    h.seal_open().unwrap();
    let report = h.fsck().unwrap();
    let (landings, changes) = chains(&h, &[d.l_left, d.l_right]);
    assert_eq!(landings, BTreeSet::from([d.l1, d.l_left, d.l_right]));
    assert_eq!(report.landings, 3);
    assert_eq!(report.versions, 4);
    assert_eq!(report.landed_changes, changes.len());
    assert_eq!(
        (
            report.views,
            report.transitions,
            report.unlogged_moves,
            report.intents,
            report.alt_sets,
            report.reviews,
            report.signatures,
        ),
        (0, 0, 0, 0, 0, 0, 0)
    );
    drop(h);

    // the right history's snapshot is missing: its chain is checked too
    let right_state = d.right.store().landing(&d.l_right).unwrap().state;
    let root = tempfile::tempdir().unwrap();
    let mut items = objects(&d.left, &all);
    items.extend(objects(&d.right, &|k, h| {
        !(k == kinds::SNAPSHOT && *h == right_state)
    }));
    let mut refs = tag_refs(&d.left, &["v1.0.0", "v1.1.0"]);
    refs.extend(tag_refs(&d.right, &["v2.0.0"]));
    let h = holder_with(&root.path().join("h"), items, refs);
    let st = h.read();
    let left = st.resolve_scope("tag:v1.1.0").unwrap();
    assert_eq!(st.block(&left, &d.prose).unwrap().body, "left");
    drop(st);
    let e = h.fsck().unwrap_err();
    assert_eq!(e.code, E_IO, "{e}");
    assert!(
        e.message.ends_with(&format!(
            "names {}:{right_state}, which is not stored",
            kinds::SNAPSHOT
        )),
        "{e}"
    );
}

#[test]
fn ha7_holder_never_uses_global_landing_of() {
    // one change, written at the same pinned clock on both sides, landed by two landings
    let d = divergent_with("same", "same", Some(1_000_000));
    let left = d.left.store().landing(&d.l_left).unwrap();
    let right = d.right.store().landing(&d.l_right).unwrap();
    assert_ne!(d.l_left, d.l_right, "two landings");
    assert_eq!(left.changes, right.changes, "of one change");
    let c = left.changes[0];
    let h = d.holder();
    let global = h.store().landing_of(&c).unwrap();
    assert!(
        global == Some(d.l_left) || global == Some(d.l_right),
        "the global index holds one of them: {global:?}"
    );
    let report = h.fsck().unwrap();
    assert_eq!(report.landings, 3);
    let (_, changes) = chains(&h, &[d.l_left, d.l_right]);
    assert_eq!(report.landed_changes, changes.len());
    assert!(changes.contains(&c));
    assert_reads(&h, &d, ["base", "same", "same"]);
}

#[test]
fn ha7_holder_has_no_work_view_or_transition() {
    let d = divergent();
    let mut h = d.holder();
    assert_eq!(h.reconcile_transitions().unwrap(), 0);
    h.invalidate();
    h.reindex().unwrap();
    h.fsck().unwrap();
    assert!(h.last_transition_error().is_none());
    // a write needs a view on canon: refused before anything is written
    let end = h.store().log_end();
    let e = h
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(
                    json!([{"op": "create", "type": "prose", "id": "$p", "body": "x"}]),
                )
                .unwrap(),
                justification: None,
                message: "write".into(),
            },
        )
        .unwrap_err();
    assert_needs_tag(&e);
    h.seal_open().unwrap();
    assert_eq!(h.store().log_end(), end, "nothing was written");
    h.checkpoint().unwrap();
    let kinds_logged = frame_kinds(&h);
    for k in [kinds::VIEW_OP, kinds::TRANSITION] {
        assert!(!kinds_logged.contains(&k), "kind {k} in {kinds_logged:?}");
    }
    for name in ref_names(&h) {
        assert!(
            name.starts_with("meta/") || name.starts_with("tag/"),
            "{name}"
        );
    }
    assert!(h.read().view_names().is_empty());
    assert_eq!(h.store().mode().unwrap(), StoreMode::Holder);
}

#[test]
fn ha7_holder_uses_no_config_or_pack() {
    let d = divergent();
    let dir = d.root.path().join("my-holder");
    let h = d.holder_keeping(&all);
    drop(h);
    std::fs::rename(d.root.path().join("holder"), &dir).unwrap();
    // config and pack files that no read could parse
    std::fs::write(dir.join(".rdy.yaml"), "pack: [unclosed\n").unwrap();
    std::fs::create_dir_all(dir.join(".rdy/packs")).unwrap();
    std::fs::write(dir.join(".rdy/packs/broken.yaml"), "{{{ not yaml").unwrap();
    std::fs::write(dir.join(".rdy/local.yaml"), ":::").unwrap();
    let h = Rdy::open_holder(&dir).unwrap();
    assert_reads(&h, &d, ["base", "left", "right"]);
    assert_eq!(
        h.pack().name,
        "rdy-standard",
        "the built-in pack, in memory"
    );
    let fp = h.pack_fingerprint();
    assert!(fp.files.is_empty());
    assert_eq!(fp.hash, Hash::of(b""));
    assert_eq!(fp.at, std::time::SystemTime::UNIX_EPOCH);
    // O10: the implicit title is the directory's, not a source name
    assert_eq!(h.read().project_name(), "My holder");
    assert_eq!(h.read().project_description(), None);
    h.fsck().unwrap();
    h.close().unwrap();
    // the project path keeps its config errors before the mode is known
    let e = Rdy::open(&dir).err().unwrap();
    assert_ne!(e.message, "this store is a holder", "{e}");
    assert_eq!(e.code, E_CONFIG, "{e}");
    // with a config it can read, the project path refuses the holder
    std::fs::write(dir.join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    std::fs::remove_file(dir.join(".rdy/packs/broken.yaml")).unwrap();
    std::fs::remove_file(dir.join(".rdy/local.yaml")).unwrap();
    let e = Rdy::open(&dir).err().unwrap();
    assert_err(
        &e,
        E_BAD_INPUT,
        "this store is a holder",
        "open it through the holder adapter",
    );
    // and the holder path refuses a project
    let plain = d.root.path().join("plain");
    project(&plain).close().unwrap();
    let e = Rdy::open_holder(&plain).err().unwrap();
    assert_err(
        &e,
        E_BAD_INPUT,
        "this store is a project",
        "open it through the project adapter",
    );
}

#[test]
fn ha7_holder_accepts_unknown_source_block_types() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("h");
    let mut h = Rdy::create_holder(&dir, RepoId([2; 16])).unwrap();
    let prov = Provenance {
        actor: "human:a".into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    };
    let mut fields = BTreeMap::new();
    fields.insert("no_such_field".into(), FieldValue::Str("v".into()));
    let block = Block {
        id: BlockId::parse("b-00001").unwrap(),
        block_type: "widget".into(),
        parent: None,
        order: "a".into(),
        fields,
        body: "a widget no pack knows".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    let s = h.store_mut();
    s.put(kinds::BLOCK, &block).unwrap();
    let change = Change {
        view: "work".into(),
        ops: vec![Op::Create {
            block: block.clone(),
        }],
        deps: vec![],
        provenance: prov.clone(),
        justification: None,
        message: "m".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    let c = s.put(kinds::CHANGE, &change).unwrap();
    let state = s
        .put(
            kinds::SNAPSHOT,
            &StateSnapshot {
                blocks: BTreeMap::from([(block.id.clone(), block.version_hash())]),
            },
        )
        .unwrap();
    let proposal = s
        .put(
            kinds::PROPOSAL,
            &Proposal {
                stable: None,
                prev: None,
                title: "t".into(),
                view: "work".into(),
                changes: vec![c],
                alternative: None,
                provenance: prov.clone(),
                canon_at: None,
                summary: None,
            },
        )
        .unwrap();
    let landing = s
        .put(
            kinds::LANDING,
            &LandingRecord {
                proposal,
                prev: None,
                state,
                approved_by: prov,
                changes: vec![c],
                chosen: None,
                checks: vec![],
                assurance: Assurance::Local,
                relayed_by: None,
                relay_evidence: None,
                approved_version: None,
                rebased_from: vec![],
                evidence: None,
            },
        )
        .unwrap();
    put_tag(&mut h, "v1.0.0", landing);
    h.seal_open().unwrap();
    h.close().unwrap();
    let h = Rdy::open_holder(&dir).unwrap();
    let st = h.read();
    let scope = st.resolve_scope("tag:v1.0.0").unwrap();
    let read = st.block(&scope, &block.id).unwrap();
    assert_eq!(read.block_type, "widget");
    assert_eq!(read.body, "a widget no pack knows");
    assert_eq!(st.materialized(&scope).unwrap().len(), 1);
    let report = h.fsck().unwrap();
    assert_eq!((report.landings, report.versions), (1, 1));
}

#[test]
fn ha7_holder_fsck_refuses_semantic_duplicate_tags() {
    let d = divergent();
    let mut h = d.holder();
    // the same version as v1.1.0, spelt without its `v`, on the other history
    put_tag(&mut h, "1.1.0", d.l_right);
    h.seal_open().unwrap();
    let e = h.fsck().unwrap_err();
    assert_err(
        &e,
        E_IO,
        "1 version problem(s): tag/1.1.0 and tag/v1.1.0 denote the same version",
        DAMAGED_TAG_FIX,
    );
}

/// A project with docs a, b and admin; tag v0.1.0 before any profile, v1.0.0 with profile
/// `pubsite` selecting a, and v2.0.0 after the profile moved to b.
struct Profiles {
    _root: tempfile::TempDir,
    p: Rdy,
    h: Rdy,
}

fn profiles() -> Profiles {
    let root = tempfile::tempdir().unwrap();
    let mut p = project(&root.path().join("p"));
    let w = write(
        &mut p,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = |n: &str| w.created[n].clone();
    land(&mut p, "docs");
    tag(&mut p, "v0.1.0");
    let pf = write(
        &mut p,
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": id("$admin"),
            "fields": {"key": "pubsite", "title": "Old title", "audience": "public", "select": [id("$a")]}}]),
    );
    let pf = pf.created["$pf"].clone();
    land(&mut p, "profile");
    tag(&mut p, "v1.0.0");
    write(
        &mut p,
        json!([{"op": "update", "id": pf, "fields": {"title": "New title", "select": [id("$b")]}}]),
    );
    land(&mut p, "moved");
    tag(&mut p, "v2.0.0");
    let h = holder_with(
        &root.path().join("h"),
        objects(&p, &all),
        tag_refs(&p, &["v0.1.0", "v1.0.0", "v2.0.0"]),
    );
    Profiles { _root: root, p, h }
}

fn assert_same(a: &Resolved, b: &Resolved) {
    assert_eq!(a.profile, b.profile);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.docs, b.docs);
    assert_eq!(a.attachments, b.attachments);
    assert_eq!(a.references, b.references);
    assert_eq!(a.selection_hash, b.selection_hash);
    assert_eq!(a.m.snapshot(), b.m.snapshot());
}

#[test]
fn ha7_holder_profile_resolution_uses_the_tag_snapshot() {
    let f = profiles();
    let (ps, hs) = (f.p.read(), f.h.read());
    for t in ["tag:v1.0.0", "tag:v2.0.0"] {
        let want = publication::resolve(&ps, &ps.resolve_scope(t).unwrap(), "pubsite").unwrap();
        let got = publication::resolve(&hs, &hs.resolve_scope(t).unwrap(), "pubsite").unwrap();
        assert_same(&got, &want);
    }
    let old =
        publication::resolve(&hs, &hs.resolve_scope("tag:v1.0.0").unwrap(), "pubsite").unwrap();
    let new =
        publication::resolve(&hs, &hs.resolve_scope("tag:v2.0.0").unwrap(), "pubsite").unwrap();
    assert_eq!(old.profile.title, "Old title");
    assert_eq!(new.profile.title, "New title");
    assert_ne!(old.profile.version, new.profile.version);
    assert_ne!(old.selection_hash, new.selection_hash);
    // a tag from before the profile has none, as in the project: no fallback
    let early = hs.resolve_scope("tag:v0.1.0").unwrap();
    let e = publication::resolve(&hs, &early, "pubsite").unwrap_err();
    assert_err(
        &e,
        E_NOT_FOUND,
        "Publication profile \"pubsite\" was not found.",
        "Use a publication profile id or key from this scope.",
    );
    // the implicit profile selects the same snapshot as in the project
    let t = "tag:v2.0.0";
    let want = publication::resolve(&ps, &ps.resolve_scope(t).unwrap(), IMPLICIT_ORG).unwrap();
    let got = publication::resolve(&hs, &hs.resolve_scope(t).unwrap(), IMPLICIT_ORG).unwrap();
    assert_eq!(got.entries, want.entries);
    assert_eq!(got.docs, want.docs);
    // canon is no publication scope in a holder
    assert_needs_tag(&publication::resolve(&hs, &Scope::Canon, "pubsite").unwrap_err());
}

#[test]
fn ha7_holder_resolves_profiles_with_the_builtin_pack() {
    let f = profiles();
    let hs = f.h.read();
    assert!(
        hs.pack().block_types.contains_key("publication_profile"),
        "O5: the built-in pack, never an empty one"
    );
    let scope = hs.resolve_scope("tag:v1.0.0").unwrap();
    let r = publication::resolve(&hs, &scope, "pubsite").unwrap();
    assert_eq!(r.profile.audience, Audience::Public);
    assert_eq!(r.profile.key.as_deref(), Some("pubsite"));
    let implicit = publication::resolve(&hs, &scope, IMPLICIT_ORG).unwrap();
    assert_eq!(implicit.profile.audience, Audience::Org);
    assert_eq!(implicit.profile.title, f.h.read().project_name());
}

#[test]
fn ha7_reindex_and_reopen_preserve_holder_reads() {
    let d = divergent();
    let mut h = d.holder();
    let end = h.store().log_end();
    assert_reads(&h, &d, ["base", "left", "right"]);
    h.reindex().unwrap();
    assert_reads(&h, &d, ["base", "left", "right"]);
    let before = h.fsck().unwrap();
    h.close().unwrap();
    let mut h = Rdy::open_holder(&d.root.path().join("holder")).unwrap();
    assert_reads(&h, &d, ["base", "left", "right"]);
    h.reindex().unwrap();
    h.close().unwrap();
    let h = Rdy::open_holder(&d.root.path().join("holder")).unwrap();
    assert_reads(&h, &d, ["base", "left", "right"]);
    assert_eq!(
        h.store().log_end(),
        end,
        "reads, reindex and reopen write nothing"
    );
    let after = h.fsck().unwrap();
    assert_eq!(
        (after.landings, after.versions, after.landed_changes),
        (before.landings, before.versions, before.landed_changes)
    );
    assert_eq!(h.store().mode().unwrap(), StoreMode::Holder);
}

#[test]
fn ha7_old_read_state_fails_after_removal() {
    // the epoch guard the holder's tag reads share, on an existing project removal
    let _clock = pin_clock(1_000_000);
    let root = tempfile::tempdir().unwrap();
    let mut r = project(&root.path().join("p"));
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The code is ZEBRA-7731."}
        ]),
    );
    let prose = BlockId::parse(&w.created["$p"]).unwrap();
    land(&mut r, "base");
    tag(&mut r, "v1.0.0");
    let res = r
        .redact_in(
            &writer(),
            "redact",
            rdy_engine::redaction::RedactInput {
                targets: vec![prose.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(res.redaction.is_some());
    let prop = r
        .propose(&writer(), "redact", "Remove the code", Some("Removes it."))
        .unwrap();
    let old = r.read();
    let tag_scope = old.resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(
        old.block(&tag_scope, &prose).unwrap().body,
        "The code is ZEBRA-7731."
    );
    old.ensure_current_epoch().unwrap();
    r.land(&founder(), &prop.id).unwrap();
    let e = old.ensure_current_epoch().unwrap_err();
    assert_err(
        &e,
        E_REDACTED_STATE,
        "This read state predates content removal.",
        "Read the project again.",
    );
    // a fresh state reads the stub at the tag
    let fresh = r.read();
    fresh.ensure_current_epoch().unwrap();
    let tag_scope = fresh.resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(
        fresh.block(&tag_scope, &prose).unwrap().body,
        rdy_model::redaction::CONTENT_REMOVED
    );
    // durable: the reopened project reads the stub too
    drop((old, fresh));
    r.close().unwrap();
    let r = Rdy::open(&root.path().join("p")).unwrap();
    let st = r.read();
    let tag_scope = st.resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(
        st.block(&tag_scope, &prose).unwrap().body,
        rdy_model::redaction::CONTENT_REMOVED
    );
}

#[test]
fn ha7_holder_fsck_checks_answer_tag_refs() {
    let d = divergent();
    let mut h = d.holder();
    let tag_hash = Hash(
        h.store()
            .get_ref("tag/v1.1.0")
            .unwrap()
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let answer = |act: AnswerAct, item: &str| Answer {
        item: item.into(),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![tag_hash],
        person: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 7,
        },
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    let good = h
        .store_mut()
        .put(kinds::ANSWER, &answer(AnswerAct::Tag, "tag:v1.1.0"))
        .unwrap();
    let confirm = h
        .store_mut()
        .put(kinds::ANSWER, &answer(AnswerAct::Confirm, "tag:v1.1.0"))
        .unwrap();
    let elsewhere = h
        .store_mut()
        .put(kinds::ANSWER, &answer(AnswerAct::Tag, "tag:v2.0.0"))
        .unwrap();
    let forge = |h: &mut Rdy, name: &str, value: Hash| {
        h.store_mut()
            .set_refs_unchecked_for_tests(vec![(name.into(), value.0.to_vec())])
            .unwrap();
        h.seal_open().unwrap();
    };
    forge(&mut h, "answer/tag:v1.1.0", good);
    h.fsck().unwrap();
    let refused = |h: &Rdy, name: &str| {
        let e = h.fsck().unwrap_err();
        assert_err(
            &e,
            E_IO,
            &format!("cannot decode object: holder ref {name} is not allowed"),
            "run rdy fsck; if the index is damaged run rdy reindex",
        );
    };
    for bad in [confirm, elsewhere, tag_hash] {
        forge(&mut h, "answer/tag:v1.1.0", bad);
        refused(&h, "answer/tag:v1.1.0");
    }
    forge(&mut h, "answer/tag:v1.1.0", good);
    h.fsck().unwrap();
    // no tag beside it
    forge(&mut h, "answer/tag:v9.0.0", good);
    refused(&h, "answer/tag:v9.0.0");
}
