//! hpb1 (D-KDFZ3, D-QWSTV, D-84QSN): the rendering inputs a publication records. Capture takes
//! the source's settings: this rdy's version, the rdy-standard theme, the effective pack, and the
//! project's title and description. Nothing else of the config enters them.
use rdy_engine::render_inputs::{
    DisagreementInput, RDY_VERSION, RenderInputs, THEME, capture, disagreement_sentence,
    read_inputs, to_bounded_json, validate_inputs,
};
use rdy_engine::{Rdy, RdyError, did_you_mean};
use rdy_model::BlockId;
use std::path::Path;

/// The workspace version, read from the workspace manifest rather than from this crate.
fn workspace_version() -> String {
    let toml = include_str!("../../../../Cargo.toml");
    let package = toml.split("[workspace.package]").nth(1).unwrap();
    let line = package
        .lines()
        .find(|l| l.starts_with("version = "))
        .unwrap();
    line.trim_start_matches("version = ")
        .trim_matches('"')
        .to_string()
}

/// A project at `dir/name` whose `.rdy.yaml` is `config` (after the pack line).
fn project(dir: &Path, name: &str, config: &str) -> (std::path::PathBuf, Rdy) {
    let root = dir.join(name);
    std::fs::create_dir(&root).unwrap();
    drop(Rdy::init(&root, "rdy-standard").unwrap());
    std::fs::write(
        root.join(".rdy.yaml"),
        format!("pack: rdy-standard\n{config}"),
    )
    .unwrap();
    let r = Rdy::open(&root).unwrap();
    (root, r)
}

const SENTINELS: &str = "\
name: \"  Sentinel Docs  \"
description: One line about the docs.
remotes:
  origin: {url: \"https://hub.example.invalid/SENTINEL-REMOTE\", mode: publish}
signing:
  sign: never
  allowed_signers: SENTINEL-SIGNERS
serve:
  port: 41999
";

#[track_caller]
fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix)
    );
}

#[track_caller]
fn mismatch(i: &RenderInputs) {
    assert_err(
        &validate_inputs(i).unwrap_err(),
        "E_BAD_INPUT",
        "render inputs do not match this renderer",
        "use the rdy-standard theme and a non-empty title",
    );
}

#[track_caller]
fn over(i: &RenderInputs, limit: &str) {
    assert_err(
        &validate_inputs(i).unwrap_err(),
        "E_LIMIT",
        &format!("render inputs exceed {limit}"),
        "reduce the render inputs to the stated limit",
    );
    assert_eq!(
        to_bounded_json(i).unwrap_err().message,
        format!("render inputs exceed {limit}")
    );
}

#[track_caller]
fn malformed(i: &RenderInputs) {
    assert_err(
        &validate_inputs(i).unwrap_err(),
        "E_BAD_INPUT",
        "render disagreement inputs are invalid",
        "capture the render inputs from the source again",
    );
}

#[track_caller]
fn strict(json: &serde_json::Value, field: &str) {
    let msg = serde_json::from_value::<RenderInputs>(json.clone())
        .unwrap_err()
        .to_string();
    let e = did_you_mean::unknown_field_error_in("render inputs", &msg, json)
        .unwrap_or_else(|| panic!("not an unknown-field error: {msg}"));
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        format!("render inputs: unknown field \"{field}\"")
    );
}

fn bid(i: usize) -> BlockId {
    const C: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let body: String = (0..5)
        .rev()
        .map(|k| C[(i >> (5 * k)) & 31] as char)
        .collect();
    BlockId::parse(&format!("B-{body}")).unwrap()
}

fn row(kind: &str, blocks: Vec<BlockId>, lead: Option<BlockId>) -> DisagreementInput {
    DisagreementInput {
        sentence: disagreement_sentence(kind).unwrap().to_string(),
        blocks,
        lead,
    }
}

