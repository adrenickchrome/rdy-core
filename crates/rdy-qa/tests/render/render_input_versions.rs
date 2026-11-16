//! Sync client A Task 1c (D-8RGWW, D-XG85Y): `rdy render-stream` takes render inputs that another
//! rdy captured, when this rdy can read them whole. The `rdy` member names the capturing client;
//! the bytes follow the rdy that renders. Only the label changes, so the packet (the manifest
//! and every artifact file) is the current-labelled case's. An input with a member this rdy does
//! not know is refused whole with D-8RGWW's words.
use rdy_engine::publication::IMPLICIT_ORG;
use rdy_engine::render_inputs::{RDY_VERSION, RenderInputs};
use rdy_engine::{Rdy, Scope};
use rdy_model::Hash;
use rdy_qa::fixtures::{approver, land, output_within_stdin, write};
use rdy_render::process::{RenderFormat, read_artifact, write_request_prelude};
use rdy_render::render_input::capture_inputs;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

/// A hang guard, not a speed check.
const HANG_BUDGET: Duration = Duration::from_secs(120);
const TAG: &str = "v1.0.0";

/// A tagged source project.
struct Source {
    _dir: tempfile::TempDir,
    r: Rdy,
    tag: Hash,
    landing: Hash,
}

fn source() -> Source {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source-project-dir");
    std::fs::create_dir_all(&root).unwrap();
    drop(Rdy::init(&root, "rdy-standard").unwrap());
    std::fs::write(
        root.join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"Versions source\"\n",
    )
    .unwrap();
    let mut r = Rdy::open(&root).unwrap();
    write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "VERSIONCANARY guide words."}
        ]),
    )
    .unwrap();
    land(&mut r, "w1").unwrap();
    r.tag(&approver(), TAG, None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == TAG)
        .unwrap()
        .1;
    let landing = r.store().read_tag(&tag).unwrap().landing;
    Source {
        _dir: dir,
        r,
        tag,
        landing,
    }
}

impl Source {
    fn stream(&self) -> Vec<u8> {
        let mut out = Vec::new();
        rdy_engine::stream_export::write(&self.r.read(), self.tag, &BTreeSet::new(), &mut out)
            .unwrap();
        out
    }

    fn inputs(&self) -> RenderInputs {
        capture_inputs(&self.r.read(), &Scope::At(self.landing)).unwrap()
    }

    fn request(&self, inputs: &RenderInputs) -> Vec<u8> {
        let mut out = Vec::new();
        write_request_prelude(IMPLICIT_ORG, RenderFormat::Site, inputs, &mut out).unwrap();
        out.extend_from_slice(&self.stream());
        out
    }
}

/// `rdy render-stream` in `dir`, with `stdin` and a home of its own.
fn render_stream(dir: &Path, home: &Path, stdin: &[u8]) -> Output {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.arg("render-stream")
        .current_dir(dir)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home);
    output_within_stdin(&mut cmd, Some(stdin), HANG_BUDGET).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn scc1_render_stream_takes_inputs_from_another_version() {
    let s = source();
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let inputs = s.inputs();
    assert_eq!(inputs.rdy, RDY_VERSION);
    let want = render_stream(dir.path(), home.path(), &s.request(&inputs));
    assert!(
        want.status.success(),
        "{}",
        String::from_utf8_lossy(&want.stderr)
    );
    let want_artifact = read_artifact(Cursor::new(want.stdout.clone())).unwrap();
    for label in ["0.0.1", "0.1.0", "99.0.0"] {
        let mut other = inputs.clone();
        other.rdy = label.into();
        let got = render_stream(dir.path(), home.path(), &s.request(&other));
        assert!(
            got.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&got.stderr)
        );
        assert!(got.stderr.is_empty(), "{label}");
        let artifact = read_artifact(Cursor::new(got.stdout.clone())).unwrap();
        assert_eq!(artifact.manifest, want_artifact.manifest, "{label}");
        assert_eq!(
            artifact.manifest_hash, want_artifact.manifest_hash,
            "{label}"
        );
        assert_eq!(artifact.files, want_artifact.files, "{label}");
        assert!(got.stdout == want.stdout, "{label}: the packet bytes");
    }
}

#[test]
fn scc1_render_stream_refuses_inputs_it_cannot_read_whole() {
    let s = source();
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let request = s.request(&s.inputs());
    let len = u32::from_le_bytes(request[..4].try_into().unwrap()) as usize;
    let base: Value = serde_json::from_slice(&request[4..4 + len]).unwrap();
    let reframe = |prelude: &Value| {
        let json = serde_json::to_vec(prelude).unwrap();
        let mut out = (json.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&json);
        out.extend_from_slice(&request[4 + len..]);
        out
    };
    let mut unknown = base.clone();
    unknown["render_inputs"]["rdy"] = json!("99.0.0");
    unknown["render_inputs"]["future"] = json!(1);
    let mut pack = base.clone();
    pack["render_inputs"]["rdy"] = json!("0.0.1");
    pack["render_inputs"]["pack"]["tensions_across"] = json!({"future_kind": "warn"});
    for (prelude, captured) in [(unknown, "99.0.0"), (pack, "0.0.1")] {
        let o = render_stream(dir.path(), home.path(), &reframe(&prelude));
        assert!(!o.status.success(), "{captured}");
        assert!(o.stdout.is_empty(), "{captured}: nothing on stdout");
        let line: Value = serde_json::from_slice(&o.stderr).unwrap();
        assert_eq!(line["code"], "E_BAD_INPUT");
        assert_eq!(
            line["message"],
            format!("render inputs from rdy {captured} do not fit rdy {RDY_VERSION}")
        );
        assert_eq!(
            line["fix"],
            format!(
                "capture the render inputs with rdy {RDY_VERSION}, or render with rdy {captured} or later"
            )
        );
    }
}
