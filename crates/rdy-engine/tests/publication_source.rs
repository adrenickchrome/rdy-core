//! hpb2 (D-48NYM, D-0VCQ5, D-KDFZ3, D-VRW0A, D-QYCRB, D-F9Z60, D-91DZW, D-Z8Q4N; hub n-XGTFE): a
//! publication profile resolves through a bounded source, with no store open. The store and an
//! in-memory source that holds the same snapshot resolve the same selection through the same
//! selector, the same exclusions and the same removals. Every bound applies before the source
//! reads a byte, and a take names its profile by a stable selector.
mod common;
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::publication::{
    self, Audience, CheckStatus, IMPLICIT_ORG, Limits, PublicationSource, ReadPublicationSource,
    Resolved,
};
use rdy_engine::*;
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_schema::Pack;
use rdy_store::StoreError;
use rdy_store::kinds::{self, BlobManifest};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

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

fn founder() -> Session {
    session(Role::Approver)
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
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    bid(&w.created[name])
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

fn assert_limit(e: &RdyError, part: &str, limit: u64, unit: &str) {
    assert_err(
        e,
        E_LIMIT,
        &format!("Publication {part} exceeds the limit of {limit} {unit}."),
        "Reduce the selection or split the source project.",
    );
}

/// Every field of two resolutions, the selected state's records included.
fn assert_every_field(a: &Resolved, b: &Resolved, what: &str) {
    assert_eq!(a.profile, b.profile, "{what}: profile");
    assert_eq!(a.m.snapshot(), b.m.snapshot(), "{what}: snapshot");
    assert_eq!(
        a.m.iter().collect::<Vec<_>>(),
        b.m.iter().collect::<Vec<_>>(),
        "{what}: selected records"
    );
    assert_eq!(a.graph, b.graph, "{what}: graph");
    assert_eq!(a.entries, b.entries, "{what}: entries");
    assert_eq!(a.docs, b.docs, "{what}: docs");
    assert_eq!(a.attachments, b.attachments, "{what}: attachments");
    assert_eq!(a.references, b.references, "{what}: references");
    assert_eq!(a.selection_hash, b.selection_hash, "{what}: selection hash");
}

/// The source of a store state, with the state's own settings.
struct Settings {
    title: String,
    description: Option<String>,
}

impl Settings {
    fn of(st: &ReadState) -> Settings {
        Settings {
            title: st.project_name(),
            description: st.project_description(),
        }
    }

    fn source<'a>(&'a self, st: &'a ReadState) -> ReadPublicationSource<'a> {
        ReadPublicationSource::with_settings(
            st,
            st.pack(),
            &self.title,
            self.description.as_deref(),
        )
    }
}

/// How a memory source holds one blob.
#[derive(Clone)]
enum Held {
    /// The manifest, which an approved request removed.
    Removed(Hash),
    /// The manifest, intact.
    Intact(BlobManifest),
}

/// Calls a memory source answered, by method.
#[derive(Default)]
struct Calls {
    manifest: Cell<u64>,
    available: Cell<u64>,
    blob: Cell<u64>,
    removed: Cell<u64>,
    visit: Cell<u64>,
}

impl Calls {
    fn total(&self) -> u64 {
        self.manifest.get()
            + self.available.get()
            + self.blob.get()
            + self.removed.get()
            + self.visit.get()
    }
}

/// A publication source held in memory: no store, no file. Its values may be hostile.
struct Memory {
    pack: Pack,
    title: String,
    description: Option<String>,
    held: RefCell<BTreeMap<Hash, Held>>,
    chunk_removals: BTreeMap<Hash, Hash>,
    bytes: RefCell<BTreeMap<Hash, Vec<u8>>>,
    calls: Calls,
}

fn bump(c: &Cell<u64>) {
    c.set(c.get() + 1);
}

impl Memory {
    fn new(pack: Pack) -> Memory {
        Memory {
            pack,
            title: "Memory title".into(),
            description: Some("Memory description".into()),
            held: RefCell::new(BTreeMap::new()),
            chunk_removals: BTreeMap::new(),
            bytes: RefCell::new(BTreeMap::new()),
            calls: Calls::default(),
        }
    }

    /// The pack, settings and figure blobs of `m` copied out of `st`'s store through the store
    /// reader's own public reads, not through any publication source.
    fn copy(st: &ReadState, m: &Materialized) -> Memory {
        let reader = st.reader();
        let mut out = Memory::new(st.pack().clone());
        out.title = st.project_name();
        out.description = st.project_description();
        for b in m.iter() {
            if b.block_type != "figure" || b.is_redacted() {
                continue;
            }
            let Some(h) = b.field_str("blob").and_then(|t| t.parse::<Hash>().ok()) else {
                continue;
            };
            if !reader.has_blob(&h).unwrap() {
                continue;
            }
            if let Some(request) = reader.redaction_of(kinds::BLOB_MANIFEST, &h).unwrap() {
                out.held.borrow_mut().insert(h, Held::Removed(request));
                continue;
            }
            let manifest: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, &h).unwrap();
            let mut whole = true;
            for c in &manifest.chunks {
                if let Some(request) = reader.redaction_of(kinds::BLOB_CHUNK, c).unwrap() {
                    out.chunk_removals.insert(*c, request);
                    whole = false;
                }
            }
            if whole {
                out.bytes
                    .borrow_mut()
                    .insert(h, reader.get_blob(&h).unwrap());
            }
            out.held.borrow_mut().insert(h, Held::Intact(manifest));
        }
        out
    }

    /// Holds `bytes` as blob `h` with a manifest of length `len` (which may lie).
    fn put(&self, h: Hash, len: u64, bytes: Vec<u8>) {
        self.held.borrow_mut().insert(
            h,
            Held::Intact(BlobManifest {
                len,
                chunks: vec![Hash::of(&bytes)],
            }),
        );
        self.bytes.borrow_mut().insert(h, bytes);
    }

    fn requests(&self, h: &Hash) -> Vec<Hash> {
        match self.held.borrow().get(h) {
            None => vec![],
            Some(Held::Removed(r)) => vec![*r],
            Some(Held::Intact(m)) => m
                .chunks
                .iter()
                .filter_map(|c| self.chunk_removals.get(c).copied())
                .collect(),
        }
    }
}

