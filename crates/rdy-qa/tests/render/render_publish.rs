//! hpb6 (D-KDFZ3, D-0VCQ5, D-QWSTV, D-QYCRB; hub D-YJ9TA, r-50C97): a publication artifact is
//! built in memory from one tagged snapshot, a publication source and the captured render
//! inputs alone. The same inputs give the same bytes whatever the store's path, canon or tension
//! state. Every generated HTML page holds one empty notice slot right after its opening main
//! element; the renderer records its byte offset while it writes the page and never searches
//! for it. The manifest covers every served file.
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::publication::{PublicationSource, ReadPublicationSource};
use rdy_engine::publication_manifest::{MANIFEST_RECIPE, NOTICE_SLOT, manifest_hash};
use rdy_engine::render_inputs::{DisagreementInput, RenderInputs, to_bounded_json};
use rdy_engine::tensions::{TensionKind, tension_label};
use rdy_engine::{E_BAD_INPUT, Materialized, Rdy, RdyError, ReadState, Scope};
use rdy_model::redaction::RedactTarget;
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::Spec;
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::golden::retired_reference;
use rdy_render::render_input::capture_inputs;
use rdy_render::{PublicationArtifact, build_publication, escape};
use rdy_store::kinds::BlobManifest;
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const TAG: &str = "v1.0.0";
const RETIRED: &str = "This page still points to something that was retired.";
const CONTRADICT: &str = "A reviewer found that these statements contradict each other.";
const ADVICE: &str = "ADVICECANARY A and C cannot both hold";

// --- helpers ------------------------------------------------------------------------------------

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Writes `ops` to `view` as the writer; panics on a refusal.
fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"));
    w.created.iter().map(|(k, v)| (k.clone(), bid(v))).collect()
}

/// A project at `at` (a directory this call creates) with `name` as its config name, or no name.
fn init_at(at: &Path, name: Option<&str>) -> Rdy {
    std::fs::create_dir_all(at).unwrap();
    drop(Rdy::init(at, "rdy-standard").expect("init"));
    let mut cfg = "pack: rdy-standard\n".to_string();
    if let Some(n) = name {
        cfg.push_str(&format!("name: {n:?}\n"));
    }
    std::fs::write(at.join(".rdy.yaml"), cfg).unwrap();
    Rdy::open(at).expect("open")
}

/// Creates and lands an `admin` doc holding one profile per (key, audience, selection).
fn profiles(r: &mut Rdy, list: &[(&str, &str, Vec<&BlockId>)]) -> BTreeMap<String, BlockId> {
    let admin = put(
        r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )["$admin"]
        .clone();
    let ops: Vec<Value> = list
        .iter()
        .map(|(key, audience, select)| {
            json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
                "parent": admin.to_string(),
                "fields": {"key": key, "title": format!("Title of {key}"),
                    "description": format!("Description of {key}."), "audience": audience,
                    "select": select.iter().map(|s| s.to_string()).collect::<Vec<_>>()}})
        })
        .collect();
    let ids = put(r, "admin", Value::Array(ops));
    land(r, "admin").expect("land the profiles");
    ids
}

/// The landing the tag `TAG` names.
fn tag_scope(read: &ReadState) -> Scope {
    let v = read
        .versions()
        .unwrap()
        .into_iter()
        .find(|v| v.name == TAG)
        .expect("the tag");
    Scope::At(v.landing.parse().unwrap())
}

/// The store source of `read`, with the captured settings of `inputs`.
fn source_of<'a>(read: &'a ReadState, inputs: &'a RenderInputs) -> ReadPublicationSource<'a> {
    ReadPublicationSource::with_settings(
        read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    )
}

/// The artifact of `selector` at the tag, from `read`'s store and `inputs`.
fn try_publish(
    read: &ReadState,
    inputs: &RenderInputs,
    selector: &str,
) -> Result<PublicationArtifact, RdyError> {
    let m = read.materialized(&tag_scope(read)).unwrap();
    build_publication(&source_of(read, inputs), &m, TAG, false, selector, inputs)
}

fn publish(read: &ReadState, inputs: &RenderInputs, selector: &str) -> PublicationArtifact {
    try_publish(read, inputs, selector).unwrap_or_else(|e| panic!("{selector}: {e}"))
}

