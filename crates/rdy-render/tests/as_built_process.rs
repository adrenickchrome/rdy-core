//! ab5 (D-WJ0SJ, D-69YCN): the stateless render process and a direct publication give the same
//! bytes for the same captured inputs, and a profile that hides planned content leaves the hidden
//! block out of every artifact while the tag stream still holds it. A profile that marks it
//! shows `Planned`. Repeated calls share no state, and no protocol field is new.
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::*;
use rdy_model::*;
use rdy_render::process::{
    ProcessSummary, RenderFormat, read_artifact, run, write_request_prelude,
};
use rdy_render::render_input::capture_inputs;
use rdy_render::{PublicationArtifact, build_publication, render_publication_page};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

const TAG: &str = "v1.0.0";
const HIDDEN: &str = "HIDDENPLANNEDSENTINEL";
const SHOWN: &str = "SHOWNSHIPPEDSENTINEL";

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

/// F2 (D-DKJS5): the person confirms each exception beside version `p` before it lands.
fn confirm_exceptions(r: &mut Rdy, s: &Session, p: &ProposalInfo) {
    use rdy_engine::answer::{AnswerRequest, relay::RelayInput};
    use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        let req = AnswerRequest {
            r#ref: item.clone(),
            option: "confirm".into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        };
        r.answer(s, &s.actor, req)
            .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}

fn put(r: &mut Rdy, ops: Value) -> BTreeMap<String, String> {
    let w = r
        .write(
            &writer(),
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

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some("test summary"))
        .unwrap();
    confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
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

/// A tagged project: doc `intro` holds a shipped prose (decided by a decision built 1.0.0), a
/// planned note and a planned prose (decided by a planned decision), a shipped figure, and a
/// term; the admin doc holds an org profile that hides planned content, one that marks it, and
/// a reference doc profile that shows since.
/// The standard pack with a `reference` doc type and one more content type, `memo`.
const TEAM_PACK: &str = "\
name: team
version: \"1\"
extends: rdy-standard
doc_types:
  reference: { skeleton: [] }
block_types:
  memo: { prefix: mm, traits: [informative], body: required }
  section:
    prefix: s
    traits: [structural]
    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, value, entity, flow, example, note, rationale, table, diagram, figure, code, decision, question, memo]
";

struct Project {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
    tag: Hash,
    landing: Hash,
    figure: Vec<u8>,
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/team.yaml"), TEAM_PACK).unwrap();
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        "pack: team\nname: \"As built\"\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let figure = png(3, 2, [9, 99, 199]);
    let blob = r.store_mut().put_blob(&figure).unwrap();
    let mut ids = put(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$rules", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dp", "parent": "$rules", "fields": {"statement": "Ship the planned part.", "status": "proposed", "built": "planned"}},
            {"op": "create", "type": "decision", "id": "$dv", "parent": "$rules", "fields": {"statement": "Ship the built part.", "status": "proposed", "built": "1.2.0"}},
            {"op": "create", "type": "folder", "id": "$guide", "fields": {"name": "guide", "title": "Guide"}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$guide", "fields": {"name": "intro", "title": "Intro page", "doc_type": "reference"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$intro", "fields": {"title": "Start here"}},
            {"op": "create", "type": "prose", "id": "$pv", "parent": "$s", "body": format!("{SHOWN} prose."), "links": {"decided_by": ["$dv"]}},
            {"op": "create", "type": "note", "id": "$np", "parent": "$s", "body": format!("{HIDDEN} note."), "links": {"decided_by": ["$dp"]}},
            {"op": "create", "type": "prose", "id": "$pp", "parent": "$s", "body": format!("{HIDDEN} prose."), "links": {"decided_by": ["$dp"]}},
            {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A figure"}, "links": {"decided_by": ["$dv"]}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r);
    let p = put(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$hide", "parent": ids["$admin"],
             "fields": {"key": "hide", "title": "Hide site", "audience": "org", "planned": "hide", "select": [ids["$intro"]]}},
            {"op": "create", "type": "publication_profile", "id": "$mark", "parent": ids["$admin"],
             "fields": {"key": "mark", "title": "Mark site", "audience": "org", "planned": "mark", "select": [ids["$intro"]]}},
            {"op": "create", "type": "publication_profile", "id": "$off", "parent": ids["$admin"],
             "fields": {"key": "off", "title": "Off site", "audience": "org", "planned": "mark", "since": "off", "select": [ids["$intro"]]}}
        ]),
    );
    ids.extend(p);
    land(&mut r);
    r.tag(&founder(), TAG, None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == TAG)
        .unwrap()
        .1;
    let landing = r.store().read_tag(&tag).unwrap().landing;
    Project {
        _dir: dir,
        r,
        ids,
        tag,
        landing,
        figure,
    }
}

impl Project {
    fn stream(&self) -> Vec<u8> {
        let mut out = Vec::new();
        rdy_engine::stream_export::write(&self.r.read(), self.tag, &BTreeSet::new(), &mut out)
            .unwrap();
        out
    }