impl PublicationSource for Memory {
    fn pack(&self) -> &Pack {
        &self.pack
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    fn manifest(&self, blob: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        bump(&self.calls.manifest);
        Ok(match self.held.borrow().get(blob) {
            Some(Held::Intact(m)) => Some(m.clone()),
            _ => None,
        })
    }

    fn blob_available(&self, blob: &Hash) -> Result<bool, RdyError> {
        bump(&self.calls.available);
        Ok(self.held.borrow().contains_key(blob) && self.requests(blob).is_empty())
    }

    fn blob(&self, blob: &Hash) -> Result<Vec<u8>, RdyError> {
        bump(&self.calls.blob);
        self.bytes
            .borrow()
            .get(blob)
            .cloned()
            .ok_or_else(|| RdyError::new(E_NOT_FOUND, format!("no blob {blob}"), "a memory source"))
    }

    fn removed_blob(&self, blob: &Hash) -> Result<Option<Hash>, RdyError> {
        bump(&self.calls.removed);
        Ok(self.requests(blob).first().copied())
    }

    fn visit_blob_redactions(
        &self,
        blob: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        bump(&self.calls.visit);
        for r in self.requests(blob) {
            visit(r)?;
        }
        Ok(())
    }
}

/// Every request `source` reports for `h`, in order.
fn visited(source: &dyn PublicationSource, h: &Hash) -> Vec<Hash> {
    let mut out = vec![];
    source
        .visit_blob_redactions(h, &mut |r| {
            out.push(r);
            Ok(())
        })
        .unwrap();
    out
}

/// A block for a state built in memory, outside any store.
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

fn named(mut b: Block, name: &str) -> Block {
    b.fields.insert("name".into(), s(name));
    b
}

fn with_body(mut b: Block, body: &str) -> Block {
    b.body = body.into();
    b
}

fn figure(id: &str, parent: &str, order: &str, blob: &Hash) -> Block {
    let mut b = blk(id, "figure", Some(parent), order);
    b.fields.insert("blob".into(), s(&blob.to_string()));
    b
}

fn profile_block(id: &str, key: &str, audience: &str, select: &[&str]) -> Block {
    let mut b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    b.fields.insert("key".into(), s(key));
    b.fields.insert("title".into(), s("Profile title"));
    b.fields.insert("audience".into(), s(audience));
    b.fields.insert(
        "select".into(),
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    );
    b
}

/// A state of `blocks` alone, built in memory.
fn synth(blocks: Vec<Block>) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

/// An admin doc with an org and a public profile selecting `d-PAGE1`, which holds a figure on
/// `blob` and a prose block that references the excluded doc `d-XC1D0`.
fn hostile_free_state(blob: &Hash) -> Vec<Block> {
    vec![
        named(blk("d-ADM1N", "doc", None, "a"), "admin"),
        profile_block("pf-0RG00", "orgsite", "org", &["d-PAGE1"]),
        profile_block("pf-PVB00", "pubsite", "public", &["d-PAGE1"]),
        named(blk("d-PAGE1", "doc", None, "b"), "page"),
        with_body(
            blk("p-PR0SE", "prose", Some("d-PAGE1"), "a"),
            "See {{d-XC1D0}}.",
        ),
        figure("fg-F1G00", "d-PAGE1", "b", blob),
        named(blk("d-XC1D0", "doc", None, "c"), "excluded"),
    ]
}

/// A limit refusal: the part, the limit and the unit.
type Refusal = (&'static str, u64, &'static str);

const STABLE: (&str, &str) = (
    "A publication take names its profile by a full profile id or implicit:org.",
    "Pass the site's profile id, or implicit:org.",
);

const ALWAYS_ORG: (&str, &str) = (
    "the implicit profile is always org (FW-PB9a)",
    "name an explicit publication profile whose audience is public",
);

/// A store project: folder `top` with doc `plan` (a section, two prose blocks, figures on a
/// kept blob, a removed blob and a blob that shares the removed one's two chunks), doc `other`,
/// and an admin doc with a public profile `pubsite` and an org profile `orgsite`, both
/// selecting `plan`. Returns the ids by `$` name, and the kept, removed and shared blobs.
struct Seeded {
    ids: BTreeMap<String, BlockId>,
    kept: Hash,
    removed: Hash,
    shared: Hash,
}

fn seed(r: &mut Rdy) -> Seeded {
    let kept = r.store_mut().put_blob(b"kept figure bytes").unwrap();
    // two distinct full chunks, then a blob that lists both and a tail of its own
    let mut removed_bytes = vec![7u8; 1 << 20];
    removed_bytes.extend(vec![8u8; 1 << 20]);
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let removed = r.store_mut().put_blob(&removed_bytes).unwrap();
    let shared = r.store_mut().put_blob(&shared_bytes).unwrap();
    let fig = |ph: &str, blob: &Hash| {
        json!({"op": "create", "type": "figure", "id": ph, "parent": "$d",
            "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": ph}})
    };
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$top", "fields": {"name": "top"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$top", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "CANARY title"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$s", "body": "CANARY body."},
            {"op": "create", "type": "prose", "id": "$p2", "parent": "$s", "body": "Readable sibling."},
            fig("$fk", &kept),
            fig("$fr", &removed),
            fig("$fs", &shared),
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$op", "parent": "$o", "body": "Other text."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let d = created(&w, "$d");
    let p = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$pub", "parent": created(&w, "$admin").to_string(),
             "fields": {"key": "pubsite", "title": "Public site", "audience": "public", "select": [d.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$org", "parent": created(&w, "$admin").to_string(),
             "fields": {"key": "orgsite", "title": "Org site", "audience": "org", "select": [d.to_string()]}}
        ]),
    );
    land(r, "work");
    let mut ids: BTreeMap<String, BlockId> = [
        "$top", "$d", "$s", "$p1", "$p2", "$fk", "$fr", "$fs", "$o", "$op", "$admin",
    ]
    .iter()
    .map(|n| (n.to_string(), created(&w, n)))
    .collect();
    ids.insert("$pub".into(), created(&p, "$pub"));
    ids.insert("$org".into(), created(&p, "$org"));
    Seeded {
        ids,
        kept,
        removed,
        shared,
    }
}

