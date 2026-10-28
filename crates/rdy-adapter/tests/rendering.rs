//! hpb13 (D-KDFZ3, D-48NYM, D-0VCQ5, D-QYCRB, D-5X0ZK, D-WDKWQ; hub i-FGX7D, i-N0G2N, r-B0ZYE,
//! r-7GS51, n-CT2GB): publication rendering through the adapter. The renderer, the recipe and the
//! process are core's, so every result here is compared with the core call it stands for. The
//! source project is built with core calls and closed to adapter handles; holders take its
//! uploads through the adapter.

use rdy_adapter::rendering::{
    Manifest, ManifestEntry, PublicationArtifact, RenderFormat, RenderInputs, RenderedPage,
    manifest_hash, read_render_artifact, run_render_process,
};
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{Hash, RdyError, Snapshot, StoreHandle};
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::{Provenance, Role};
use rdy_render::probe;
use rdy_render::process::write_request_prelude;
use rdy_render::render_input::capture_inputs;
use rdy_render::{build_publication, render_publication_page};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

const CANARY: &str = "ZEBRA-7731";
const INTRO_WORDS: &str = "INTROWORDS";
const OTHER_WORDS: &str = "OTHERWORDS";
const TITLE: &str = "Explicit source title";
const UNSTABLE: (&str, &str) = (
    "A publication take names its profile by a full profile id or implicit:org.",
    "Pass the site's profile id, or implicit:org.",
);

// ---------------------------------------------------------------------------------------------
// the source project (core)

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

fn write(r: &mut Rdy, ops: Value) -> BTreeMap<String, String> {
    let w = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "hpb13".into(),
            },
        )
        .unwrap();
    w.created.into_iter().collect()
}

fn land(r: &mut Rdy, title: &str) {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

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

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

struct Source {
    r: Rdy,
    /// The org profile's block id: it selects `intro` alone.
    profile: String,
    /// The block that holds the canary.
    secret: String,
    v1: Hash,
    l1: Hash,
}

/// A project named `Source project`: docs `intro` and `other`, an admin doc with an org profile
/// selecting `intro` alone, and a prose block in `intro` that holds the canary. Tagged `v1.0.0`.
fn source(dir: &Path) -> Source {
    std::fs::create_dir_all(dir).unwrap();
    drop(Rdy::init(dir, "rdy-standard").unwrap());
    std::fs::write(
        dir.join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"Source project\"\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir).unwrap();
    let ids = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": format!("{INTRO_WORDS} here.")},
            {"op": "create", "type": "prose", "id": "$secret", "parent": "$intro", "body": format!("The launch code is {CANARY}.")},
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$op", "parent": "$other", "body": format!("{OTHER_WORDS} there.")},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "first");
    let p = write(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$orgp", "parent": ids["$admin"],
             "fields": {"key": "orgp", "title": "Org site", "audience": "org", "select": [ids["$intro"]]}}
        ]),
    );
    land(&mut r, "profile");
    let v1 = tag(&mut r, "v1.0.0");
    let l1 = r.store().read_tag(&v1).unwrap().landing;
    Source {
        r,
        profile: p["$orgp"].clone(),
        secret: ids["$secret"].clone(),
        v1,
        l1,
    }
}

impl Source {
    /// The explicit render inputs of a caller that is not the source: its own title and pack.
    fn inputs(&self) -> RenderInputs {
        let mut inputs = capture_inputs(&self.r.read(), &Scope::At(self.l1)).unwrap();
        assert_eq!(inputs.title, "Source project");
        inputs.title = TITLE.into();
        inputs.description = Some("An explicit description.".into());
        inputs.pack.version = "9.9.9-custom".into();
        inputs
    }

