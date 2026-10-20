//! hpb9: `rdy render-stream`, the internal render process (hub i-N0G2N, r-B0ZYE, r-7GS51;
//! D-KDFZ3, D-48NYM, D-0VCQ5). It reads one render request on standard input (a bounded JSON
//! prelude, then one complete tag history stream) and writes the artifact packet on standard
//! output. It runs before project discovery, config reads and any daemon connection, so the
//! directory it runs in changes no byte. A refusal leaves standard output empty, exits 1 and
//! writes one compact JSON line on standard error; a success writes nothing there.
use rdy_engine::publication::IMPLICIT_ORG;
use rdy_engine::{E_BAD_INPUT, Rdy, Scope};
use rdy_model::Hash;
use rdy_qa::fixtures::{approver, land, output_within_stdin, rdy_bin, write};
use rdy_render::PublicationArtifact;
use rdy_render::process::{RenderFormat, read_artifact, write_request_prelude};
use rdy_render::render_input::capture_inputs;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

/// A hang guard, not a speed check: a child still running at this budget is killed and reaped,
/// and the test fails.
const HANG_BUDGET: Duration = Duration::from_secs(120);
const TAG: &str = "v1.0.0";
const SOURCE_TITLE: &str = "Stream source title";

/// A tagged source project and its render requests.
pub(crate) struct Source {
    _dir: tempfile::TempDir,
    r: Rdy,
    pub(crate) profile: String,
    tag: Hash,
    landing: Hash,
}

pub(crate) fn source() -> Source {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source-project-dir");
    std::fs::create_dir_all(&root).unwrap();
    drop(Rdy::init(&root, "rdy-standard").unwrap());
    std::fs::write(
        root.join(".rdy.yaml"),
        format!("pack: rdy-standard\nname: {SOURCE_TITLE:?}\n"),
    )
    .unwrap();
    let mut r = Rdy::open(&root).unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "STREAMCANARY guide words."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    land(&mut r, "w1").unwrap();
    let p = write(
        &mut r,
        "w2",
        json!([{"op": "create", "type": "publication_profile", "id": "$p",
            "parent": w.created["$admin"],
            "fields": {"key": "guidep", "title": "Guide site", "audience": "org",
                "select": [w.created["$d"]]}}]),
    )
    .unwrap();
    land(&mut r, "w2").unwrap();
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
        profile: p.created["$p"].clone(),
        tag,
        landing,
    }
}

impl Source {
    pub(crate) fn stream(&self) -> Vec<u8> {
        let mut out = Vec::new();
        rdy_engine::stream_export::write(&self.r.read(), self.tag, &BTreeSet::new(), &mut out)
            .unwrap();
        out
    }

    pub(crate) fn request(&self, profile: &str, format: RenderFormat) -> Vec<u8> {
        let inputs = capture_inputs(&self.r.read(), &Scope::At(self.landing)).unwrap();
        let mut out = Vec::new();
        write_request_prelude(profile, format, &inputs, &mut out).unwrap();
        out.extend_from_slice(&self.stream());
        out
    }
}

/// `rdy <args>` in `dir`, with `stdin` fed to it and a home of its own, so no user config of
/// the person running the tests applies. The child is supervised: a child still running at the
/// budget is killed and reaped.
pub(crate) fn rdy_in(dir: &Path, home: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut cmd = Command::new(rdy_bin());
    cmd.args(args)
        .current_dir(dir)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home);
    output_within_stdin(&mut cmd, Some(stdin), HANG_BUDGET).unwrap_or_else(|e| panic!("{e}"))
}

fn err_text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A successful run's artifact: exit 0, nothing on stderr, a valid packet on stdout.
#[track_caller]
pub(crate) fn artifact_of(o: &Output) -> PublicationArtifact {
    assert!(o.status.success(), "{}", err_text(o));
    assert!(o.stderr.is_empty(), "nothing on stderr: {}", err_text(o));
    read_artifact(Cursor::new(o.stdout.clone())).unwrap_or_else(|e| panic!("{e}"))
}

/// A refusal: exit 1, no stdout byte, and one compact JSON line `{code,message,fix,issues}` on
/// stderr.
#[track_caller]
fn refusal(o: &Output) -> Value {
    assert_eq!(o.status.code(), Some(1), "{}", err_text(o));
    assert!(o.stdout.is_empty(), "{} stdout bytes", o.stdout.len());
    let text = err_text(o);
    assert!(
        text.ends_with('\n') && text.matches('\n').count() == 1,
        "{text:?}"
    );
    let v: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["code", "fix", "issues", "message"]
    );
    // the CLI's error fields in their order, written compactly (the same value written compactly
    // with its keys sorted takes the same bytes)
    let line = text.trim_end();
    let head = format!(
        "{{\"code\":{},\"message\":{},\"fix\":{},\"issues\":[",
        v["code"], v["message"], v["fix"]
    );
    assert!(line.starts_with(&head) && line.ends_with("]}"), "{line}");
    assert_eq!(
        serde_json::to_string(&v).unwrap().len(),
        line.len(),
        "compact"
    );
    v
}