/// Removes the section, the first prose block and the removed blob with one approved request;
/// returns the reopened project and the request.
fn redact(dir: &tempfile::TempDir, r: Rdy, seeded: &Seeded) -> (Rdy, Hash) {
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![
                RedactTarget::AllVersions(seeded.ids["$s"].clone()),
                RedactTarget::AllVersions(seeded.ids["$p1"].clone()),
                RedactTarget::Blob(seeded.removed),
            ],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    (Rdy::open(dir.path()).unwrap(), fixture.requests[0].request)
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb2_memory_and_store_resolve_the_same_selection() {
    let (dir, mut r) = project();
    let seeded = seed(&mut r);
    let (r, _request) = redact(&dir, r, &seeded);
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let settings = Settings::of(&st);
    let store = settings.source(&st);
    let memory = Memory::copy(&st, &m);
    let pf = seeded.ids["$pub"].to_string();
    let of = seeded.ids["$org"].to_string();
    for profile in ["pubsite", pf.as_str(), "orgsite", of.as_str(), IMPLICIT_ORG] {
        let want = publication::resolve(&st, &Scope::Canon, profile).unwrap();
        let from_store = publication::resolve_source(&store, &m, profile).unwrap();
        let from_memory = publication::resolve_source(&memory, &m, profile).unwrap();
        assert_every_field(&want, &from_store, profile);
        assert_every_field(&want, &from_memory, profile);
        // the explicit profiles select one doc; the implicit one every block
        let selected = if profile == IMPLICIT_ORG {
            m.iter().count()
        } else {
            1 + 1 + 3 + 3
        };
        assert_eq!(want.entries.len(), selected, "{profile}");
        // the assets: the kept blob alone, through either source, with the same bytes
        let assets = publication::load_assets(&st, &want).unwrap();
        assert_eq!(
            assets.keys().copied().collect::<Vec<_>>(),
            vec![seeded.kept]
        );
        assert_eq!(
            publication::load_source_assets(&store, &from_store).unwrap(),
            assets
        );
        assert_eq!(
            publication::load_source_assets(&memory, &from_memory).unwrap(),
            assets
        );
    }
    // the implicit profile reads its title and description from the source
    let implicit = publication::resolve_source(&memory, &m, IMPLICIT_ORG).unwrap();
    assert_eq!(implicit.profile.title, st.project_name());
    assert_eq!(implicit.profile.description, st.project_description());
    let renamed = Memory {
        title: "Captured title".into(),
        description: Some("Captured description".into()),
        ..Memory::copy(&st, &m)
    };
    let captured = publication::resolve_source(&renamed, &m, IMPLICIT_ORG).unwrap();
    assert_eq!(captured.profile.title, "Captured title");
    assert_eq!(
        captured.profile.description.as_deref(),
        Some("Captured description")
    );
    assert_eq!(captured.selection_hash, implicit.selection_hash);
    // an explicit profile shows its own title, whatever the source's
    let named = publication::resolve_source(&renamed, &m, "pubsite").unwrap();
    assert_eq!(named.profile.title, "Public site");
    // the removal reads agree blob by blob
    let absent = Hash::of(b"a blob neither source holds");
    for h in [seeded.kept, seeded.removed, seeded.shared, absent] {
        assert_eq!(visited(&store, &h), visited(&memory, &h), "{h}");
        assert_eq!(
            store.removed_blob(&h).unwrap(),
            memory.removed_blob(&h).unwrap()
        );
        assert_eq!(
            store.blob_available(&h).unwrap(),
            memory.blob_available(&h).unwrap()
        );
        assert_eq!(store.manifest(&h).unwrap(), memory.manifest(&h).unwrap());
    }
}

#[test]
fn hpb2_profile_is_pinned_at_the_requested_snapshot() {
    let (_dir, mut r) = project();
    let seeded = seed(&mut r);
    r.tag(&founder(), "v1.0.0", None, Some("first profile".into()))
        .unwrap();
    let old_version = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&seeded.ids["$pub"]);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": seeded.ids["$pub"].to_string(),
            "fields": {"title": "New title", "select": [seeded.ids["$o"].to_string()]}}]),
    );
    land(&mut r, "work");
    let st = r.read();
    let settings = Settings::of(&st);
    let store = settings.source(&st);
    let tag = st.resolve_scope("tag:v1.0.0").unwrap();
    let tagged = st.materialized(&tag).unwrap();
    let canon = st.materialized(&Scope::Canon).unwrap();
    let memory = Memory::copy(&st, &tagged);
    let pf = seeded.ids["$pub"].to_string();
    for profile in ["pubsite", pf.as_str()] {
        let want = publication::resolve(&st, &tag, profile).unwrap();
        for (what, got) in [
            (
                "store",
                publication::resolve_source(&store, &tagged, profile).unwrap(),
            ),
            (
                "memory",
                publication::resolve_source(&memory, &tagged, profile).unwrap(),
            ),
        ] {
            assert_every_field(&want, &got, what);
            assert_eq!(got.profile.title, "Public site", "{what}");
            assert_eq!(got.profile.version, old_version, "{what}");
            assert_eq!(got.docs.len(), 1);
            assert_eq!(got.docs[0].id, seeded.ids["$d"], "{what}");
        }
        // the current profile selects the other doc; the tag keeps its own revision
        let now = publication::resolve_source(&store, &canon, profile).unwrap();
        assert_eq!(now.profile.title, "New title");
        assert_ne!(now.profile.version, old_version);
        assert_eq!(now.docs[0].id, seeded.ids["$o"]);
        assert_ne!(now.selection_hash, want.selection_hash);
    }
}

