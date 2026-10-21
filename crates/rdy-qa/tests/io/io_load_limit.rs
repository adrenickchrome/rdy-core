//! A stopped load whose carried record is over its bounds (D-QF2HQ, D-2FMRR, D-KQ4CA). The
//! source holds more physical copies of a removed version than one carried record names. A load
//! of its chain stopped before the removal is refused before any target exists, with the same
//! error on the CLI and through rdy_load, and with a fix that works: the whole chain, with no
//! stop, loads and passes fsck. rdy_schema has one row for the code, naming the fixes of its three
//! cases: a redaction, a stopped load and a holder load. The holder load case has no remedy but
//! to keep the chain and the holder; the adapter tests pin it.
use rdy_engine::Rdy;
use rdy_engine::redaction::RedactInput;
use rdy_mcp::tools::ERROR_ROWS;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::segment::{Location, SegmentLog};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const SECRET: &str = "The launch code is ZEBRA-7731.";
/// One more physical copy than a carried record may name.
const COPIES: usize = 100_001;
const LIMIT_ROW_FIX: &str = "For a redaction, select fewer targets and run the dry run again. For a stopped load, stop at another place, or load the whole chain with no stop. For a holder load, keep the dump chain and the holder it came from; this rdy cannot load it.";

/// A served project with `COPIES` extra physical copies of the secret's block frame, and its
/// chain: the full dump `full` before the removal of every copy, and the increment `inc` after
/// it. No daemon runs.
struct Copies {
    s: Served,
    scratch: tempfile::TempDir,
    full: PathBuf,
    inc: PathBuf,
    full_end: Location,
}

fn add(r: &mut Rdy, body: &str) -> String {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d0", "title": "D0", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
    )
    .unwrap();
    w.created["$p"].clone()
}

fn remove(r: &mut Rdy, target: &str) {
    r.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    land(r, "redact").unwrap();
}

/// Every present data frame of the store `s`: kind and payload.
fn frames(s: &rdy_store::Store) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    s.reader()
        .for_each_record_from(
            Location {
                segment: 0,
                offset: 0,
            },
            s.log_end(),
            |_, f, _| {
                if let rdy_store::redaction::StoredFrame::Present { kind, payload, .. } = f {
                    out.push((kind, payload.to_vec()));
                }
                Ok(())
            },
        )
        .unwrap();
    out
}

fn copies() -> Copies {
    let s = Served::new();
    let scratch = short_tempdir();
    let root = s.dir.path();
    let mut r = Rdy::open(root).unwrap();
    let target = add(&mut r, SECRET);
    land(&mut r, "work").unwrap();
    r.close().unwrap();
    // more physical copies of the selected version, appended while the store is closed: each is
    // one span of the removal
    let store = root.join(".rdy/store");
    let st = rdy_store::Store::open(&store).unwrap();
    let raw = frames(&st)
        .into_iter()
        .find(|(kind, body)| {
            *kind == rdy_store::kinds::BLOCK
                && body.windows(SECRET.len()).any(|w| w == SECRET.as_bytes())
        })
        .map(|(_, body)| body)
        .expect("the secret's block frame");
    drop(st);
    let (mut log, q) =
        SegmentLog::open_marked(&store.join("segments"), 64 * 1024 * 1024, None).unwrap();
    assert!(q.is_empty());
    log.append_batch(&vec![(rdy_store::kinds::BLOCK, raw); COPIES])
        .unwrap();
    log.sync().unwrap();
    drop(log);
    // the full dump before the removal, the increment after it
    let mut r = Rdy::open(root).unwrap();
    let full = scratch.path().join("full.rdydump");
    let f = r.read().dump(&full).unwrap();
    remove(&mut r, &target);
    let inc = scratch.path().join("inc.rdydump");
    r.read().dump_after(&f.chain_end(), &inc).unwrap();
    r.close().unwrap();
    Copies {
        s,
        scratch,
        full,
        inc,
        full_end: f.end,
    }
}