#[test]
fn hpb9_process_has_no_project_or_cwd_dependency() {
    let s = source();
    let home = tempfile::tempdir().unwrap();
    // a config that does not parse in the user config: a command that reads config would say so
    std::fs::create_dir_all(home.path().join("rdy")).unwrap();
    std::fs::write(home.path().join("rdy/config.yaml"), b"human: [unclosed\n").unwrap();
    // one directory that looks like a project with a malformed config, one named otherwise
    let base = tempfile::tempdir().unwrap();
    let broken = base.path().join("Broken project dir");
    std::fs::create_dir_all(broken.join(".rdy")).unwrap();
    std::fs::write(broken.join(".rdy.yaml"), b"name: [unclosed\nserve: {\n").unwrap();
    std::fs::write(broken.join(".rdy/FORMAT"), b"not a store").unwrap();
    let plain = base.path().join("zz other place");
    std::fs::create_dir_all(&plain).unwrap();
    let probe = rdy_in(&broken, home.path(), &["config"], b"");
    assert!(
        err_text(&probe).contains("E_CONFIG"),
        "the config is invalid, as the test needs: {}",
        err_text(&probe)
    );
    let listing = |d: &Path| {
        let mut v: Vec<String> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let before = (listing(&broken), listing(&plain));
    for format in [
        RenderFormat::Site,
        RenderFormat::Markdown,
        RenderFormat::Techdocs,
    ] {
        for profile in [IMPLICIT_ORG.to_string(), s.profile.clone()] {
            let input = s.request(&profile, format);
            let a = rdy_in(&broken, home.path(), &["render-stream"], &input);
            let b = rdy_in(&plain, home.path(), &["render-stream"], &input);
            assert!(
                a.stdout == b.stdout,
                "{format:?} {profile}: the directory changed bytes"
            );
            let a = artifact_of(&a);
            let all: String = a
                .files
                .values()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            assert!(all.contains("STREAMCANARY"), "{format:?} {profile}");
            // the captured source title, never a directory's name
            let title = if profile == IMPLICIT_ORG {
                SOURCE_TITLE
            } else {
                "Guide site"
            };
            assert!(all.contains(title), "{format:?} {profile}");
            for dir in ["Broken project dir", "zz other place", "source-project-dir"] {
                assert!(!all.contains(dir), "{format:?} {profile}: {dir}");
            }
        }
    }
    assert_eq!(
        (listing(&broken), listing(&plain)),
        before,
        "no file written"
    );
}

#[test]
fn hpb9_bad_prelude_stream_or_trailer_emits_no_stdout() {
    let s = source();
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let good = s.request(IMPLICIT_ORG, RenderFormat::Site);
    artifact_of(&rdy_in(dir.path(), home.path(), &["render-stream"], &good));
    let len = u32::from_le_bytes(good[..4].try_into().unwrap()) as usize;
    let mut extra = good.clone();
    extra.push(0);
    let mut bad_json = 4u32.to_le_bytes().to_vec();
    bad_json.extend_from_slice(b"nope");
    bad_json.extend_from_slice(&s.stream());
    let mut key = Vec::new();
    {
        let v: Value = serde_json::from_slice(&good[4..4 + len]).unwrap();
        let mut v = v;
        v["profile"] = json!("guidep");
        let j = serde_json::to_vec(&v).unwrap();
        key.extend_from_slice(&(j.len() as u32).to_le_bytes());
        key.extend_from_slice(&j);
        key.extend_from_slice(&s.stream());
    }
    for (what, input, code, message) in [
        (
            "empty",
            vec![],
            E_BAD_INPUT,
            Some("render process input is invalid"),
        ),
        (
            "short",
            good[..4 + len - 1].to_vec(),
            E_BAD_INPUT,
            Some("render process input is invalid"),
        ),
        (
            "no JSON",
            bad_json,
            E_BAD_INPUT,
            Some("render process input is invalid"),
        ),
        (
            "a profile key",
            key,
            E_BAD_INPUT,
            Some("A publication take names its profile by a full profile id or implicit:org."),
        ),
        (
            "prelude alone",
            good[..4 + len].to_vec(),
            "E_STREAM_TRUNCATED",
            None,
        ),
        (
            "truncated stream",
            good[..good.len() - 1].to_vec(),
            "E_STREAM_TRUNCATED",
            None,
        ),
        ("trailing byte", extra, "E_BAD_RECORD", None),
    ] {
        let o = rdy_in(dir.path(), home.path(), &["render-stream"], &input);
        let v = refusal(&o);
        assert_eq!(v["code"], json!(code), "{what}: {v}");
        if let Some(m) = message {
            assert_eq!(v["message"], json!(m), "{what}: {v}");
        }
        assert!(v["issues"].is_array(), "{what}: {v}");
        assert!(!err_text(&o).contains("nope"), "{what}: no raw payload");
    }
}

#[test]
fn hpb9_render_stream_takes_no_global_options() {
    let s = source();
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let input = s.request(IMPLICIT_ORG, RenderFormat::Site);
    let project = dir.path().display().to_string();
    for args in [
        vec!["--json", "render-stream"],
        vec!["render-stream", "--json"],
        vec!["--project", project.as_str(), "render-stream"],
        vec!["render-stream", "--project", project.as_str()],
        vec!["--no-daemon", "render-stream"],
    ] {
        let v = refusal(&rdy_in(dir.path(), home.path(), &args, &input));
        assert_eq!(
            v,
            json!({"code": E_BAD_INPUT,
                "message": "render-stream accepts only its standard input",
                "fix": "send the render request on standard input without global output or project options",
                "issues": []}),
            "{args:?}"
        );
    }
    // no flag of its own: no scope, out, audience, project or input file
    for flag in ["--out", "--scope", "--audience", "--file"] {
        let o = rdy_in(
            dir.path(),
            home.path(),
            &["render-stream", flag, "x"],
            &input,
        );
        assert!(!o.status.success(), "{flag}");
        assert!(o.stdout.is_empty(), "{flag}");
    }
    // the same input through the CLI and the library gives the same bytes
    let o = rdy_in(dir.path(), home.path(), &["render-stream"], &input);
    let mut lib = Vec::new();
    rdy_render::process::run(Cursor::new(input.clone()), &mut lib).unwrap();
    assert!(o.stdout == lib, "CLI and library differ");
    artifact_of(&o);
}