#[test]
fn hpb2_public_refusal_reads_no_asset() {
    let (_dir, mut r) = project();
    let blob = r.store_mut().put_blob(b"figure bytes").unwrap();
    let st = r.read();
    let m = synth(hostile_free_state(&blob));
    let memory = Memory::new(st.pack().clone());
    memory.put(blob, 12, b"figure bytes".to_vec());

    // public: the excluded reference blocks output before any blob byte is read
    let res = publication::resolve_source(&memory, &m, "pubsite").unwrap();
    assert_eq!(res.references.status, CheckStatus::Blocked);
    let want = res.require_output().unwrap_err();
    assert_eq!(want.code, E_PUBLICATION_REFERENCE);
    assert_eq!(memory.calls.blob.get(), 0, "resolution reads no byte");
    let (manifests, available) = (memory.calls.manifest.get(), memory.calls.available.get());
    let e = publication::load_source_assets(&memory, &res).unwrap_err();
    assert_err(&e, want.code, &want.message, &want.fix);
    assert_eq!(memory.calls.blob.get(), 0, "a blocked load reads no byte");
    assert_eq!(
        (memory.calls.manifest.get(), memory.calls.available.get()),
        (manifests, available),
        "a blocked load reads no manifest either"
    );

    // the store source and the existing reader refuse the same way, with no blob read
    let settings = Settings::of(&st);
    let store = settings.source(&st);
    let res = publication::resolve_source(&store, &m, "pubsite").unwrap();
    let reads = publication::blob_reads_for_tests();
    let e = publication::load_source_assets(&store, &res).unwrap_err();
    assert_err(&e, want.code, &want.message, &want.fix);
    let e = publication::load_assets(&st, &res).unwrap_err();
    assert_err(&e, want.code, &want.message, &want.fix);
    assert_eq!(publication::blob_reads_for_tests(), reads);

    // org: the same reference warns, and the load reads the one blob
    let res = publication::resolve_source(&memory, &m, "orgsite").unwrap();
    assert_eq!(res.references.status, CheckStatus::Warning);
    assert_eq!(
        res.references.warning().unwrap().code,
        W_PUBLICATION_REFERENCE
    );
    let assets = publication::load_source_assets(&memory, &res).unwrap();
    assert_eq!(assets, BTreeMap::from([(blob, b"figure bytes".to_vec())]));
    assert_eq!(memory.calls.blob.get(), 1);
}