#[test]
fn hpb1_render_inputs_capture_source_settings_and_refuse_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (root, r) = project(dir.path(), "source-dir", SENTINELS);
    let st = r.read();
    let i = capture(&st).unwrap();
    assert_eq!(i.rdy, workspace_version(), "the compiled workspace version");
    assert_eq!(i.theme, "rdy-standard");
    assert_eq!(THEME, "rdy-standard");
    assert_eq!(i.title, "Sentinel Docs");
    assert_eq!(i.title, st.project_name());
    assert_eq!(i.description.as_deref(), Some("One line about the docs."));
    assert_eq!(&i.pack, st.pack(), "the effective pack");
    assert!(i.disagreements.is_empty(), "capture fills no disagreement");
    validate_inputs(&i).unwrap();

    let json = to_bounded_json(&i).unwrap();
    assert_eq!(json, serde_json::to_vec(&i).unwrap(), "compact JSON");
    let text = String::from_utf8(json.clone()).unwrap();
    for secret in [
        "SENTINEL",
        "hub.example.invalid",
        "41999",
        "remotes",
        "signing",
        "allowed_signers",
        root.to_str().unwrap(),
        "source-dir",
        ".rdy.yaml",
    ] {
        assert!(!text.contains(secret), "{secret} stays out of the inputs");
    }
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "description",
            "disagreements",
            "pack",
            "rdy",
            "theme",
            "title"
        ]
        .into_iter()
        .collect()
    );
    let back: RenderInputs = serde_json::from_slice(&json).unwrap();
    assert_eq!(back, i, "the inputs read back equal");

    // unknown keys, at the top, in a disagreement row and inside the pack
    let mut top = v.clone();
    top["token"] = "x".into();
    strict(&top, "token");
    let mut nested = v.clone();
    nested["disagreements"] = serde_json::json!([
        {"sentence": disagreement_sentence("fork").unwrap(), "blocks": ["B-00001"], "lead": null, "kind": "fork"}
    ]);
    strict(&nested, "disagreements[0].kind");
    let mut packed = v.clone();
    packed["pack"]["config_path"] = "/x".into();
    let msg = serde_json::from_value::<RenderInputs>(packed)
        .unwrap_err()
        .to_string();
    assert!(msg.starts_with("unknown field `config_path`"), "{msg}");

    // another capturing version is read (D-8RGWW); another theme or an empty title is refused
    for rdy in ["0.0.1", "99.0.0", ""] {
        let mut other = i.clone();
        other.rdy = rdy.into();
        validate_inputs(&other).unwrap();
    }
    let changes: [fn(&mut RenderInputs); 3] = [
        |i: &mut RenderInputs| i.theme = "rdy-dark".into(),
        |i: &mut RenderInputs| i.theme = "RDY-STANDARD".into(),
        |i: &mut RenderInputs| i.title = String::new(),
    ];
    for change in changes {
        let mut bad = i.clone();
        change(&mut bad);
        mismatch(&bad);
    }

    // an unnamed project takes its directory's name: two clones can differ
    let (_, a) = project(dir.path(), "clone_one-a", "");
    let (_, b) = project(dir.path(), "clone-two", "");
    let ta = capture(&a.read()).unwrap();
    let tb = capture(&b.read()).unwrap();
    assert_eq!(ta.title, "Clone one a");
    assert_eq!(tb.title, "Clone two");
    assert_eq!(ta.description, None);
    assert_ne!(to_bounded_json(&ta).unwrap(), to_bounded_json(&tb).unwrap());
    // a shared name makes them equal
    let (_, c) = project(dir.path(), "clone-three", "name: Shared\n");
    let (_, d) = project(dir.path(), "clone-four", "name: Shared\n");
    assert_eq!(
        to_bounded_json(&capture(&c.read()).unwrap()).unwrap(),
        to_bounded_json(&capture(&d.read()).unwrap()).unwrap()
    );
}

#[test]
fn hpb1_disagreement_sentences_are_the_closed_table() {
    let table = [
        ("fork", "These pages give two versions of one point."),
        (
            "duplicate_definition",
            "These pages define the same thing twice.",
        ),
        (
            "deprecated_reference",
            "This page still points to something that was retired.",
        ),
        (
            "value_disagreement",
            "These pages state different values for the same thing.",
        ),
        (
            "restatement_drift",
            "This page restates a point that has since changed.",
        ),
        (
            "semantic",
            "A reviewer found that these statements contradict each other.",
        ),
    ];
    for (kind, sentence) in table {
        assert_eq!(disagreement_sentence(kind), Some(sentence), "{kind}");
    }
    for other in [
        "edit_collision",
        "premise_shift",
        "area_overlap",
        "unjustified_change",
        "Fork",
        "",
    ] {
        assert_eq!(disagreement_sentence(other), None, "{other}");
    }
}

