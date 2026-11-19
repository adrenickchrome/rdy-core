//! ab5 (D-WJ0SJ, D-69YCN, D-KDFZ3): a holder's profile list, selection identity, disclosure check
//! and renderings read the as-built status of the tag they name. Profile settings travel in the
//! tag's own block fields and the caller's captured pack, so neither the source's current canon
//! nor any host configuration changes what an old or a new tag shows.

use rdy_adapter::rendering::{
    PublicationArtifact, RenderFormat, RenderInputs, read_render_artifact, run_render_process,
};
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource, profile_facts, resolve};
use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::Role;
use rdy_render::build_publication;
use rdy_render::render_input::capture_inputs;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

const PLAN: &str = "PLANNEDSENTINEL";
const SHIP: &str = "SHIPPEDSENTINEL";

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
                message: "ab5".into(),
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

fn tag(r: &mut Rdy, name: &str) -> (Hash, Hash) {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    let t = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1;
    (t, r.store().read_tag(&t).unwrap().landing)
}

fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

fn take(h: &StoreHandle, scratch: &Path, name: &str, bytes: &[u8], tag: &str, landing: Hash) {
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn process(request: &[u8]) -> PublicationArtifact {
    let mut packet = Vec::new();
    run_render_process(request, &mut packet).unwrap();
    read_render_artifact(packet.as_slice()).unwrap()
}

#[test]
fn ab5_held_tag_profiles_and_artifacts_keep_status() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    drop(Rdy::init(&src_dir, "rdy-standard").unwrap());
    std::fs::write(
        src_dir.join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"Source project\"\n",
    )
    .unwrap();
    let mut r = Rdy::open(&src_dir).unwrap();
    let ids = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ship", "parent": "$intro", "body": format!("{SHIP} words.")},
            {"op": "create", "type": "prose", "id": "$plan", "parent": "$intro", "fields": {"planned": true}, "body": format!("{PLAN} words.")},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "first");
    let p = write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$hide", "parent": ids["$admin"],
            "fields": {"key": "hide", "title": "Hide site", "audience": "org", "planned": "hide", "select": [ids["$intro"]]}}]),
    );
    land(&mut r, "profile");
    let hide = p["$hide"].clone();
    let (v1, l1) = tag(&mut r, "v1.0.0");

    // the planned block ships: the tag v2.0.0 shows it
    write(
        &mut r,
        json!([{"op": "update", "id": ids["$plan"], "fields": {"planned": false}}]),
    );
    land(&mut r, "ship it");
    let (v2, l2) = tag(&mut r, "v2.0.0");

    // canon moves on after both tags: the profile now marks, and the block is planned again.
    // Neither change may reach a rendering of a tag.
    write(
        &mut r,
        json!([
            {"op": "update", "id": ids["$plan"], "fields": {"planned": true}},
            {"op": "update", "id": hide, "fields": {"planned": "mark"}}
        ]),
    );
    land(&mut r, "later");

    let repo = r.store().repo_id().unwrap().unwrap();
    let holder = StoreHandle::create_holder(&dir.path().join("holder"), repo).unwrap();
    take(&holder, dir.path(), "v1", &export(&r, v1), "v1.0.0", l1);
    take(&holder, dir.path(), "v2", &export(&r, v2), "v2.0.0", l2);
    let snap = holder.snapshot().unwrap();

    // the profile list: the same facts the source project resolves at each tag
    let mut hashes = BTreeMap::new();
    for (name, landing) in [("v1.0.0", l1), ("v2.0.0", l2)] {
        let listed = snap.profiles(name).unwrap();
        let scope = Scope::At(landing);
        for summary in &listed {
            let want = profile_facts(&resolve(&r.read(), &scope, &summary.selector).unwrap());
            assert_eq!(summary.selection_hash, want.selection_hash, "{name}");
            assert_eq!(
                summary.selection_identity, want.selection_identity,
                "{name}"
            );
            assert_eq!(summary.selection, want.selection, "{name}");
        }
        let org = listed.iter().find(|s| s.selector == hide).unwrap();
        hashes.insert(name, (org.selection_hash, org.selection_identity));
        // the implicit profile marks planned content, so it keeps the block in both tags
        let implicit = listed.iter().find(|s| s.selector == IMPLICIT_ORG).unwrap();
        assert!(
            implicit
                .selection
                .iter()
                .any(|e| e.id.as_str() == ids["$plan"]),
            "{name}"
        );
        assert_eq!(
            org.selection.iter().any(|e| e.id.as_str() == ids["$plan"]),
            name == "v2.0.0",
            "{name}: the hiding profile selects the block only once it ships"
        );
    }
    assert_ne!(hashes["v1.0.0"].0, hashes["v2.0.0"].0);
    assert_ne!(hashes["v1.0.0"].1, hashes["v2.0.0"].1);

    // the disclosure check compares the two tags: the shipped block is the difference
    let prev = {
        let org = snap
            .profiles("v1.0.0")
            .unwrap()
            .into_iter()
            .find(|s| s.selector == hide)
            .unwrap();
        rdy_adapter::publication::PreviousPublication {
            tag: "v1.0.0".into(),
            profile_version: org.version,
            selection_identity: org.selection_identity,
        }
    };
    let check = snap.disclosure("v2.0.0", &hide, Some(&prev)).unwrap();
    let diff = check.comparison.expect("a comparison");
    assert!(
        diff.changed_docs
            .iter()
            .any(|d| d.id.as_str() == ids["$intro"]),
        "the doc that gained the shipped block is disclosed: {diff:?}"
    );
    assert_ne!(
        diff.before_selection_identity,
        diff.after_selection_identity
    );

    // renderings: the holder's pages and artifacts are the source's own for each tag, with the
    // caller's explicit inputs, whatever canon holds now
    let inputs: RenderInputs = capture_inputs(&r.read(), &Scope::At(l2)).unwrap();
    for (name, landing, shows) in [("v1.0.0", l1, false), ("v2.0.0", l2, true)] {
        for profile in [hide.as_str(), IMPLICIT_ORG] {
            let read = r.read();
            let source = ReadPublicationSource::with_settings(
                &read,
                &inputs.pack,
                &inputs.title,
                inputs.description.as_deref(),
            );
            let m = read.materialized(&Scope::At(landing)).unwrap();
            let want = build_publication(&source, &m, name, false, profile, &inputs).unwrap();
            let mut req = Vec::new();
            snap.write_render_request(name, profile, &inputs, RenderFormat::Site, &mut req)
                .unwrap();
            let got = process(&req);
            assert_eq!(got, want, "{name} {profile}");
            let page = snap
                .render_page(name, profile, &inputs, "intro.html")
                .unwrap();
            assert!(page.html == want.files["intro.html"], "{name} {profile}");
            let html = text(&page.html);
            assert!(html.contains(SHIP), "{name} {profile}");
            let plan_shown = html.contains(PLAN);
            let marked = html.contains(">Planned</span>");
            if profile == IMPLICIT_ORG {
                // the implicit profile marks: planned in v1, shipped plain in v2
                assert!(plan_shown, "{name}");
                assert_eq!(marked, !shows, "{name}");
            } else {
                assert_eq!(plan_shown, shows, "{name}");
                assert!(!marked, "{name}");
            }
        }
    }
    holder.close().unwrap();
}