#[test]
fn hpb2_removed_blocks_and_blobs_keep_identity() {
    let (dir, mut r) = project();
    let seeded = seed(&mut r);
    let st = r.read();
    let before = publication::resolve(&st, &Scope::Canon, "orgsite").unwrap();
    drop(st);
    let (r, request) = redact(&dir, r, &seeded);
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let settings = Settings::of(&st);
    let store = settings.source(&st);
    let memory = Memory::copy(&st, &m);
    let want = publication::resolve(&st, &Scope::Canon, "orgsite").unwrap();
    for (what, source) in [
        ("store", &store as &dyn PublicationSource),
        ("memory", &memory as &dyn PublicationSource),
    ] {
        let got = publication::resolve_source(source, &m, "orgsite").unwrap();
        assert_every_field(&want, &got, what);
        // every removed block keeps its original version hash and names its request
        assert_eq!(got.entries.len(), before.entries.len());
        for (a, b) in got.entries.iter().zip(&before.entries) {
            assert_eq!(a.id, b.id, "{what}");
            assert_eq!(a.version, b.version, "{what}: {} keeps its version", a.id);
            let removed = a.id == seeded.ids["$s"] || a.id == seeded.ids["$p1"];
            assert_eq!(a.redaction, removed.then_some(request), "{what}: {}", a.id);
        }
        for n in ["$s", "$p1"] {
            let stub = got.m.get(&seeded.ids[n]).unwrap();
            assert!(stub.is_redacted());
            assert_eq!(
                stub.read_identity,
                st.block(&Scope::Canon, &seeded.ids[n])
                    .unwrap()
                    .read_identity,
                "{what}: {n}"
            );
            assert_eq!(stub.redaction(), Some(request));
        }
        // both figures on the removed chunks are unavailable, the kept one is not
        let avail: BTreeMap<BlockId, (Hash, bool)> = got
            .attachments
            .iter()
            .map(|a| (a.block.clone(), (a.blob, a.available)))
            .collect();
        assert_eq!(
            avail,
            BTreeMap::from([
                (seeded.ids["$fk"].clone(), (seeded.kept, true)),
                (seeded.ids["$fr"].clone(), (seeded.removed, false)),
                (seeded.ids["$fs"].clone(), (seeded.shared, false)),
            ]),
            "{what}"
        );
        assert!(!format!("{:?} {:?}", got.entries, got.docs).contains("CANARY"));
        assert!(got.m.iter().all(|b| !format!("{b:?}").contains("CANARY")));
        // each removed blob names the request the store's own reader refuses with
        for h in [seeded.removed, seeded.shared] {
            let refused = match st.reader().get_blob(&h) {
                Err(StoreError::Redacted { request }) => request,
                other => panic!("{h}: {other:?}"),
            };
            assert_eq!(refused, request);
            assert_eq!(source.removed_blob(&h).unwrap(), Some(request), "{what}");
        }
        assert_eq!(source.removed_blob(&seeded.kept).unwrap(), None, "{what}");
        assert_eq!(
            source
                .removed_blob(&Hash::of(b"a blob no source holds"))
                .unwrap(),
            None,
            "{what}"
        );
    }
    assert_ne!(want.selection_hash, before.selection_hash);
}

