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
        .propose(&session(Role::Writer), "work", title, None)
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