#[test]
fn hpb1_render_inputs_bounds_and_disagreement_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (_, r) = project(dir.path(), "p", "name: P\n");
    let base = capture(&r.read()).unwrap();
    let with = |f: &dyn Fn(&mut RenderInputs)| {
        let mut i = base.clone();
        f(&mut i);
        i
    };

    // title and description
    validate_inputs(&with(&|i| i.title = "t".repeat(4096))).unwrap();
    over(&with(&|i| i.title = "t".repeat(4097)), "4096 title bytes");
    validate_inputs(&with(&|i| i.description = Some("d".repeat(65_536)))).unwrap();
    validate_inputs(&with(&|i| i.description = Some(String::new()))).unwrap();
    over(
        &with(&|i| i.description = Some("d".repeat(65_537))),
        "65536 description bytes",
    );

    // well-formed rows, in the captured order, which is kept
    let ok = with(&|i| {
        i.disagreements = vec![
            row("semantic", vec![bid(2), bid(1)], None),
            row("deprecated_reference", vec![bid(3), bid(4)], Some(bid(4))),
            row("fork", vec![bid(1), bid(2)], None),
        ]
    });
    validate_inputs(&ok).unwrap();
    let back: RenderInputs = serde_json::from_slice(&to_bounded_json(&ok).unwrap()).unwrap();
    assert_eq!(back.disagreements, ok.disagreements, "row order is kept");

    // malformed rows
    let bad_rows: Vec<Vec<DisagreementInput>> = vec![
        vec![DisagreementInput {
            sentence: "These pages disagree.".into(),
            blocks: vec![bid(1)],
            lead: None,
        }],
        vec![DisagreementInput {
            sentence: "fork".into(),
            blocks: vec![bid(1)],
            lead: None,
        }],
        vec![row("fork", vec![], None)],
        vec![row("fork", vec![bid(1), bid(1)], None)],
        vec![row("fork", vec![bid(1)], Some(bid(2)))],
        vec![
            row("fork", vec![bid(1), bid(2)], None),
            row("fork", vec![bid(1), bid(2)], None),
        ],
    ];
    for rows in bad_rows {
        malformed(&with(&|i| i.disagreements = rows.clone()));
    }
    // the same blocks under another sentence, or in another order, are another row
    validate_inputs(&with(&|i| {
        i.disagreements = vec![
            row("fork", vec![bid(1), bid(2)], None),
            row("fork", vec![bid(2), bid(1)], None),
            row("semantic", vec![bid(1), bid(2)], None),
        ]
    }))
    .unwrap();

    // block references in total: 100,000
    let refs =
        |n: usize| with(&|i| i.disagreements = vec![row("fork", (0..n).map(bid).collect(), None)]);
    validate_inputs(&refs(100_000)).unwrap();
    over(&refs(100_001), "100000 disagreement block references");
    let split = with(&|i| {
        i.disagreements = vec![
            row("fork", (0..50_000).map(bid).collect(), None),
            row("semantic", (50_000..100_001).map(bid).collect(), None),
        ]
    });
    over(&split, "100000 disagreement block references");

    // rows: 100,001 is over; at 100,000 the rows pass and the value count is what stops them
    let rows = |n: usize| {
        with(&|i| i.disagreements = (0..n).map(|k| row("fork", vec![bid(k)], None)).collect())
    };
    over(&rows(100_001), "100000 disagreement rows");
    over(&rows(100_000), "200000 values");

    // values: list elements and map members of the whole input
    let links = |n: usize| {
        with(&|i| {
            i.pack.link_types = (0..n).map(|k| format!("l{k}")).collect();
        })
    };
    let fixed = {
        let none = links(0);
        let v = serde_json::to_value(&none).unwrap();
        count_values(&v)
    };
    validate_inputs(&links(200_000 - fixed)).unwrap();
    over(&links(200_000 - fixed + 1), "200000 values");

    // a scalar: 1 MiB
    let area = |len: usize| {
        with(&|i| {
            i.pack.areas.insert("big".into(), area_def(len));
        })
    };
    validate_inputs(&area(1_048_576)).unwrap();
    over(&area(1_048_577), "1048576 scalar bytes");

    // bytes: 8 MiB of compact JSON, each scalar under its own bound
    let areas = |extra: usize| {
        let target = 8_388_608 + extra;
        let mut i = base.clone();
        for k in 0.. {
            let name = format!("a{k:02}");
            i.pack.areas.insert(name.clone(), area_def(0));
            let need = target - serde_json::to_vec(&i).unwrap().len();
            let len = need.min(1_000_000);
            i.pack.areas.insert(name, area_def(len));
            if len == need {
                break;
            }
        }
        i
    };
    let at = areas(0);
    let len = serde_json::to_vec(&at).unwrap().len();
    assert_eq!(len, 8_388_608, "the input is at the byte limit");
    validate_inputs(&at).unwrap();
    assert_eq!(to_bounded_json(&at).unwrap().len(), 8_388_608);
    let past = areas(1);
    assert_eq!(serde_json::to_vec(&past).unwrap().len(), 8_388_609);
    over(&past, "8388608 bytes");
}