#[test]
fn hpb2_all_limits_apply_before_source_allocation() {
    let (_dir, r) = project();
    let st = r.read();
    let pack = st.pack().clone();
    let fresh = || Memory::new(pack.clone());
    let base = |extra: Vec<Block>| {
        let mut blocks = vec![
            named(blk("d-ADM1N", "doc", None, "a"), "admin"),
            profile_block("pf-0RG00", "orgsite", "org", &["d-PAGE1"]),
            named(blk("d-PAGE1", "doc", None, "b"), "page"),
        ];
        blocks.extend(extra);
        synth(blocks)
    };

    // the selector bound precedes everything
    let src = fresh();
    let e = publication::resolve_source(&src, &base(vec![]), &"k".repeat(129)).unwrap_err();
    assert_limit(&e, "profile name", 128, "bytes");
    assert_eq!(src.calls.total(), 0);

    // the block-count, text, reference, depth, graph and attachment bounds: refused on the
    // snapshot before the source answers any read
    let prose = |id: &str, body: &str| with_body(blk(id, "prose", Some("d-PAGE1"), "a"), body);
    let many = |n: usize, f: &dyn Fn(usize) -> Block| (0..n).map(f).collect::<Vec<_>>();
    let id5 = |p: &str, i: usize| format!("{p}-{:0>5}", i);
    let some_blob = Hash::of(b"a hostile blob");
    let cases: Vec<(&str, Limits, Vec<Block>, Refusal)> = vec![
        (
            "selection",
            Limits {
                selection: 4,
                ..Limits::default()
            },
            many(2, &|i| prose(&id5("p", i), "x")),
            ("selection", 4, "blocks"),
        ),
        (
            "text value",
            Limits::default(),
            vec![prose("p-00000", &"x".repeat(1_048_577))],
            ("text value", 1_048_576, "bytes"),
        ),
        (
            "text",
            Limits {
                text: 10,
                ..Limits::default()
            },
            many(3, &|i| prose(&id5("p", i), "abcd")),
            ("text", 10, "bytes"),
        ),
        (
            "references",
            Limits {
                references: 2,
                ..Limits::default()
            },
            vec![prose("p-00000", "{{d-PAGE1}} {{d-PAGE1}} {{d-PAGE1}}")],
            ("references", 2, "occurrences"),
        ),
        (
            "graph",
            Limits {
                graph: 2,
                ..Limits::default()
            },
            many(3, &|i| prose(&id5("p", i), "x")),
            ("graph", 2, "edges"),
        ),
        (
            "attachments",
            Limits {
                attachments: 2,
                ..Limits::default()
            },
            many(3, &|i| figure(&id5("fg", i), "d-PAGE1", "c", &some_blob)),
            ("attachments", 2, "rows"),
        ),
    ];
    for (what, l, extra, (part, n, unit)) in cases {
        let _g = publication::override_limits_for_tests(l);
        let src = fresh();
        src.put(some_blob, 4, b"four".to_vec());
        let e = publication::resolve_source(&src, &base(extra), "orgsite").unwrap_err();
        assert_limit(&e, part, n, unit);
        assert_eq!(src.calls.total(), 0, "{what}: no source read");
    }

    // a hostile manifest length: refused on the manifests, with no availability or byte read
    let blobs: Vec<Hash> = (0..5u8).map(|i| Hash::of(&[i])).collect();
    let figures = |hs: &[Hash]| {
        hs.iter()
            .enumerate()
            .map(|(i, h)| figure(&id5("fg", i), "d-PAGE1", "c", h))
            .collect::<Vec<_>>()
    };
    let src = fresh();
    src.put(blobs[0], u64::MAX, vec![]);
    let e = publication::resolve_source(&src, &base(figures(&blobs[..1])), "orgsite").unwrap_err();
    assert_limit(&e, "blob", 67_108_864, "bytes");
    assert_eq!(
        (src.calls.available.get(), src.calls.blob.get()),
        (0, 0),
        "a hostile manifest is refused before any other read"
    );
    // five 60 MiB manifests pass the 256 MiB total with none of their bytes held
    let src = fresh();
    for h in &blobs {
        src.put(*h, 60 << 20, vec![]);
    }
    let e = publication::resolve_source(&src, &base(figures(&blobs)), "orgsite").unwrap_err();
    assert_limit(&e, "assets", 268_435_456, "bytes");
    assert_eq!((src.calls.available.get(), src.calls.blob.get()), (0, 0));

    // the load checks the manifests again before it reads a byte: a manifest that grew after
    // the resolution is refused with no blob read
    let src = fresh();
    src.put(blobs[0], 4, b"four".to_vec());
    let m = base(figures(&blobs[..1]));
    let res = publication::resolve_source(&src, &m, "orgsite").unwrap();
    assert!(res.attachments[0].available);
    src.put(blobs[0], (64 << 20) + 1, b"four".to_vec());
    let e = publication::load_source_assets(&src, &res).unwrap_err();
    assert_limit(&e, "blob", 67_108_864, "bytes");
    assert_eq!(src.calls.blob.get(), 0);
    // a source whose bytes outgrow its manifest never passes the bound either
    {
        let _g = publication::override_limits_for_tests(Limits {
            blob: 8,
            ..Limits::default()
        });
        src.put(blobs[0], 4, b"more than eight bytes".to_vec());
        let e = publication::load_source_assets(&src, &res).unwrap_err();
        assert_limit(&e, "blob", 8, "bytes");
        assert_eq!(src.calls.blob.get(), 1);
    }
    // honest values load
    src.put(blobs[0], 4, b"four".to_vec());
    assert_eq!(
        publication::load_source_assets(&src, &res).unwrap(),
        BTreeMap::from([(blobs[0], b"four".to_vec())])
    );
}