    fn export(&self, tag: Hash) -> Vec<u8> {
        let mut out = Vec::new();
        stream_export::write(&self.r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
        out
    }

    /// What the source project itself renders at `path`.
    fn page(&self, profile: &str, inputs: &RenderInputs, path: &str) -> RenderedPage {
        let read = self.r.read();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(&Scope::At(self.l1)).unwrap();
        render_publication_page(&source, &m, "v1.0.0", false, profile, inputs, path).unwrap()
    }

    /// What the source project itself builds.
    fn site(&self, profile: &str, inputs: &RenderInputs) -> PublicationArtifact {
        let read = self.r.read();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(&Scope::At(self.l1)).unwrap();
        build_publication(&source, &m, "v1.0.0", false, profile, inputs).unwrap()
    }

    /// A holder at `root` of the source's repo that has taken the upload of `v1.0.0`.
    fn holder(&self, root: &Path, scratch: &Path) -> StoreHandle {
        let repo = self.r.store().repo_id().unwrap().unwrap();
        let mut h = StoreHandle::create_holder(root, repo).unwrap();
        take(
            &mut h,
            scratch,
            "v1",
            &self.export(self.v1),
            "v1.0.0",
            self.l1,
        );
        h
    }
}

fn take(h: &mut StoreHandle, scratch: &Path, name: &str, bytes: &[u8], tag: &str, landing: Hash) {
    let p = scratch.join(format!("{name}.rdysync"));
    std::fs::write(&p, bytes).unwrap();
    h.take(
        &[File::open(&p).unwrap()],
        UploadTarget {
            tag: tag.into(),
            landing,
        },
    )
    .unwrap();
}

fn request(
    snap: &Snapshot,
    tag: &str,
    profile: &str,
    inputs: &RenderInputs,
    format: RenderFormat,
) -> Result<Vec<u8>, RdyError> {
    let mut out = Vec::new();
    snap.write_render_request(tag, profile, inputs, format, &mut out)
        .map(|_| out)
}

/// The artifact the render process makes of `request`.
fn process(request: &[u8]) -> Result<PublicationArtifact, RdyError> {
    let mut packet = Vec::new();
    run_render_process(request, &mut packet)?;
    read_render_artifact(packet.as_slice())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[track_caller]
fn same_error(a: &RdyError, b: &RdyError) {
    assert_eq!(
        (a.code, &a.message, &a.fix, format!("{:?}", a.issues)),
        (b.code, &b.message, &b.fix, format!("{:?}", b.issues))
    );
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb13_holder_request_renders_like_the_source_project() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let inputs = src.inputs();
    let holder = src.holder(&dir.path().join("holder"), dir.path());
    let snap = holder.snapshot().unwrap();

    // one page: the source project's own bytes, whether the tag is named by stored name or version
    for tag in ["v1.0.0", "1.0.0"] {
        for path in ["index.html", "intro.html"] {
            let want = src.page(&src.profile, &inputs, path);
            let got = snap.render_page(tag, &src.profile, &inputs, path).unwrap();
            assert_eq!(got, want, "{tag} {path}");
        }
    }
    let home = snap
        .render_page("v1.0.0", IMPLICIT_ORG, &inputs, "index.html")
        .unwrap();
    assert_eq!(home, src.page(IMPLICIT_ORG, &inputs, "index.html"));
    let html = text(&home.html);
    assert!(html.contains(TITLE), "the explicit source title is shown");
    assert!(
        !html.contains("Source project"),
        "the holder's own settings are not"
    );

    // the full render: the request through the process is the source project's artifact
    for profile in [src.profile.as_str(), IMPLICIT_ORG] {
        let want = src.site(profile, &inputs);
        let bytes = request(&snap, "v1.0.0", profile, &inputs, RenderFormat::Site).unwrap();
        let got = process(&bytes).unwrap();
        assert_eq!(got, want, "{profile}");
        assert_eq!(
            manifest_hash(&got.manifest, 1).unwrap(),
            want.manifest_hash,
            "{profile}"
        );
    }

    // the request is core's prelude and core's stream: nothing else is in it
    let bytes = request(&snap, "1.0.0", &src.profile, &inputs, RenderFormat::Site).unwrap();
    let mut want = Vec::new();
    write_request_prelude(&src.profile, RenderFormat::Site, &inputs, &mut want).unwrap();
    want.extend_from_slice(&src.export(src.v1));
    assert_eq!(bytes, want);
    holder.close().unwrap();
}

#[test]
fn hpb13_one_page_never_uses_the_holder_directory_name() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let inputs = src.inputs();
    let a = src.holder(&dir.path().join("first-holder-name"), dir.path());
    let b = src.holder(
        &dir.path().join("second").join("another-holder-name"),
        dir.path(),
    );
    let (sa, sb) = (a.snapshot().unwrap(), b.snapshot().unwrap());
    for profile in [src.profile.as_str(), IMPLICIT_ORG] {
        for path in ["index.html", "intro.html"] {
            let pa = sa.render_page("v1.0.0", profile, &inputs, path).unwrap();
            let pb = sb.render_page("v1.0.0", profile, &inputs, path).unwrap();
            assert_eq!(pa, pb, "{profile} {path}");
            for name in ["first-holder-name", "another-holder-name"] {
                assert!(!text(&pa.html).contains(name));
            }
        }
    }
    // a different explicit title changes the page; the path does not
    let mut other = inputs.clone();
    other.title = "Another explicit title".into();
    let p = sa
        .render_page("v1.0.0", IMPLICIT_ORG, &other, "index.html")
        .unwrap();
    assert!(text(&p.html).contains("Another explicit title"));
    assert!(!text(&p.html).contains(TITLE));
    a.close().unwrap();
    b.close().unwrap();
}

fn entry(path: &str, hash: [u8; 32], media: &str, size: u64, offset: Option<u64>) -> ManifestEntry {
    ManifestEntry {
        path: path.into(),
        hash: Hash(hash),
        media_type: media.into(),
        size,
        notice_offset: offset,
    }
}

#[test]
fn hpb13_adapter_recipe_equals_engine_recipe() {
    let counting: [u8; 32] = std::array::from_fn(|i| i as u8);
    let a = Manifest {
        files: vec![
            entry("index.html", [0x11; 32], "text/html", 1234, Some(100)),
            entry("assets/site.css", counting, "text/css", 0, None),
        ],
    };
    let b = Manifest {
        files: vec![
            entry("z.html", [0xa5; 32], "text/html", 18, Some(0)),
            entry(
                "a",
                [0; 32],
                "application/octet-stream",
                2_147_483_648 - 36,
                None,
            ),
            entry("A-b/c_d.E9", [0xff; 32], "image/svg+xml", 18, None),
        ],
    };
    let hex = |s: &str| -> Hash { s.parse().unwrap() };
    let want_a = hex("fec7a84cd0b0a6e97d151a2e8ad53c8adac9a0ee27144fb855437034f2a98971");
    let want_b = hex("90bfec9079e2d2b96c3dd68402590b774c35c3321892af901d03716c39433ce4");
    assert_eq!(manifest_hash(&a, 1).unwrap(), want_a);
    assert_eq!(manifest_hash(&b, 1).unwrap(), want_b);
    // the adapter's function is the engine's function, errors included
    for m in [&a, &b] {
        assert_eq!(
            manifest_hash(m, 1).unwrap(),
            rdy_engine::publication_manifest::manifest_hash(m, 1).unwrap()
        );
        for recipe in [0, 2, u32::MAX] {
            same_error(
                &manifest_hash(m, recipe).unwrap_err(),
                &rdy_engine::publication_manifest::manifest_hash(m, recipe).unwrap_err(),
            );
        }
    }
}

#[test]
fn hpb13_full_render_request_calls_no_renderer_in_holder() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let inputs = src.inputs();
    let holder = src.holder(&dir.path().join("holder"), dir.path());
    let snap = holder.snapshot().unwrap();