/// The captured inputs of the tag.
fn inputs_of(read: &ReadState) -> RenderInputs {
    capture_inputs(read, &tag_scope(read)).expect("the inputs")
}

/// Everything of an artifact a reader or the hub can tell apart.
fn same(a: &PublicationArtifact, b: &PublicationArtifact, what: &str) {
    assert_eq!(
        a.files.keys().collect::<Vec<_>>(),
        b.files.keys().collect::<Vec<_>>(),
        "{what}: paths"
    );
    for (path, bytes) in &a.files {
        assert!(b.files[path] == *bytes, "{what}: {path} differs");
    }
    assert_eq!(a.manifest, b.manifest, "{what}: manifest");
    assert_eq!(a.manifest_hash, b.manifest_hash, "{what}: manifest hash");
    assert_eq!(a.warnings, b.warnings, "{what}: warnings");
}

fn text(a: &PublicationArtifact, path: &str) -> String {
    String::from_utf8(
        a.files
            .get(path)
            .unwrap_or_else(|| panic!("no {path}: {:?}", a.files.keys()))
            .clone(),
    )
    .unwrap()
}

/// The manifest's notice offset of `path`.
fn offset(a: &PublicationArtifact, path: &str) -> Option<u64> {
    a.manifest
        .files
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("{path} is not in the manifest"))
        .notice_offset
}

/// Checks that every HTML file has one slot, at its recorded offset, right after the opening
/// main element, and that no other file has an offset.
#[track_caller]
fn check_slots(a: &PublicationArtifact) {
    for (path, bytes) in &a.files {
        let at = offset(a, path);
        if !path.ends_with(".html") {
            assert_eq!(at, None, "{path}");
            continue;
        }
        let at = at.unwrap_or_else(|| panic!("{path} has no notice slot")) as usize;
        assert_eq!(&bytes[at..at + NOTICE_SLOT.len()], NOTICE_SLOT, "{path}");
        let html = String::from_utf8(bytes.clone()).unwrap();
        // a check of the test only: the renderer records the offset while it writes the page
        let main = html.find("<main ").expect("a main element");
        let open = main + html[main..].find('>').unwrap() + 1;
        assert_eq!(
            at, open,
            "{path}: the slot follows the opening main element"
        );
        let slot = std::str::from_utf8(NOTICE_SLOT).unwrap();
        assert_eq!(html.matches(slot).count(), 1, "{path}: one slot");
    }
}