#[test]
fn hpb2_implicit_org_never_becomes_public() {
    let (_dir, r) = project();
    let st = r.read();
    let blob = Hash::of(b"bytes");
    let m = synth(hostile_free_state(&blob));
    let src = Memory::new(st.pack().clone());
    src.put(blob, 5, b"bytes".to_vec());

    let e = publication::resolve_source(&src, &m, "implicit:public").unwrap_err();
    assert_err(&e, E_BAD_INPUT, ALWAYS_ORG.0, ALWAYS_ORG.1);
    for v in ["implicit:", "implicit:all", "implicit:org "] {
        let e = publication::resolve_source(&src, &m, v).unwrap_err();
        assert_err(
            &e,
            E_BAD_INPUT,
            &format!("Implicit profile \"{v}\" is not known."),
            "Use implicit:org.",
        );
    }
    assert_eq!(src.calls.blob.get() + src.calls.available.get(), 0);

    // the one implicit selector is org, with the source's title, whatever it selects
    let res = publication::resolve_source(&src, &m, IMPLICIT_ORG).unwrap();
    assert_eq!(res.profile.audience, Audience::Org);
    assert_eq!(
        (&res.profile.id, &res.profile.version, &res.profile.key),
        (&None, &None, &None)
    );
    assert_eq!(res.profile.title, "Memory title");
    assert!(res.profile.trail);
    assert_eq!(res.entries.len(), m.iter().count());
    // every block is selected, so no reference leaves the selection
    assert_eq!(res.references.status, CheckStatus::Clear);
    // the stable selector refuses the public value with the same text
    let e = publication::check_stable_selector("implicit:public").unwrap_err();
    assert_err(&e, E_BAD_INPUT, ALWAYS_ORG.0, ALWAYS_ORG.1);
    publication::check_stable_selector(IMPLICIT_ORG).unwrap();
}