    // the holder resolves no profile and builds no page: a well-formed profile id that this tag
    // lacks is written like any other, and only the process that reads the request refuses it
    let unknown = format!("{}-ZZZZZ", src.profile.split_once('-').unwrap().0);
    assert_ne!(unknown, src.profile);
    let bytes = request(&snap, "v1.0.0", &unknown, &inputs, RenderFormat::Site).unwrap();
    let e = process(&bytes).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    // disagreements that name no block of the tag are the process's to refuse, not the holder's
    let mut odd = inputs.clone();
    odd.disagreements = vec![];
    request(&snap, "v1.0.0", &src.profile, &odd, RenderFormat::Markdown).unwrap();

    // the renderer's entry points count themselves (rdy-render's `probe`, on for this crate's
    // tests): the request writer, whatever the profile and format, runs none of them
    let before = probe::renders();
    for format in [
        RenderFormat::Site,
        RenderFormat::Markdown,
        RenderFormat::Techdocs,
    ] {
        for profile in [src.profile.as_str(), IMPLICIT_ORG] {
            snap.write_render_request("v1.0.0", profile, &inputs, format, Vec::new())
                .unwrap();
        }
    }
    assert_eq!(
        probe::renders(),
        before,
        "the request writer ran the renderer"
    );

    // positive control: the probe sees every real render, one per format, and the one-page render
    let mut seen = probe::renders();
    for format in [
        RenderFormat::Site,
        RenderFormat::Markdown,
        RenderFormat::Techdocs,
    ] {
        let bytes = request(&snap, "v1.0.0", &src.profile, &inputs, format).unwrap();
        assert_eq!(
            probe::renders(),
            seen,
            "{format:?}: writing is not rendering"
        );
        process(&bytes).unwrap();
        assert!(
            probe::renders() > seen,
            "{format:?}: the probe missed a render"
        );
        seen = probe::renders();
    }
    snap.render_page("v1.0.0", &src.profile, &inputs, "index.html")
        .unwrap();
    assert!(probe::renders() > seen, "the probe missed the page render");