fn arg(p: &Path) -> &str {
    p.to_str().unwrap()
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

fn triple(v: &Value) -> Value {
    json!({"code": v["code"], "message": v["message"], "fix": v["fix"]})
}

#[test]
fn plc1_stopped_load_limit_has_cli_mcp_parity() {
    let c = copies();
    let stop = format!("pos:{}.{}", c.full_end.segment, c.full_end.offset);
    let want = json!({
        "code": "E_REDACTION_LIMIT",
        "message": "The redaction exceeds 100000 carried spans.",
        "fix": "Stop at another place, or load the whole chain with no stop.",
    });
    let inputs: Vec<Vec<u8>> = [&c.full, &c.inc]
        .iter()
        .map(|f| std::fs::read(f).unwrap())
        .collect();
    // the CLI's stopped load
    let cli_dir = c.scratch.path().join("cli");
    let (ok, cli) = c.s.json(&[
        "--json",
        "load",
        arg(&c.full),
        arg(&cli_dir),
        "--then",
        arg(&c.inc),
        "--stop-at",
        &stop,
    ]);
    assert!(!ok, "{cli}");
    assert_eq!(triple(&cli), want, "{cli}");
    // the same load through rdy_load
    let mcp_dir = c.scratch.path().join("mcp");
    let mut m = agent(&c.s);
    let mcp = match m.call(
        "rdy_load",
        json!({"file": arg(&c.full), "dir": arg(&mcp_dir), "then": [arg(&c.inc)], "stop_at": stop}),
    ) {
        Reply::ToolError(v) => v,
        other => panic!("the stopped load passed: {other:?}"),
    };
    assert_eq!(triple(&mcp), want, "{mcp}");
    drop(m);
    c.s.stop();
    // neither made a target: no directory, no store.loading, no project file, no partial load
    for dir in [&cli_dir, &mcp_dir] {
        assert!(!dir.exists(), "{} exists", dir.display());
        assert!(!dir.join(".rdy/store.loading").exists());
        assert!(!dir.join(".rdy/store").exists());
        assert!(!dir.join(".rdy.yaml").exists());
    }
    let after: Vec<Vec<u8>> = [&c.full, &c.inc]
        .iter()
        .map(|f| std::fs::read(f).unwrap())
        .collect();
    assert_eq!(inputs, after, "no input changed");
    // the offered remedy works: the whole chain, with no stop, loads and passes fsck
    let whole = c.scratch.path().join("whole");
    let (ok, loaded) = c.s.json(&[
        "--json",
        "load",
        arg(&c.full),
        arg(&whole),
        "--then",
        arg(&c.inc),
    ]);
    assert!(ok, "the whole chain did not load: {loaded}");
    assert!(loaded["stopped_at"].is_null(), "{loaded}");
    let r = Rdy::open(&whole).unwrap();
    r.fsck().expect("the whole load passes fsck");
    r.close().unwrap();
}

#[test]
fn plc1_schema_has_one_limit_row_with_both_fixes() {
    // the runtime fixes of the first two cases keep their texts
    assert_eq!(
        rdy_engine::REDACTION_LIMIT_FIX,
        "Select fewer targets, then run the dry run again."
    );
    assert_eq!(
        rdy_engine::STOPPED_LOAD_REDACTION_LIMIT_FIX,
        "Stop at another place, or load the whole chain with no stop."
    );
    assert_eq!(
        rdy_engine::HOLDER_LOAD_REDACTION_LIMIT_FIX,
        "Keep the dump chain and the holder it came from. This rdy cannot load this chain."
    );
    let s = Served::new();
    let mut m = agent(&s);
    let schema = m.call("rdy_schema", json!({})).ok();
    assert_eq!(
        schema["errors"]["E_REDACTION_LIMIT"],
        json!({"when": "The redaction exceeds {limit}.", "fix": LIMIT_ROW_FIX}),
        "{}",
        schema["errors"]["E_REDACTION_LIMIT"]
    );
    drop(m);
    s.stop();
    let rows: Vec<_> = ERROR_ROWS
        .iter()
        .filter(|(code, _, _)| *code == "E_REDACTION_LIMIT")
        .collect();
    assert_eq!(
        rows,
        vec![&(
            "E_REDACTION_LIMIT",
            "The redaction exceeds {limit}.",
            LIMIT_ROW_FIX
        )]
    );
}
