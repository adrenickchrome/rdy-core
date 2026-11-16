//! Sync client A Task 1c (D-8RGWW, D-XG85Y): a render takes render inputs that another rdy
//! captured, when this rdy can read them whole. The `rdy` member names the capturing client; the
//! bytes follow the rdy that renders. Only the label changes here, so every artifact byte is the
//! current-labelled case's.
use rdy_engine::hub_publish::PublishRenderer;
use rdy_engine::render_inputs::{RDY_VERSION, RenderInputs};
use rdy_engine::*;
use rdy_model::*;
use rdy_render::process::{RenderFormat, read_artifact, run, write_request_prelude};
use rdy_render::publish::PublicationRenderer;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Cursor;

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

/// A tagged project: doc `a` (which references doc `b`), doc `b`, and the org profile `site`
/// that selects `a`.
struct Source {
    _dir: tempfile::TempDir,
    r: Rdy,
    site: String,
    tag: Hash,
    landing: Hash,
}

fn source() -> Source {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let ops = |ops: Value| WriteRequest {
        view: "work".into(),
        ops: serde_json::from_value(ops).unwrap(),
        justification: None,
        message: "scc1".into(),
    };
    let w = r
        .write(
            &writer(),
            ops(json!([
                {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$a", "body": "Text of a."},
                {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$b", "body": "Text of b."},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
            ])),
        )
        .unwrap();
    let (a, b, admin) = (&w.created["$a"], &w.created["$b"], &w.created["$admin"]);
    let w = r
        .write(
            &writer(),
            ops(json!([
                {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{b}}}}}.")},
                {"op": "create", "type": "publication_profile", "id": "$site", "parent": admin,
                    "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}
            ])),
        )
        .unwrap();
    let site = w.created["$site"].clone();
    let p = r
        .propose(&writer(), "work", "scc1", Some("test summary"))
        .unwrap();
    r.land(&founder(), &p.id).unwrap();
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let (_, tag, info) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap();
    Source {
        _dir: dir,
        r,
        site,
        tag,
        landing: info.landing,
    }
}

/// The labels other rdy versions captured under: older and newer than this one.
const LABELS: [&str; 3] = ["0.0.1", "0.1.0", "99.0.0"];

#[test]
fn scc1_render_takes_inputs_from_another_version() {
    let s = source();
    let read = s.r.read();
    let scope = Scope::At(s.landing);
    let renderer = PublicationRenderer;
    let current = renderer.capture(&read, &scope).unwrap();
    assert_eq!(current.rdy, RDY_VERSION);
    let render = |inputs: &RenderInputs| {
        renderer
            .render(&read, &scope, "v1.0.0", &s.site, inputs)
            .unwrap_or_else(|e| panic!("{}: {e}", inputs.rdy))
    };
    let want = render(&current);
    let want_manifest = want.artifact.manifest().clone();
    let want_paths: Vec<String> = want
        .artifact
        .paths()
        .iter()
        .map(|p| p.to_string())
        .collect();
    assert!(!want_paths.is_empty());
    let bytes = |r: &rdy_engine::hub_publish::RenderedUpload, path: &str| {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut r.artifact.open(path).unwrap(), &mut out).unwrap();
        out
    };
    for label in LABELS {
        let mut other = current.clone();
        other.rdy = label.into();
        let got = render(&other);
        assert_eq!(got.warnings, want.warnings, "{label}");
        assert_eq!(got.artifact.manifest(), &want_manifest, "{label}");
        let paths: Vec<String> = got.artifact.paths().iter().map(|p| p.to_string()).collect();
        assert_eq!(paths, want_paths, "{label}");
        for path in &paths {
            assert!(bytes(&got, path) == bytes(&want, path), "{label} {path}");
        }
    }
}

/// The process request for `inputs`, then the tag's history stream.
fn request(s: &Source, inputs: &RenderInputs) -> Vec<u8> {
    let mut out = Vec::new();
    write_request_prelude("implicit:org", RenderFormat::Site, inputs, &mut out).unwrap();
    rdy_engine::stream_export::write(&s.r.read(), s.tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

#[test]
fn scc1_process_takes_inputs_from_another_version_and_refuses_unknown_members() {
    let s = source();
    let inputs =
        rdy_render::render_input::capture_inputs(&s.r.read(), &Scope::At(s.landing)).unwrap();
    let process = |input: &[u8]| {
        let mut out = Vec::new();
        run(Cursor::new(input.to_vec()), &mut out).map(|_| out)
    };
    let want = process(&request(&s, &inputs)).unwrap();
    read_artifact(Cursor::new(want.clone())).unwrap();
    for label in LABELS {
        let mut other = inputs.clone();
        other.rdy = label.into();
        let got = process(&request(&s, &other)).unwrap();
        assert!(got == want, "{label}");
    }
    // a member the inputs carry that this rdy does not know is refused whole
    let stream = request(&s, &inputs);
    let len = u32::from_le_bytes(stream[..4].try_into().unwrap()) as usize;
    let mut prelude: Value = serde_json::from_slice(&stream[4..4 + len]).unwrap();
    prelude["render_inputs"]["rdy"] = json!("99.0.0");
    prelude["render_inputs"]["future"] = json!(1);
    let json = serde_json::to_vec(&prelude).unwrap();
    let mut framed = (json.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(&json);
    framed.extend_from_slice(&stream[4 + len..]);
    let e = process(&framed).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        format!("render inputs from rdy 99.0.0 do not fit rdy {RDY_VERSION}")
    );
}