    // the bytes are the prelude and the stream, written to a sink that records every call
    struct Sink {
        writes: Vec<usize>,
        bytes: Vec<u8>,
    }
    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.writes.push(b.len());
            self.bytes.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        writes: vec![],
        bytes: vec![],
    };
    let before = probe::renders();
    let report = snap
        .write_render_request(
            "v1.0.0",
            &src.profile,
            &inputs,
            RenderFormat::Techdocs,
            &mut sink,
        )
        .unwrap();
    assert_eq!(probe::renders(), before);
    assert!(!sink.writes.is_empty());
    let mut stream = Vec::new();
    let want = snap
        .write_stream("v1.0.0", &BTreeSet::new(), &mut stream)
        .unwrap();
    assert_eq!(report, want);
    assert!(sink.bytes.ends_with(&stream));
    let prelude = &sink.bytes[..sink.bytes.len() - stream.len()];
    let len = u32::from_le_bytes(prelude[..4].try_into().unwrap()) as usize;
    assert_eq!(len + 4, prelude.len());
    let value: Value = serde_json::from_slice(&prelude[4..]).unwrap();
    assert_eq!(
        value.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["format", "profile", "render_inputs"]
    );
    assert_eq!(value["format"], "techdocs");
    assert_eq!(value["profile"], src.profile.as_str());
    holder.close().unwrap();
}