#[test]
fn hpb2_stable_selectors_share_the_take_text() {
    // full ids pass, whether or not a block has them
    for v in ["pf-ABCDE", "d-12345", "abc-0Z9YX"] {
        publication::check_stable_selector(v).unwrap();
    }
    publication::check_stable_selector(IMPLICIT_ORG).unwrap();
    // keys and malformed text name no stable profile
    for v in [
        "pubsite",
        "site-key",
        "",
        "pf-",
        "pf-abcde",
        "pf-ABCDEF",
        "pf-ABCDI",
        "abcd-ABCDE",
        " pf-ABCDE",
        "pf-ABCDE\n",
        "Implicit:org",
    ] {
        let e = publication::check_stable_selector(v).unwrap_err();
        assert_err(&e, E_BAD_INPUT, STABLE.0, STABLE.1);
    }
    // the existing implicit refusals keep their texts
    let e = publication::check_stable_selector("implicit:public").unwrap_err();
    assert_err(&e, E_BAD_INPUT, ALWAYS_ORG.0, ALWAYS_ORG.1);
    for v in ["implicit:", "implicit:all", "implicit:org "] {
        let e = publication::check_stable_selector(v).unwrap_err();
        assert_err(
            &e,
            E_BAD_INPUT,
            &format!("Implicit profile \"{v}\" is not known."),
            "Use implicit:org.",
        );
    }
    // 128 bytes are parsed; 129 bytes are refused with the take text before any parse
    let at_cap = format!("implicit:{}", "x".repeat(119));
    assert_eq!(at_cap.len(), 128);
    let e = publication::check_stable_selector(&at_cap).unwrap_err();
    assert_err(
        &e,
        E_BAD_INPUT,
        &format!("Implicit profile \"{at_cap}\" is not known."),
        "Use implicit:org.",
    );
    let over = format!("implicit:{}", "x".repeat(120));
    assert_eq!(over.len(), 129);
    let e = publication::check_stable_selector(&over).unwrap_err();
    assert_err(&e, E_BAD_INPUT, STABLE.0, STABLE.1);
    for n in [128, 129] {
        let e = publication::check_stable_selector(&"k".repeat(n)).unwrap_err();
        assert_err(&e, E_BAD_INPUT, STABLE.0, STABLE.1);
    }

    // ordinary project reads still take a profile key
    let (_dir, mut r) = project();
    seed(&mut r);
    let st = r.read();
    let res = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
    assert_eq!(res.profile.key.as_deref(), Some("pubsite"));
    let settings = Settings::of(&st);
    let m = st.materialized(&Scope::Canon).unwrap();
    publication::resolve_source(&settings.source(&st), &m, "pubsite").unwrap();
    let id = res.profile.id.unwrap().to_string();
    publication::check_stable_selector(&id).unwrap();
}

#[test]
fn hpb2_blob_redaction_visitor_reports_requests_without_payload_reads() {
    let (dir, mut r) = project();
    let seeded = seed(&mut r);
    let (r, request) = redact(&dir, r, &seeded);
    let st = r.read();
    let settings = Settings::of(&st);
    let store = settings.source(&st);
    let absent = Hash::of(b"a blob this store never held");

    // a blob no removal touches, and one the store lacks: nothing to report
    assert_eq!(visited(&store, &seeded.kept), vec![]);
    assert_eq!(visited(&store, &absent), vec![]);
    assert!(store.blob_available(&seeded.kept).unwrap());
    assert!(!store.blob_available(&absent).unwrap());
    assert_eq!(store.manifest(&absent).unwrap(), None);
    // the removed blob: its manifest's mark; the masked manifest has no chunks to walk
    assert_eq!(visited(&store, &seeded.removed), vec![request]);
    assert_eq!(store.manifest(&seeded.removed).unwrap(), None);
    assert!(!store.blob_available(&seeded.removed).unwrap());
    // the shared blob: its intact manifest, then one report per masked chunk, in manifest
    // order; the caller deduplicates
    let manifest = store.manifest(&seeded.shared).unwrap().unwrap();
    assert_eq!(manifest.chunks.len(), 3);
    assert_eq!(manifest.len, (2 << 20) + 4);
    let masked: Vec<Hash> = manifest
        .chunks
        .iter()
        .filter(|c| {
            st.reader()
                .redaction_of(kinds::BLOB_CHUNK, c)
                .unwrap()
                .is_some()
        })
        .copied()
        .collect();
    assert_eq!(masked, manifest.chunks[..2].to_vec());
    assert_eq!(visited(&store, &seeded.shared), vec![request, request]);
    assert!(!store.blob_available(&seeded.shared).unwrap());
    // no payload was read: the store refuses the masked bytes, yet every walk above finished
    assert!(matches!(
        st.reader().get_blob(&seeded.shared),
        Err(StoreError::Redacted { .. })
    ));
    assert!(matches!(
        st.reader().get_kind(kinds::BLOB_CHUNK, &manifest.chunks[0]),
        Err(StoreError::Redacted { .. })
    ));

    // the caller stops the walk at the first report: its error is returned as is
    let mut calls = 0;
    let e = store
        .visit_blob_redactions(&seeded.shared, &mut |r| {
            calls += 1;
            assert_eq!(r, request);
            Err(RdyError::new(E_LIMIT, "stop here", "caller"))
        })
        .unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (E_LIMIT, "stop here"));
    assert_eq!(calls, 1);

    // a memory copy of the same state reports the same requests in the same order
    let m = st.materialized(&Scope::Canon).unwrap();
    let memory = Memory::copy(&st, &m);
    for h in [seeded.kept, seeded.removed, seeded.shared, absent] {
        assert_eq!(visited(&memory, &h), visited(&store, &h));
    }
    assert_eq!(memory.calls.blob.get(), 0);
}