    fn inputs(&self) -> RenderInputs {
        capture_inputs(&self.r.read(), &Scope::At(self.landing)).unwrap()
    }

    fn request(&self, selector: &str, format: RenderFormat) -> Vec<u8> {
        let mut out = Vec::new();
        write_request_prelude(selector, format, &self.inputs(), &mut out).unwrap();
        out.extend_from_slice(&self.stream());
        out
    }

    fn direct(&self, selector: &str) -> PublicationArtifact {
        let read = self.r.read();
        let inputs = self.inputs();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(&Scope::At(self.landing)).unwrap();
        build_publication(&source, &m, TAG, false, selector, &inputs).unwrap()
    }

    fn one_page(&self, selector: &str, path: &str) -> Vec<u8> {
        let read = self.r.read();
        let inputs = self.inputs();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(&Scope::At(self.landing)).unwrap();
        render_publication_page(&source, &m, TAG, false, selector, &inputs, path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .html
    }

    fn id(&self, ph: &str) -> String {
        self.ids[ph].clone()
    }
}

fn process(input: &[u8]) -> (ProcessSummary, Vec<u8>, PublicationArtifact) {
    let mut out = Vec::new();
    let s = run(Cursor::new(input.to_vec()), &mut out).unwrap_or_else(|e| panic!("{e}"));
    let a = read_artifact(Cursor::new(out.clone())).unwrap_or_else(|e| panic!("{e}"));
    (s, out, a)
}

#[track_caller]
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

fn all_text(a: &PublicationArtifact) -> String {
    a.files
        .iter()
        .filter(|(p, _)| !p.ends_with(".png"))
        .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ab5_captured_stream_and_direct_artifacts_match() {
    let p = project();
    // the tag's stream holds the planned block: the hiding is the profile's, not the history's
    let stream = p.stream();
    assert!(
        stream.windows(HIDDEN.len()).any(|w| w == HIDDEN.as_bytes()),
        "the stream is the whole history"
    );
    for (selector, hides) in [
        (p.id("$hide"), true),
        (p.id("$mark"), false),
        (p.id("$off"), false),
        (IMPLICIT_ORG.to_string(), false),
    ] {
        for format in [
            RenderFormat::Site,
            RenderFormat::Markdown,
            RenderFormat::Techdocs,
        ] {
            let what = format!("{selector} {format:?}");
            let direct_sel = p.direct(&selector);
            let (summary, out, a) = process(&p.request(&selector, format));
            if matches!(format, RenderFormat::Site) {
                same(&a, &direct_sel, &what);
            }
            assert_eq!(summary.manifest_hash, a.manifest_hash, "{what}");
            let text = all_text(&a);
            assert!(text.contains(SHOWN), "{what}");
            assert!(a.files.values().any(|f| *f == p.figure), "{what}");
            assert_eq!(text.contains(HIDDEN), !hides, "{what}");
            // hidden source ids never reach an artifact; a marked block shows `Planned`
            assert!(!text.contains(&p.id("$dp")), "{what}: the decision");
            // the reference doc shows the since label unless the profile turns it off
            assert_eq!(
                text.contains("since 1.2.0"),
                selector != p.id("$off"),
                "{what}"
            );
            assert!(
                !text.contains("since 1.2\n") && !text.contains("since 1.2 "),
                "{what}"
            );
            if hides {
                assert!(
                    !text.contains(&p.id("$np")) && !text.contains(&p.id("$pp")),
                    "{what}"
                );
                assert!(!text.contains("Planned"), "{what}");
            } else {
                assert!(text.contains("Planned"), "{what}");
            }
            // no new protocol field: the packet and its prelude keep their keys
            let len = u32::from_le_bytes(out[..4].try_into().unwrap()) as usize;
            let meta: Value = serde_json::from_slice(&out[4..4 + len]).unwrap();
            assert_eq!(
                meta.as_object().unwrap().keys().collect::<Vec<_>>(),
                ["manifest", "manifest_hash", "manifest_recipe", "warnings"],
                "{what}"
            );
            // a second call shares nothing with the first
            let (_, again, _) = process(&p.request(&selector, format));
            assert!(again == out, "{what}: repeated call");
        }
        // the one-page render gives the full site's page for the same captured inputs
        let full = p.direct(&selector);
        let page = p.one_page(&selector, "guide/intro.html");
        assert!(
            page == full.files["guide/intro.html"],
            "{selector}: one page"
        );
        assert_eq!(
            String::from_utf8_lossy(&page).contains(HIDDEN),
            !hides,
            "{selector}"
        );
    }
    // the prelude's keys are the old ones
    let mut prelude = Vec::new();
    write_request_prelude(
        &p.id("$hide"),
        RenderFormat::Site,
        &p.inputs(),
        &mut prelude,
    )
    .unwrap();
    let len = u32::from_le_bytes(prelude[..4].try_into().unwrap()) as usize;
    let value: Value = serde_json::from_slice(&prelude[4..4 + len]).unwrap();
    assert_eq!(
        value.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["format", "profile", "render_inputs"]
    );
}