#[test]
fn hpb13_stream_markdown_and_techdocs_share_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let inputs = src.inputs();
    let holder = src.holder(&dir.path().join("holder"), dir.path());
    let snap = holder.snapshot().unwrap();

    let mut hashes = BTreeSet::new();
    for format in [
        RenderFormat::Site,
        RenderFormat::Markdown,
        RenderFormat::Techdocs,
    ] {
        let bytes = request(&snap, "v1.0.0", &src.profile, &inputs, format).unwrap();
        let artifact = process(&bytes).unwrap();
        // the same selection in each: intro only, whatever the layout
        let all: String = artifact.files.values().map(|f| text(f)).collect();
        assert!(all.contains(INTRO_WORDS), "{format:?}");
        assert!(!all.contains(OTHER_WORDS), "{format:?}");
        assert!(
            all.contains(CANARY),
            "{format:?}: the canary is not removed yet"
        );
        // an explicit profile shows its own title, not the source title
        assert!(all.contains("Org site"), "{format:?}");
        // and the request made by core's export gives the same artifact
        let mut core = Vec::new();
        write_request_prelude(&src.profile, format, &inputs, &mut core).unwrap();
        core.extend_from_slice(&src.export(src.v1));
        assert_eq!(artifact, process(&core).unwrap(), "{format:?}");
        hashes.insert(artifact.manifest_hash);
    }
    assert_eq!(hashes.len(), 3, "each layout is its own artifact");
    // the techdocs tree has its configuration and docs; the plain tree has neither
    let tech = process(
        &request(
            &snap,
            "v1.0.0",
            &src.profile,
            &inputs,
            RenderFormat::Techdocs,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(tech.files.contains_key("mkdocs.yml"));
    assert!(tech.files.keys().any(|p| p.starts_with("docs/")));
    let plain = process(
        &request(
            &snap,
            "v1.0.0",
            &src.profile,
            &inputs,
            RenderFormat::Markdown,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!plain.files.contains_key("mkdocs.yml"));
    holder.close().unwrap();
}

#[test]
fn hpb13_bad_inputs_and_stale_snapshots_return_no_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = source(&dir.path().join("src"));
    let inputs = src.inputs();
    let repo = src.r.store().repo_id().unwrap().unwrap();
    let mut holder = StoreHandle::create_holder(&dir.path().join("holder"), repo).unwrap();
    take(
        &mut holder,
        dir.path(),
        "v1",
        &src.export(src.v1),
        "v1.0.0",
        src.l1,
    );
    let snap = holder.snapshot().unwrap();

    let fail = |tag: &str, profile: &str, inputs: &RenderInputs| -> RdyError {
        let mut out = Vec::new();
        snap.write_render_request(tag, profile, inputs, RenderFormat::Site, &mut out)
            .unwrap_err()
    };

    // a profile key is no stable selector: the take's own text, and no byte written
    let mut out = Vec::new();
    let e = snap
        .write_render_request("v1.0.0", "orgp", &inputs, RenderFormat::Site, &mut out)
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        ("E_BAD_INPUT", UNSTABLE.0, UNSTABLE.1)
    );
    assert!(out.is_empty());
    let too_long = format!("p{}", "0".repeat(128));
    assert_eq!(fail("v1.0.0", &too_long, &inputs).code, "E_BAD_INPUT");

    // inputs for another renderer, or with no title, are refused
    let mut wrong = inputs.clone();
    wrong.rdy = "0.0.1".into();
    assert_eq!(fail("v1.0.0", &src.profile, &wrong).code, "E_BAD_INPUT");
    let mut untitled = inputs.clone();
    untitled.title = String::new();
    assert_eq!(fail("v1.0.0", &src.profile, &untitled).code, "E_BAD_INPUT");
    assert_eq!(
        snap.render_page("v1.0.0", &src.profile, &untitled, "index.html")
            .unwrap_err()
            .code,
        "E_BAD_INPUT"
    );

    // unknown fields in the inputs are refused where they are read
    let mut value = serde_json::to_value(&inputs).unwrap();
    value["colour"] = json!("blue");
    assert!(serde_json::from_value::<RenderInputs>(value).is_err());

    // a tag the snapshot lacks
    assert_eq!(fail("2.0.0", &src.profile, &inputs).code, "E_NOT_FOUND");
    assert_eq!(
        snap.render_page("2.0.0", &src.profile, &inputs, "index.html")
            .unwrap_err()
            .code,
        "E_NOT_FOUND"
    );

    // a page the publication lacks, from the renderer
    assert_eq!(
        snap.render_page("v1.0.0", &src.profile, &inputs, "other.html")
            .unwrap_err()
            .code,
        "E_NOT_FOUND"
    );

    // the holder takes a removal: the older snapshot is stale and returns no success
    let secret = src.secret.clone();
    remove(&mut src.r, &secret);
    let v2 = tag(&mut src.r, "v2.0.0");
    let l2 = src.r.store().read_tag(&v2).unwrap().landing;
    take(&mut holder, dir.path(), "v2", &src.export(v2), "v2.0.0", l2);
    let mut out = Vec::new();
    let e = snap
        .write_render_request(
            "v1.0.0",
            &src.profile,
            &inputs,
            RenderFormat::Site,
            &mut out,
        )
        .unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    let e = snap
        .render_page("v1.0.0", &src.profile, &inputs, "index.html")
        .unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");

    // a fresh snapshot renders, and the removed words are gone from every file of v1 too
    let fresh = holder.snapshot().unwrap();
    for tag in ["v1.0.0", "v2.0.0"] {
        let page = fresh
            .render_page(tag, &src.profile, &inputs, "intro.html")
            .unwrap();
        assert!(!text(&page.html).contains(CANARY), "{tag}");
        let bytes = request(&fresh, tag, &src.profile, &inputs, RenderFormat::Site).unwrap();
        let artifact = process(&bytes).unwrap();
        assert!(
            artifact.files.values().all(|f| !text(f).contains(CANARY)),
            "{tag}"
        );
        assert!(
            !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
            "{tag}"
        );
    }
    holder.close().unwrap();
}