/// A PNG of `w` by `h` pixels of one colour.
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &d in data {
            a = (a + d as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut line = vec![0u8];
    for _ in 0..w {
        line.extend_from_slice(&rgb);
    }
    let raw = line.repeat(h as usize);
    let mut z = vec![0x78, 0x01, 1];
    z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Copies the closed project at `from` to `to`, file by file.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_dir(&src, &dst);
        } else if e.file_type().unwrap().is_file() {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// Applies one approved redaction of `targets` to the closed project at `dir`, then opens it.
fn redact(dir: &Path, targets: Vec<RedactTarget>) -> Rdy {
    authorized_batch_fixture(
        &dir.join(".rdy/store"),
        &[RequestSpec::new("redact", targets)],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    Rdy::open(dir).unwrap()
}

/// A judged contradiction between claims `a` and `c` of `Spec`, with raw advice and a reason.
fn contradiction(r: &mut Rdy, s: &Spec) {
    use rdy_model::*;
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let content = |id: &BlockId| m.get(id).unwrap().content_hash_with(r.store().tracking());
    let mut items = vec![
        PairItem {
            block: s.a.clone(),
            content: content(&s.a),
        },
        PairItem {
            block: s.c.clone(),
            content: content(&s.c),
        },
    ];
    items.sort();
    let provenance = Provenance {
        actor: "agent:r".into(),
        run: Some("r".into()),
        role: Role::Reviewer,
        at_ms: 1,
    };
    let review = Review {
        subject: ReviewSubject::Pair {
            key: pair_key(&items),
            items: items.clone(),
        },
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: "REASONCANARY review reason".into(),
        citations: vec![],
        provenance: provenance.clone(),
    };
    let tension = SemanticTension {
        key: pair_key(&items),
        items,
        review: Hash::of(&postcard::to_allocvec(&review).unwrap()),
        advice: Some(ADVICE.into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance,
    };
    drop(m);
    r.record_verdict(&review, Some(&tension)).unwrap();
}

/// The retired-reference project with a judged contradiction, three profiles (the notes doc,
/// the spec doc, both) and the tag `TAG` at canon's head: both disagreements are current.
fn disagreeing() -> (
    tempfile::TempDir,
    Rdy,
    Spec,
    BlockId,
    BTreeMap<String, BlockId>,
) {
    let (dir, mut r, spec) = retired_reference();
    contradiction(&mut r, &spec);
    let notes = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(None)
        .into_iter()
        .find(|b| b.field_str("name") == Some("notes"))
        .map(|b| b.id.clone())
        .expect("the notes doc");
    let p = profiles(
        &mut r,
        &[
            ("notesonly", "org", vec![&notes]),
            ("speconly", "org", vec![&spec.doc]),
            ("both", "org", vec![&notes, &spec.doc]),
        ],
    );
    r.tag(&approver(), TAG, None, Some("TAGMESSAGECANARY".into()))
        .unwrap();
    (dir, r, spec, notes, p)
}

/// A memory copy of what `src` answers for the figure blobs of `m`: no store is open behind it.
struct Memory {
    pack: rdy_schema::Pack,
    title: String,
    description: Option<String>,
    blobs: BTreeMap<Hash, Held>,
}

struct Held {
    manifest: Option<BlobManifest>,
    bytes: Option<Vec<u8>>,
    requests: Vec<Hash>,
}

impl Memory {
    fn copy(src: &dyn PublicationSource, m: &Materialized) -> Memory {
        let mut blobs = BTreeMap::new();
        for b in m.iter() {
            let Some(h) = b.field_str("blob").and_then(|h| h.parse::<Hash>().ok()) else {
                continue;
            };
            let mut requests = vec![];
            src.visit_blob_redactions(&h, &mut |r| {
                requests.push(r);
                Ok(())
            })
            .unwrap();
            let available = src.blob_available(&h).unwrap();
            blobs.insert(
                h,
                Held {
                    manifest: src.manifest(&h).unwrap(),
                    bytes: available.then(|| src.blob(&h).unwrap()),
                    requests,
                },
            );
        }
        Memory {
            pack: src.pack().clone(),
            title: src.title().to_string(),
            description: src.description().map(str::to_string),
            blobs,
        }
    }
}

impl PublicationSource for Memory {
    fn pack(&self) -> &rdy_schema::Pack {
        &self.pack
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    fn manifest(&self, blob: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        Ok(self.blobs.get(blob).and_then(|h| h.manifest.clone()))
    }
    fn blob_available(&self, blob: &Hash) -> Result<bool, RdyError> {
        Ok(self.blobs.get(blob).is_some_and(|h| h.bytes.is_some()))
    }
    fn blob(&self, blob: &Hash) -> Result<Vec<u8>, RdyError> {
        Ok(self.blobs[blob].bytes.clone().expect("an available blob"))
    }
    fn removed_blob(&self, blob: &Hash) -> Result<Option<Hash>, RdyError> {
        Ok(self
            .blobs
            .get(blob)
            .and_then(|h| h.requests.first().copied()))
    }
    fn visit_blob_redactions(
        &self,
        blob: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        for r in self
            .blobs
            .get(blob)
            .map(|h| h.requests.clone())
            .unwrap_or_default()
        {
            visit(r)?;
        }
        Ok(())
    }
}

// --- tests --------------------------------------------------------------------------------------

#[test]
fn hpb6_store_and_memory_build_identical_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory-proj");
    let mut r = init_at(&root, Some("Memory project"));
    let kept_png = png(5, 3, [10, 120, 200]);
    let gone_png = png(4, 7, [201, 33, 44]);
    let kept = r.store_mut().put_blob(&kept_png).unwrap();
    let gone = r.store_mut().put_blob(&gone_png).unwrap();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$op", "parent": "$other", "body": "Other words."},
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area"}},
            {"op": "create", "type": "doc", "id": "$main", "parent": "$area", "fields": {"name": "main", "title": "Main page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ms", "parent": "$main", "fields": {"title": "Main section"}},
            {"op": "create", "type": "prose", "parent": "$ms", "body": "Main words point at {{$op}}."},
            {"op": "create", "type": "figure", "parent": "$ms", "fields": {"blob": kept.to_string(), "media_type": "image/png", "alt": "Kept figure"}},
            {"op": "create", "type": "figure", "parent": "$ms", "fields": {"blob": gone.to_string(), "media_type": "image/png; charset=binary", "alt": "Gone figure"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let p = profiles(&mut r, &[("mainonly", "org", vec![&ids["$main"]])]);
    r.tag(&approver(), TAG, None, None).unwrap();
    r.close().unwrap();
    let r = redact(&root, vec![RedactTarget::Blob(gone)]);
    let read = r.read();
    let inputs = inputs_of(&read);
    let m = read.materialized(&tag_scope(&read)).unwrap();
    let store = source_of(&read, &inputs);
    let memory = Memory::copy(&store, &m);
    assert!(memory.blobs[&kept].bytes.is_some() && memory.blobs[&gone].bytes.is_none());
    let explicit = p["$mainonly"].to_string();
    for selector in [explicit.as_str(), "implicit:org"] {
        let a = build_publication(&store, &m, TAG, false, selector, &inputs).unwrap();
        let b = build_publication(&memory, &m, TAG, false, selector, &inputs).unwrap();
        same(&a, &b, selector);
        check_slots(&a);
        // the kept figure is served, the removed one is not
        assert!(a.files.values().any(|f| *f == kept_png), "{selector}");
        assert!(!a.files.values().any(|f| *f == gone_png), "{selector}");
    }
    // the org profile's reference warning comes first and is the same on both paths
    let a = build_publication(&store, &m, TAG, false, &explicit, &inputs).unwrap();
    assert_eq!(a.warnings.len(), 1, "{:?}", a.warnings);
    assert!(a.warnings[0].contains("outside"), "{:?}", a.warnings);

    // a source whose effective pack differs from the inputs' is refused, with no fallback
    let mut other = inputs.clone();
    other.pack.name = format!("{}-other", other.pack.name);
    let e = build_publication(&store, &m, TAG, false, "implicit:org", &other).expect_err("refused");
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}

#[test]
fn hpb6_source_title_survives_arbitrary_paths() {
    let scratch_names = ["scratch-one", "zz_other_place", "Third dir"];
    let base = tempfile::tempdir().unwrap();
    // a project, its content, a profile and the tag
    let build = |r: &mut Rdy| -> BlockId {
        let ids = put(
            r,
            "w1",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$d", "body": "Guide words."}
            ]),
        );
        land(r, "w1").unwrap();
        let p = profiles(r, &[("guidep", "org", vec![&ids["$d"]])]);
        r.tag(&approver(), TAG, None, None).unwrap();
        p["$guidep"].clone()
    };
    // the same project copied under each scratch name, opened there
    let replay = |from: &Path, tag: &str| -> Vec<(tempfile::TempDir, Rdy)> {
        scratch_names
            .iter()
            .map(|n| {
                let d = tempfile::tempdir().unwrap();
                let at = d.path().join(format!("{n}-{tag}"));
                copy_dir(from, &at);
                (d, Rdy::open(&at).unwrap())
            })
            .collect()
    };

    // a named project: its title and the profile's survive every path
    let named = base.path().join("named-source");
    let mut r = init_at(&named, Some("Named ☃ project"));
    let profile = build(&mut r).to_string();
    let inputs = inputs_of(&r.read());
    assert_eq!(inputs.title, "Named ☃ project");
    r.close().unwrap();
    let copies = replay(&named, "named");
    let first = (
        publish(&copies[0].1.read(), &inputs, "implicit:org"),
        publish(&copies[0].1.read(), &inputs, &profile),
    );
    for (_d, c) in &copies {
        let read = c.read();
        // the config names the project, so every copy reads the same name
        assert_eq!(read.project_name(), "Named ☃ project");
        let implicit = publish(&read, &inputs, "implicit:org");
        let explicit = publish(&read, &inputs, &profile);
        same(&implicit, &first.0, "named implicit");
        same(&explicit, &first.1, "named explicit");
        assert!(text(&implicit, "index.html").contains("Named ☃ project"));
        assert!(text(&explicit, "index.html").contains("Title of guidep"));
        for a in [&implicit, &explicit] {
            assert_eq!(a.warnings, Vec::<String>::new(), "no unset-name warning");
        }
    }

    // two identical unnamed clones under two source names: their captured titles follow the
    // directory fallback and differ, so their implicit artifacts differ
    let alpha = base.path().join("alpha-clone");
    let mut r = init_at(&alpha, None);
    let profile = build(&mut r).to_string();
    r.close().unwrap();
    let beta = base.path().join("beta_clone");
    copy_dir(&alpha, &beta);
    let captured: Vec<RenderInputs> = [&alpha, &beta]
        .iter()
        .map(|p| inputs_of(&Rdy::open(p).unwrap().read()))
        .collect();
    assert_eq!(captured[0].title, "Alpha clone");
    assert_eq!(captured[1].title, "Beta clone");
    let mut implicit_hashes = vec![];
    let mut explicit_all = vec![];
    for (inputs, source) in captured.iter().zip([&alpha, &beta]) {
        let copies = replay(source, "clone");
        let first = publish(&copies[0].1.read(), inputs, "implicit:org");
        assert!(text(&first, "index.html").contains(&inputs.title));
        for (_d, c) in &copies {
            let read = c.read();
            // the copy's own directory name gives it another fallback title, which no byte reads
            assert_ne!(read.project_name(), inputs.title);
            let implicit = publish(&read, inputs, "implicit:org");
            same(&implicit, &first, &inputs.title);
            for (path, bytes) in &implicit.files {
                let t = String::from_utf8_lossy(bytes);
                assert!(!t.contains(&read.project_name()), "{path}");
            }
            assert_eq!(implicit.warnings, Vec::<String>::new());
            let explicit = publish(&read, inputs, &profile);
            assert_eq!(explicit.warnings, Vec::<String>::new());
            explicit_all.push(explicit);
        }
        implicit_hashes.push(first.manifest_hash);
    }
    assert_ne!(implicit_hashes[0], implicit_hashes[1]);
    // the explicit profile's title is its own: equal from either captured input
    for e in &explicit_all {
        same(e, &explicit_all[0], "explicit from either clone");
        assert!(text(e, "index.html").contains("Title of guidep"));
    }
}

#[test]
fn hpb6_recorded_offsets_ignore_authored_marker_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = init_at(
        &dir.path().join("marker"),
        Some("Ünïcödé <!--rdy-notices--> ☃ site"),
    );
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "page", "title": "Pâge <!--rdy-notices--> tïtle", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Séction"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": "Authored <!--rdy-notices--> marker and `<!--rdy-notices-->` code."},
            {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": "html"}, "body": "<main><!--rdy-notices--></main>"}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let _ = ids;
    r.tag(&approver(), TAG, None, None).unwrap();
    let read = r.read();
    let inputs = inputs_of(&read);
    let a = publish(&read, &inputs, "implicit:org");
    check_slots(&a);
    let page = text(&a, "page.html");
    let at = offset(&a, "page.html").unwrap() as usize;
    // non-ASCII text comes before the slot, and the authored marker text after it
    assert!(!page[..at].is_ascii(), "multi-byte text precedes the slot");
    assert!(page[..at].contains("Ünïcödé"));
    assert!(page[at + NOTICE_SLOT.len()..].contains("rdy-notices"));
    assert!(page.matches("rdy-notices").count() >= 4, "{page}");
}

#[test]
fn hpb6_cached_redirect_offsets_follow_utf8_substitutions() {
    let dir = tempfile::tempdir().unwrap();
    let site = "Sïte ☃ «name»";
    let mut r = init_at(&dir.path().join("redirects"), Some(site));
    // labels and hrefs of differing lengths, with multi-byte characters and escaped bytes
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "ff", "title": "Ff"}},
            {"op": "create", "type": "doc", "id": "$b", "parent": "$f", "fields": {"name": "bb", "title": "Bé & <longer> title — ☃☃", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$g", "parent": "$f", "fields": {"name": "ggg-deeper", "title": "Ggg"}},
            {"op": "create", "type": "doc", "id": "$c", "parent": "$g", "fields": {"name": "c", "title": "日本語のとても長いタイトル", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    r.tag(&approver(), TAG, None, None).unwrap();
    let read = r.read();
    let inputs = inputs_of(&read);
    let a = publish(&read, &inputs, "implicit:org");
    check_slots(&a);

    // the full template, rendered by substitution here
    let template = include_str!("../../../rdy-render/templates/redirect.html").trim_end();
    let slot_tpl = "{% if notice_slot is defined %}{{ notice_slot }}{% endif %}";
    assert_eq!(template.matches(slot_tpl).count(), 1, "{template}");
    let render = |href: &str, label: &str| -> (String, usize) {
        let fill = |t: &str| {
            t.replace("{{ body.redirect.href }}", &escape(href))
                .replace("{{ body.redirect.label }}", &escape(label))
                .replace("{{ site_name }}", &escape(site))
        };
        let (before, after) = template.split_once(slot_tpl).unwrap();
        let before = fill(before);
        let at = before.len();
        let slot = std::str::from_utf8(NOTICE_SLOT).unwrap();
        (format!("{before}{slot}{}", fill(after)), at)
    };
    let mut offsets = vec![];
    for (key, href, label) in [
        ("$a", "../a.html", "A"),
        ("$b", "../ff/bb.html", "Bé & <longer> title — ☃☃"),
        (
            "$c",
            "../ff/ggg-deeper/c.html",
            "日本語のとても長いタイトル",
        ),
    ] {
        let path = format!("go/{}.html", ids[key]);
        let (want, at) = render(href, label);
        let got = text(&a, &path);
        assert_eq!(got.trim_end(), want, "{path}");
        assert_eq!(offset(&a, &path), Some(at as u64), "{path}");
        offsets.push(at);
    }
    // each substitution before the slot moved it by its own byte length
    assert!(
        offsets[0] < offsets[1] && offsets[1] < offsets[2],
        "{offsets:?}"
    );
}

#[test]
fn hpb6_every_html_page_has_one_slot() {
    let (_dir, mut r, spec, _notes, _p) = disagreeing();
    // a folder, a term and an open question beside the disagreements; then the tag again at head
    put(
        &mut r,
        "more",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area", "title": "Area", "description": "Area words."}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "inner", "title": "Inner", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Inner section"}},
            {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "The ledger term."},
            {"op": "create", "type": "question", "parent": "$s", "fields": {"prompt": "Is it open?"}}
        ]),
    );
    land(&mut r, "more").unwrap();
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    let read = r.read();
    let v2 = read
        .versions()
        .unwrap()
        .into_iter()
        .find(|v| v.name == "v2.0.0")
        .unwrap();
    let scope = Scope::At(v2.landing.parse().unwrap());
    let inputs = capture_inputs(&read, &scope).unwrap();
    assert_eq!(inputs.disagreements.len(), 2, "{:?}", inputs.disagreements);
    let m = read.materialized(&scope).unwrap();
    let a = build_publication(
        &source_of(&read, &inputs),
        &m,
        "v2.0.0",
        false,
        "implicit:org",
        &inputs,
    )
    .unwrap();
    check_slots(&a);
    for path in [
        "index.html",
        "area/index.html",
        "area/inner.html",
        "glossary.html",
        "questions.html",
        "disagreements.html",
        &format!("go/{}.html", spec.doc),
        &format!("go/{}.html", spec.a),
    ] {
        assert!(offset(&a, path).is_some(), "{path}: {:?}", a.files.keys());
    }
    let d = text(&a, "disagreements.html");
    assert!(d.contains(RETIRED) && d.contains(CONTRADICT), "{d}");
    // the publication shows only the supplied tag label: no tag message, other version or link
    // to the source's history
    for (path, bytes) in a.files.iter().filter(|(p, _)| !p.starts_with("assets/")) {
        let t = String::from_utf8_lossy(bytes);
        for word in ["TAGMESSAGECANARY", "v1.0.0", "changes-since", "~versions"] {
            assert!(!t.contains(word), "{path} holds {word}");
        }
    }
}

#[test]
fn hpb6_same_inputs_survive_canon_moves_and_dismissals() {
    let (_dir, mut r, _spec, _notes, p) = disagreeing();
    let both = p["$both"].to_string();
    let read = r.read();
    let inputs = inputs_of(&read);
    assert_eq!(
        inputs
            .disagreements
            .iter()
            .map(|d| d.sentence.as_str())
            .collect::<Vec<_>>()
            .len(),
        2
    );
    // the projection holds the fixed sentences and block ids alone: no summary, advice, reason,
    // status, actor or time
    let json = String::from_utf8(to_bounded_json(&inputs).unwrap()).unwrap();
    for word in [
        "ADVICECANARY",
        "REASONCANARY",
        "references deprecated",
        "agent:r",
        "Open",
        "TAGMESSAGECANARY",
    ] {
        assert!(!json.contains(word), "the inputs hold {word}");
    }
    let before: Vec<PublicationArtifact> = [both.as_str(), "implicit:org"]
        .iter()
        .map(|s| publish(&read, &inputs, s))
        .collect();
    drop(read);
    assert!(text(&before[0], "disagreements.html").contains(RETIRED));

    // canon moves past the tag, and the retired reference is dismissed
    put(
        &mut r,
        "later",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later", "doc_type": "page"}}]),
    );
    land(&mut r, "later").unwrap();
    let t = r
        .read()
        .tensions()
        .unwrap()
        .iter()
        .find(|t| t.kind == TensionKind::DeprecatedReference)
        .cloned()
        .expect("the retired reference");
    r.dismiss_tension(&writer(), &tension_label(&t.id), "fine as it is")
        .unwrap();
    let read = r.read();
    // a fresh capture at the tag sees none of them now: the tag is no longer canon's head
    assert_eq!(inputs_of(&read).disagreements, vec![]);
    // the captured inputs replay the same bytes
    for (selector, was) in [both.as_str(), "implicit:org"].iter().zip(&before) {
        same(&publish(&read, &inputs, selector), was, selector);
    }
}

#[test]
fn hpb6_profile_filters_disagreement_projection() {
    let (_dir, r, spec, notes, p) = disagreeing();
    let read = r.read();
    let inputs = inputs_of(&read);
    let m = read.materialized(&tag_scope(&read)).unwrap();
    let claim = m
        .iter()
        .find(|b| b.block_type == "claim" && m.doc_of(&b.id).is_some_and(|d| d.id == notes))
        .map(|b| b.id.clone())
        .expect("the notes claim");
    let pages = |a: &PublicationArtifact| -> Option<String> {
        a.files
            .contains_key("disagreements.html")
            .then(|| text(a, "disagreements.html"))
    };
    let holds = |a: &PublicationArtifact, word: &str| -> Vec<String> {
        a.files
            .iter()
            .filter(|(path, b)| {
                // the theme's own assets are not the selection's
                !(path.starts_with("assets/") && !path.ends_with(".png"))
                    && String::from_utf8_lossy(b).contains(word)
            })
            .map(|(path, _)| path.clone())
            .collect()
    };

    // one side of the retired reference, none of the contradiction: no entry, no page
    let one = publish(&read, &inputs, &p["$notesonly"].to_string());
    assert_eq!(pages(&one), None);
    for word in [RETIRED, CONTRADICT, "disagreements"] {
        assert_eq!(holds(&one, word), Vec::<String>::new(), "{word}");
    }
    for excluded in [&spec.a, &spec.b, &spec.c, &spec.doc] {
        assert_eq!(
            holds(&one, &excluded.to_string()),
            Vec::<String>::new(),
            "{excluded}"
        );
    }
    // the spec alone: the contradiction (both sides in the spec) stays
    let other = publish(&read, &inputs, &p["$speconly"].to_string());
    let d = pages(&other).expect("the page");
    assert!(d.contains(CONTRADICT) && !d.contains(RETIRED), "{d}");
    // every side selected: both captured entries, each once. The page lists them in the docs'
    // reading order (Site::set_disagreements), which the capture order need not follow.
    let both = publish(&read, &inputs, &p["$both"].to_string());
    let d = pages(&both).expect("the page");
    assert_eq!(inputs.disagreements.len(), 2, "guard: two captured entries");
    for row in &inputs.disagreements {
        assert_eq!(d.matches(row.sentence.as_str()).count(), 1, "{d}");
    }
    for sentence in [RETIRED, CONTRADICT] {
        assert_eq!(d.matches(sentence).count(), 1, "{sentence}: {d}");
    }

    // supplied rows are filtered side by side: a row with an excluded side is dropped, a row
    // whose every side is selected is kept
    let mut supplied = inputs.clone();
    supplied.disagreements = vec![
        DisagreementInput {
            sentence: "These pages give two versions of one point.".into(),
            blocks: vec![claim.clone(), spec.a.clone()],
            lead: None,
        },
        DisagreementInput {
            sentence: "These pages define the same thing twice.".into(),
            blocks: vec![claim.clone()],
            lead: None,
        },
    ];
    let one = publish(&read, &supplied, &p["$notesonly"].to_string());
    let d = pages(&one).expect("the kept row");
    assert!(
        d.contains("These pages define the same thing twice."),
        "{d}"
    );
    assert!(
        !d.contains("These pages give two versions of one point."),
        "{d}"
    );
    assert_eq!(holds(&one, &spec.a.to_string()), Vec::<String>::new());
}

#[test]
fn hpb6_manifest_covers_every_served_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = init_at(&dir.path().join("manifest"), Some("Manifest site"));
    let fig = png(3, 2, [1, 2, 3]);
    let blob = r.store_mut().put_blob(&fig).unwrap();
    let pdf = r.store_mut().put_blob(b"%PDF-1.4 download").unwrap();
    put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "page", "title": "Page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Words."},
            {"op": "create", "type": "diagram", "parent": "$d", "fields": {"lang": "mermaid"}, "body": "graph TD; A-->B"},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "Fig"}},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": pdf.to_string(), "media_type": "application/pdf", "alt": "Download"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    r.tag(&approver(), TAG, None, None).unwrap();
    let read = r.read();
    let a = publish(&read, &inputs_of(&read), "implicit:org");
    // one entry per served file, in the files' order, and nothing else (no manifest file)
    assert_eq!(
        a.manifest
            .files
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        a.files.keys().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(!a.files.contains_key("manifest.json"));
    let media = |path: &str| -> &str {
        match path.rsplit_once('.').map(|(_, e)| e) {
            Some("html") => "text/html",
            Some("txt") => "text/plain",
            Some("json") => "application/json",
            Some("css") => "text/css",
            Some("js") => "application/javascript",
            Some("woff2") => "font/woff2",
            Some("png") => "image/png",
            Some("pdf") => "application/pdf",
            // generated: a drawn diagram's file only
            Some("svg") if path.starts_with("assets/diagrams/") => "image/svg+xml",
            other => panic!("{path}: {other:?}"),
        }
    };
    for (e, (path, bytes)) in a.manifest.files.iter().zip(&a.files) {
        assert_eq!(e.hash, Hash::of(bytes), "{path}");
        assert_eq!(e.size, bytes.len() as u64, "{path}");
        assert_eq!(e.media_type, media(path), "{path}");
        assert_eq!(
            e.notice_offset.is_some(),
            e.media_type == "text/html",
            "{path}"
        );
    }
    let drawn: Vec<&String> = a
        .files
        .keys()
        .filter(|p| p.starts_with("assets/diagrams/"))
        .collect();
    assert_eq!(drawn.len(), 1, "{:?}", a.files.keys());
    assert_eq!(
        drawn[0].as_str(),
        format!("assets/diagrams/{}.svg", Hash::of(&a.files[drawn[0]]))
    );
    assert!(!a.files.keys().any(|p| p.starts_with("assets/js/")));
    for want in [
        "assets/site.css",
        "assets/app.js",
        drawn[0].as_str(),
        "assets/fonts/onest.woff2",
        "search-index.json",
        "llms.txt",
        "llms-full.txt",
        &format!("assets/{blob}.png"),
        &format!("assets/{pdf}.pdf"),
    ] {
        assert!(a.files.contains_key(want), "{want}: {:?}", a.files.keys());
    }
    // the recipe-1 hash of the manifest, which holds no file of its own
    assert_eq!(MANIFEST_RECIPE, 1);
    assert_eq!(manifest_hash(&a.manifest, 1).unwrap(), a.manifest_hash);
    check_slots(&a);
}
