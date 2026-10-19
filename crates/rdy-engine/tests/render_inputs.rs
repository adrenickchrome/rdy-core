//! hpb1 (D-KDFZ3, D-QWSTV, D-84QSN): the rendering inputs a publication records. Capture takes
//! the source's settings: this rdy's version, the rdy-standard theme, the effective pack, and the
//! project's title and description. Nothing else of the config enters them.
use rdy_engine::render_inputs::{
    DisagreementInput, RenderInputs, THEME, capture, disagreement_sentence, to_bounded_json,
    validate_inputs,
};
use rdy_engine::{Rdy, RdyError, did_you_mean};
use rdy_model::BlockId;
use std::path::Path;

/// The workspace version, read from the workspace manifest rather than from this crate.
fn workspace_version() -> String {
    let toml = include_str!("../../../Cargo.toml");
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
        "use this rdy version and the rdy-standard theme",
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

    // a different renderer is refused; no fallback is substituted
    let changes: [fn(&mut RenderInputs); 5] = [
        |i: &mut RenderInputs| i.rdy = "0.0.1".into(),
        |i: &mut RenderInputs| i.rdy = String::new(),
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