/// spv4 (D-CWC91, D-0VCQ5): the live site warns of a blocked public profile, and every durable
/// output still refuses it. Through the adapter, a holder's one-page render and its full render
/// request refuse a blocked public profile with `E_PUBLICATION_REFERENCE`, with and without a
/// selected figure, and so does the core build each stands for: no artifact is returned and no
/// figure byte is read.
#[test]
fn spv4_adapter_output_keeps_the_public_reference_gate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let png = b"\x89PNG\r\n\x1a\nspv4 gate figure".to_vec();
    let blob = r.store_mut().put_blob(&png).unwrap();
    let ids = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$plain", "fields": {"name": "plain", "title": "Plain page", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$fig", "fields": {"name": "fig", "title": "Figure page", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$fig", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "Gate figure"}},
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$op", "parent": "$other", "body": format!("{OTHER_WORDS} there.")},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    let op = ids["$op"].clone();
    write(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "parent": ids["$plain"], "body": format!("Plain sees {{{{{op}}}}}.")},
            {"op": "create", "type": "prose", "parent": ids["$fig"], "body": format!("Figure sees {{{{{op}}}}}.")}
        ]),
    );
    land(&mut r, "first");
    let p = write(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$pubzero", "parent": ids["$admin"],
             "fields": {"key": "pubzero", "title": "Public zero", "audience": "public", "select": [ids["$plain"]]}},
            {"op": "create", "type": "publication_profile", "id": "$pubfig", "parent": ids["$admin"],
             "fields": {"key": "pubfig", "title": "Public figure", "audience": "public", "select": [ids["$fig"]]}}
        ]),
    );
    land(&mut r, "profiles");
    let v1 = tag(&mut r, "v1.0.0");
    let l1 = r.store().read_tag(&v1).unwrap().landing;
    let read = r.read();
    let inputs = capture_inputs(&read, &Scope::At(l1)).unwrap();
    let mut export = Vec::new();
    stream_export::write(&read, v1, &BTreeSet::new(), &mut export).unwrap();
    let repo = r.store().repo_id().unwrap().unwrap();
    let mut holder = StoreHandle::create_holder(&dir.path().join("holder"), repo).unwrap();
    take(&mut holder, dir.path(), "v1", &export, "v1.0.0", l1);
    let snap = holder.snapshot().unwrap();
    let blocked = (
        "Public profile has 1 references outside its selection.",
        "Include the target docs or remove these references. Run the publication check again.",
    );
    for key in ["$pubzero", "$pubfig"] {
        let profile = p[key].as_str();
        // the live inspection of the same profile builds: only output refuses
        let resolved =
            rdy_engine::publication::resolve_inspection(&read, &Scope::At(l1), profile).unwrap();
        assert_eq!(
            resolved.references.status,
            rdy_engine::publication::CheckStatus::Blocked
        );
        let reads = rdy_engine::publication::blob_reads_for_tests();
        // the core build each adapter call stands for
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(&Scope::At(l1)).unwrap();
        let core = build_publication(&source, &m, "v1.0.0", false, profile, &inputs).unwrap_err();
        let core_page =
            render_publication_page(&source, &m, "v1.0.0", false, profile, &inputs, "index.html")
                .unwrap_err();
        assert_eq!(
            rdy_engine::publication::blob_reads_for_tests(),
            reads,
            "{key}: no figure byte is read"
        );
        // through the adapter: the one-page render and the full render request
        let page = snap
            .render_page("v1.0.0", profile, &inputs, "index.html")
            .unwrap_err();
        let full = request(&snap, "v1.0.0", profile, &inputs, RenderFormat::Site)
            .and_then(|bytes| process(&bytes))
            .unwrap_err();
        for e in [&core, &core_page, &page, &full] {
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                (rdy_engine::E_PUBLICATION_REFERENCE, blocked.0, blocked.1),
                "{key}: {e}"
            );
        }
        same_error(&page, &core_page);
        same_error(&full, &core);
    }
    holder.close().unwrap();
}

