//! hc8 (D-2FMRR, D-FR0C4, D-QF2HQ): the last-surface cross-path test. A full dump and two
//! increments are made through MCP as the project grows; the CLI loads the chain; the engine dumps
//! the loaded store; MCP loads that dump. The repo, tags, positions, hashes and warning targets
//! agree at every step. Adapter parity is pinned in hc4 and hc7.
use rdy_engine::redaction::RedactInput;
use rdy_engine::{DumpSummary, Rdy};
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::every_kind::facts;
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const SECRET: &str = "The launch code is ZEBRA-7731.";

fn add(r: &mut Rdy, n: usize, body: &str) -> String {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": format!("D{n}"), "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
    )
    .unwrap();
    w.created["$p"].clone()
}

fn agent(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    c
}

fn arg(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn segs(project: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(project.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

/// What a store's full dump says of it: repo, positions and hashes (the dump's own digest).
fn shape(d: &DumpSummary) -> Value {
    let mut v = serde_json::to_value(d).unwrap();
    v.as_object_mut().unwrap().remove("out");
    v.as_object_mut().unwrap().remove("bytes");
    v
}

fn full_dump(project: &Path, out: &Path) -> DumpSummary {
    let r = Rdy::open(project).unwrap();
    let d = r.read().dump(out).unwrap();
    r.close().unwrap();
    d
}

fn targets(warnings: &Value) -> Vec<String> {
    warnings
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|w| w["issues"].as_array().unwrap().clone())
        .map(|i| i["target"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn hc8_chain_round_trips_across_mcp_cli_and_engine() {
    let s = Served::new();
    let scratch = short_tempdir();
    let (f0, i1, i2) = (
        scratch.path().join("f0.rdydump"),
        scratch.path().join("i1.rdydump"),
        scratch.path().join("i2.rdydump"),
    );
    // the project grows between the three dumps, each made through the tool
    let mut r = Rdy::open(s.dir.path()).unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r, "work").unwrap();
    let block = add(&mut r, 1, SECRET);
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
    let mut m = agent(&s);
    let full = m.call("rdy_dump", json!({"out": arg(&f0)})).ok();
    assert_eq!(full["kind"], "full", "{full}");
    drop(m);
    s.stop();

    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![block.clone()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    land(&mut r, "redact").unwrap();
    add(&mut r, 2, "a later block");
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    r.close().unwrap();
    let mut m = agent(&s);
    let inc1 = m
        .call("rdy_dump", json!({"out": arg(&i1), "after": arg(&f0)}))
        .ok();
    assert_eq!(inc1["kind"], "increment", "{inc1}");
    drop(m);
    s.stop();

    let mut r = Rdy::open(s.dir.path()).unwrap();
    add(&mut r, 3, "the last block");
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v3.0.0", None, None).unwrap();
    r.close().unwrap();
    let mut m = agent(&s);
    let inc2 = m
        .call("rdy_dump", json!({"out": arg(&i2), "after": arg(&i1)}))
        .ok();
    assert_eq!(inc2["base"], inc1["end"], "the chain is unbroken: {inc2}");
    drop(m);
    s.stop();
    let inputs: Vec<Vec<u8>> = [&f0, &i1, &i2]
        .iter()
        .map(|f| std::fs::read(f).unwrap())
        .collect();
    let source_segs = segs(s.dir.path());

    // the source's own full dump is the reference for every later load
    let reference = full_dump(s.dir.path(), &scratch.path().join("source.rdydump"));

    // the CLI loads the chain
    let cli_dir = scratch.path().join("cli");
    let (ok, cli) = s.json(&[
        "--json",
        "load",
        arg(&f0),
        arg(&cli_dir),
        "--then",
        arg(&i1),
        "--then",
        arg(&i2),
    ]);
    assert!(ok, "{cli}");
    assert!(
        cli["stopped_at"].is_null() && cli.get("warnings").is_none(),
        "{cli}"
    );
    let source = Rdy::open(s.dir.path()).unwrap();
    let loaded = Rdy::open(&cli_dir).unwrap();
    assert_eq!(facts(&loaded), facts(&source), "repo, refs, landings");
    let tags = |r: &Rdy| -> Value {
        json!(
            r.read()
                .reader()
                .refs_with_prefix("")
                .unwrap()
                .into_iter()
                .map(|(n, _)| n)
                .filter(|n| n.contains("v1.0.0") || n.contains("v2.0.0") || n.contains("v3.0.0"))
                .collect::<Vec<_>>()
        )
    };
    assert_eq!(
        tags(&loaded).as_array().unwrap().len(),
        3,
        "{}",
        tags(&loaded)
    );
    assert_eq!(tags(&loaded), tags(&source));
    loaded.close().unwrap();
    source.close().unwrap();
    assert_eq!(
        segs(&cli_dir)
            .into_iter()
            .map(|(n, b)| (n, b.len()))
            .collect::<Vec<_>>(),
        source_segs
            .iter()
            .map(|(n, b)| (n.clone(), b.len()))
            .collect::<Vec<_>>(),
        "the loaded log is as long as the source's"
    );

    // the engine dumps the loaded store; its positions and hashes are the source's
    let engine = scratch.path().join("engine.rdydump");
    let from_cli = full_dump(&cli_dir, &engine);
    assert_eq!(shape(&from_cli), shape(&reference), "positions and hashes");

    // MCP loads that dump
    let mut m = agent(&s);
    let mcp_dir = scratch.path().join("mcp");
    let l = m
        .call(
            "rdy_load",
            json!({"file": arg(&engine), "dir": arg(&mcp_dir)}),
        )
        .ok();
    assert_eq!(l["batches"], cli["batches"], "{l}");
    assert_eq!(l["landings"], cli["landings"], "{l}");
    assert_eq!(l["tags"], cli["tags"], "{l}");
    assert!(
        l["fsck"].get("warnings").is_none_or(|w| w == &json!([])),
        "{l}"
    );
    let again = Rdy::open(&mcp_dir).unwrap();
    let first = Rdy::open(&cli_dir).unwrap();
    assert_eq!(facts(&again), facts(&first));
    again.close().unwrap();
    first.close().unwrap();
    assert_eq!(
        shape(&full_dump(&mcp_dir, &scratch.path().join("mcp.rdydump"))),
        shape(&reference)
    );

    // a stop before the removal: the warning targets agree on the CLI, in the engine's dump of
    // the stopped store, and in the MCP load of that dump
    let stopped_dir = scratch.path().join("stopped");
    let (ok, stopped) = s.json(&[
        "--json",
        "load",
        arg(&f0),
        arg(&stopped_dir),
        "--then",
        arg(&i1),
        "--then",
        arg(&i2),
        "--stop-at",
        "tag:v1.0.0",
    ]);
    assert!(ok, "{stopped}");
    let cli_targets = targets(&stopped["warnings"]);
    assert_eq!(cli_targets.len(), 1, "{stopped}");
    assert!(cli_targets[0].starts_with(&block), "{cli_targets:?}");
    let stopped_dump = scratch.path().join("stopped.rdydump");
    let sd = full_dump(&stopped_dir, &stopped_dump);
    assert_eq!(sd.kind, rdy_store::dump_chain::DumpKind::Full);
    let replay = scratch.path().join("replay");
    let r = m
        .call(
            "rdy_load",
            json!({"file": arg(&stopped_dump), "dir": arg(&replay)}),
        )
        .ok();
    assert_eq!(targets(&r["fsck"]["warnings"]), cli_targets, "{r}");
    assert_eq!(
        full_dump(&replay, &scratch.path().join("replay.rdydump")).digest,
        sd.digest,
        "the stopped store's hashes survive the round trip"
    );
    // nothing the loads read was changed
    drop(m);
    s.stop();
    let after: Vec<Vec<u8>> = [&f0, &i1, &i2]
        .iter()
        .map(|f| std::fs::read(f).unwrap())
        .collect();
    assert_eq!(inputs, after);
    assert_eq!(source_segs, segs(s.dir.path()));
}