/// List elements and map members of `v`, as the input bound counts them.
fn count_values(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Array(a) => a.len() + a.iter().map(count_values).sum::<usize>(),
        serde_json::Value::Object(o) => o.len() + o.values().map(count_values).sum::<usize>(),
        _ => 0,
    }
}

fn area_def(len: usize) -> rdy_schema::AreaDef {
    rdy_schema::AreaDef {
        description: "a".repeat(len),
        paths: vec![],
    }
}

/// Fix round 1: `BlockId::MIN` is the empty range-bound sentinel, not a valid id, so a row that
/// names it is refused by both the validator and the serializer, as a block or as the lead.
#[test]
fn hpb1_render_inputs_refuse_the_min_block_id() {
    let dir = tempfile::tempdir().unwrap();
    let (_, r) = project(dir.path(), "p", "name: P\n");
    let base = capture(&r.read()).unwrap();
    let cases: Vec<Vec<DisagreementInput>> = vec![
        vec![row("fork", vec![BlockId::MIN], Some(BlockId::MIN))],
        vec![row("fork", vec![BlockId::MIN], None)],
        vec![row("semantic", vec![bid(1), BlockId::MIN], Some(bid(1)))],
        vec![
            row("fork", vec![bid(1), bid(2)], None),
            row("fork", vec![BlockId::MIN, bid(2)], Some(BlockId::MIN)),
        ],
    ];
    for rows in cases {
        let mut i = base.clone();
        i.disagreements = rows;
        malformed(&i);
        assert_err(
            &to_bounded_json(&i).unwrap_err(),
            "E_BAD_INPUT",
            "render disagreement inputs are invalid",
            "capture the render inputs from the source again",
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Sync client A Task 1c (D-8RGWW): inputs from any captured version, read whole or refused
// ---------------------------------------------------------------------------------------------

/// The captured inputs of a small project, as JSON.
fn captured_json() -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let (_, r) = project(dir.path(), "p", "name: P\n");
    serde_json::to_value(capture(&r.read()).unwrap()).unwrap()
}

fn read_value(v: &serde_json::Value) -> Result<RenderInputs, RdyError> {
    read_inputs(&serde_json::to_vec(v).unwrap())
}

/// D-8RGWW's refusal of inputs that rdy `captured` wrote.
#[track_caller]
fn unfit(e: &RdyError, captured: &str) {
    assert_err(
        e,
        "E_BAD_INPUT",
        &format!("render inputs from rdy {captured} do not fit rdy {RDY_VERSION}"),
        &format!(
            "capture the render inputs with rdy {RDY_VERSION}, or render with rdy {captured} or later"
        ),
    );
}

#[test]
fn scc1_render_inputs_older_and_newer_versions_give_this_renderers_bytes() {
    let base = captured_json();
    let current = read_value(&base).unwrap();
    assert_eq!(current.rdy, RDY_VERSION);
    for label in ["0.1.0", "0.0.1", "99.99.99", "1.0.0-rc.1"] {
        let mut v = base.clone();
        v["rdy"] = label.into();
        let got = read_value(&v).unwrap();
        assert_eq!(got.rdy, label, "the capturing client is kept as named");
        // equal inputs but the label: the same everything else
        let mut same = got.clone();
        same.rdy = current.rdy.clone();
        assert_eq!(same, current);
        assert_eq!(
            to_bounded_json(&same).unwrap(),
            to_bounded_json(&current).unwrap()
        );
    }
}

#[test]
fn scc1_render_inputs_unknown_member_names_both_versions() {
    let base = captured_json();
    for (path, label) in [
        (vec!["token"], "0.9.0"),
        (vec!["theme_variant"], "99.0.0"),
        (vec!["pack", "future_field"], "99.1.0"),
        (vec!["pack", "block_types", "decision", "future"], "99.1.0"),
    ] {
        let mut v = base.clone();
        v["rdy"] = label.into();
        let mut at = &mut v;
        for k in &path[..path.len() - 1] {
            at = &mut at[*k];
        }
        at.as_object_mut()
            .unwrap_or_else(|| panic!("{path:?} is no object"))
            .insert(path[path.len() - 1].into(), 1.into());
        unfit(&read_value(&v).unwrap_err(), label);
    }
    // a member in a disagreement row
    let mut v = base.clone();
    v["rdy"] = "99.0.0".into();
    v["disagreements"] = serde_json::json!([
        {"sentence": disagreement_sentence("fork").unwrap(), "blocks": ["B-00001"], "lead": null, "kind": "fork"}
    ]);
    unfit(&read_value(&v).unwrap_err(), "99.0.0");
}

#[test]
fn scc1_render_inputs_unknown_pack_member_names_both_versions() {
    let base = captured_json();
    for (key, label) in [("nmae", "0.7.0"), ("config_path", "99.0.0")] {
        let mut v = base.clone();
        v["rdy"] = label.into();
        v["pack"][key] = "x".into();
        unfit(&read_value(&v).unwrap_err(), label);
    }
    // a `tensions_across` key the pack parser would skip is refused here
    for key in ["future_kind", "fork"] {
        let mut v = base.clone();
        v["rdy"] = "99.2.0".into();
        v["pack"]["tensions_across"] = serde_json::json!({ key: "warn" });
        unfit(&read_value(&v).unwrap_err(), "99.2.0");
    }
    // a known kind stays readable
    let mut v = base.clone();
    v["pack"]["tensions_across"] = serde_json::json!({"value_disagreement": "blocking"});
    let got = read_value(&v).unwrap();
    assert_eq!(got.pack.tensions_across.len(), 1);
}

#[test]
fn scc1_render_inputs_missing_needed_member_names_both_versions() {
    let base = captured_json();
    for (path, label) in [
        (vec!["title"], "0.8.0"),
        (vec!["theme"], "99.0.0"),
        (vec!["pack", "name"], "0.8.0"),
        (vec!["pack", "version"], "99.3.0"),
    ] {
        let mut v = base.clone();
        v["rdy"] = label.into();
        let mut at = &mut v;
        for k in &path[..path.len() - 1] {
            at = &mut at[*k];
        }
        at.as_object_mut()
            .unwrap()
            .remove(path[path.len() - 1])
            .unwrap();
        unfit(&read_value(&v).unwrap_err(), label);
    }
}

#[test]
fn scc1_render_inputs_theme_and_empty_title_still_refuse() {
    let base = captured_json();
    for label in [RDY_VERSION, "0.0.1", "99.0.0"] {
        let mut theme = base.clone();
        theme["rdy"] = label.into();
        theme["theme"] = "rdy-dark".into();
        let mut title = base.clone();
        title["rdy"] = label.into();
        title["title"] = "".into();
        for v in [theme, title] {
            assert_err(
                &read_value(&v).unwrap_err(),
                "E_BAD_INPUT",
                "render inputs do not match this renderer",
                "use the rdy-standard theme and a non-empty title",
            );
        }
    }
}

#[test]
fn scc1_render_inputs_bounds_stay_exact() {
    let base = captured_json();
    let bytes = |v: &serde_json::Value| serde_json::to_vec(v).unwrap();
    // too many bytes: refused before any parse
    let mut big = bytes(&base);
    big.resize(8_388_609, b' ');
    assert_err(
        &read_inputs(&big).unwrap_err(),
        "E_LIMIT",
        "render inputs exceed 8388608 bytes",
        "reduce the render inputs to the stated limit",
    );
    // a title, a scalar, a value count and a depth over their bounds, from any captured version
    let limit = |v: &serde_json::Value, what: &str| {
        assert_err(
            &read_value(v).unwrap_err(),
            "E_LIMIT",
            &format!("render inputs exceed {what}"),
            "reduce the render inputs to the stated limit",
        );
    };
    let mut v = base.clone();
    v["rdy"] = "99.0.0".into();
    v["title"] = "t".repeat(4097).into();
    limit(&v, "4096 title bytes");
    let mut v = base.clone();
    v["rdy"] = "0.0.1".into();
    v["pack"]["areas"] =
        serde_json::json!({"big": {"description": "a".repeat(1_048_577), "paths": []}});
    limit(&v, "1048576 scalar bytes");
    let mut v = base.clone();
    v["pack"]["link_types"] = (0..200_001)
        .map(|k| format!("l{k}"))
        .collect::<Vec<_>>()
        .into();
    limit(&v, "200000 values");
    let mut v = base.clone();
    v["description"] = "d".repeat(65_537).into();
    limit(&v, "65536 description bytes");
    // at the limits the inputs read
    let mut ok = base.clone();
    ok["title"] = "t".repeat(4096).into();
    read_value(&ok).unwrap();
}