/// dgm6 (D-FY4VF, D-KAGB2): a holder's one-page render carries the generated diagram file the
/// page links, with the bytes and manifest entry of the source project's own render.
#[test]
fn dgm6_adapter_page_carries_generated_svg() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    drop(Rdy::init(&root, "rdy-standard").unwrap());
    let mut r = Rdy::open(&root).unwrap();
    write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$flow", "fields": {"name": "flow", "title": "Flow page", "doc_type": "page"}},
            {"op": "create", "type": "diagram", "parent": "$flow", "fields": {"lang": "mermaid", "alt": "The flow"},
             "body": "flowchart LR\n  subgraph S[first]\n    A[start] --> B[end]\n  end"}
        ]),
    );
    land(&mut r, "first");
    let v1 = tag(&mut r, "v1.0.0");
    let l1 = r.store().read_tag(&v1).unwrap().landing;
    let src = Source {
        r,
        profile: String::new(),
        secret: String::new(),
        v1,
        l1,
    };
    let inputs = capture_inputs(&src.r.read(), &Scope::At(l1)).unwrap();
    let holder = src.holder(&dir.path().join("holder"), dir.path());
    let snap = holder.snapshot().unwrap();
    let got = snap
        .render_page("v1.0.0", IMPLICIT_ORG, &inputs, "flow.html")
        .unwrap();
    assert_eq!(got, src.page(IMPLICIT_ORG, &inputs, "flow.html"));
    let drawn: Vec<&String> = got
        .assets
        .keys()
        .filter(|k| k.starts_with("assets/diagrams/"))
        .collect();
    assert_eq!(drawn.len(), 1, "{:?}", got.assets.keys());
    let path = drawn[0];
    let bytes = &got.assets[path];
    assert_eq!(
        path,
        &format!("assets/diagrams/{}.svg", Hash::of(bytes)),
        "named by the hash of its own bytes"
    );
    let svg = text(bytes);
    assert!(svg.contains("<svg") && svg.contains(">start<") && svg.contains(">first<"));
    assert!(!svg.contains("<script"));
    let entry = got.manifest.files.iter().find(|e| &e.path == path).unwrap();
    assert_eq!(
        (
            entry.media_type.as_str(),
            entry.hash,
            entry.size,
            entry.notice_offset
        ),
        ("image/svg+xml", Hash::of(bytes), bytes.len() as u64, None)
    );
    let html = text(&got.html);
    assert!(
        html.contains(&format!("href=\"{path}\">open full size</a>")),
        "{html}"
    );
    // the full render through the process ships the same file
    let full = request(&snap, "v1.0.0", IMPLICIT_ORG, &inputs, RenderFormat::Site)
        .and_then(|b| process(&b))
        .unwrap();
    assert_eq!(full.files.get(path), Some(bytes));
    holder.close().unwrap();
}

/// rvm3 (TASK-823; hub D-X0F6P; D-WDKWQ, D-KDFZ3): a core publication of a page with figures of
/// several media types and a drawn diagram, built as `dgm6_` builds its fixture.
fn rvm3_artifact() -> (PublicationArtifact, BTreeMap<&'static str, Hash>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    drop(Rdy::init(&root, "rdy-standard").unwrap());
    let mut r = Rdy::open(&root).unwrap();
    let mut blobs = BTreeMap::new();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$pics", "fields": {"name": "pics", "title": "Pictures", "doc_type": "page"}}),
    ];
    let svg: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
    for (key, bytes, media, alt) in [
        (
            "png",
            b"\x89PNG\r\n\x1a\nrvm3 png".as_slice(),
            "image/png",
            "A png",
        ),
        ("svg", svg, "image/svg+xml", "An svg"),
        (
            "params",
            b"rvm3 params".as_slice(),
            "image/svg+xml; charset=utf-8",
            "With parameters",
        ),
        (
            "other",
            b"rvm3 other".as_slice(),
            "application/x-rvm3",
            "Other",
        ),
    ] {
        let blob = r.store_mut().put_blob(bytes).unwrap();
        blobs.insert(key, blob);
        ops.push(json!({"op": "create", "type": "figure", "parent": "$pics",
            "fields": {"blob": blob.to_string(), "media_type": media, "alt": alt}}));
    }
    ops.push(json!({"op": "create", "type": "diagram", "parent": "$pics",
        "fields": {"lang": "mermaid", "alt": "The flow"},
        "body": "flowchart LR\n  subgraph S[first]\n    A[start] --> B[end]\n  end"}));
    write(&mut r, Value::Array(ops));
    land(&mut r, "first");
    let v1 = tag(&mut r, "v1.0.0");
    let l1 = r.store().read_tag(&v1).unwrap().landing;
    let src = Source {
        r,
        profile: String::new(),
        secret: String::new(),
        v1,
        l1,
    };
    let inputs = capture_inputs(&src.r.read(), &Scope::At(l1)).unwrap();
    let art = src.site(IMPLICIT_ORG, &inputs);
    (art, blobs)
}

fn rvm3_entry_at<'a>(art: &'a PublicationArtifact, path: &str) -> &'a ManifestEntry {
    art.manifest
        .files
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("{path} in {:?}", art.files.keys()))
}

/// Every entry of a core publication passes the rule the adapter re-exports.
#[test]
fn rvm3_every_entry_of_a_core_publication_passes_core_rule() {
    let (art, blobs) = rvm3_artifact();
    for e in &art.manifest.files {
        rdy_adapter::rendering::check_publication_media_type(e)
            .unwrap_or_else(|err| panic!("{} {}: {}", e.path, e.media_type, err.message));
    }
    for (key, ext, media) in [
        ("png", "png", "image/png"),
        ("svg", "svg", "image/svg+xml"),
        ("params", "bin", "image/svg+xml"),
        ("other", "bin", "application/x-rvm3"),
    ] {
        let e = rvm3_entry_at(&art, &format!("assets/{}.{ext}", blobs[key]));
        assert_eq!(e.media_type, media, "{key}");
    }
    let drawn: Vec<&ManifestEntry> = art
        .manifest
        .files
        .iter()
        .filter(|e| e.path.starts_with("assets/diagrams/"))
        .collect();
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].media_type, "image/svg+xml");
}

/// A changed entry is refused with core's own manifest text.
#[test]
fn rvm3_a_changed_entry_is_refused_with_cores_text() {
    let (art, blobs) = rvm3_artifact();
    let png_path = format!("assets/{}.png", blobs["png"]);
    let png = rvm3_entry_at(&art, &png_path).clone();
    let index = rvm3_entry_at(&art, "index.html").clone();
    let diagram = art
        .manifest
        .files
        .iter()
        .find(|e| e.path.starts_with("assets/diagrams/"))
        .unwrap()
        .clone();
    let mut changed = Vec::new();
    changed.push(ManifestEntry {
        media_type: "text/html".into(),
        ..png.clone()
    });
    changed.push(ManifestEntry {
        media_type: "image/svg+xml".into(),
        ..index
    });
    changed.push(ManifestEntry {
        path: "pics.svg".into(),
        ..diagram
    });
    changed.push(ManifestEntry {
        path: format!("assets/{}.jpg", blobs["png"]),
        ..png
    });
    for e in &changed {
        let err = rdy_adapter::rendering::check_publication_media_type(e).unwrap_err();
        assert_eq!(err.code, "E_BAD_INPUT", "{}", e.path);
        assert_eq!(err.message, "publication manifest is invalid");
        assert_eq!(err.fix, "use unique relative paths and valid file metadata");
        assert!(err.issues.is_empty());
    }
}
