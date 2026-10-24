//! hc6 (D-QF2HQ, D-KQ4CA): the MCP overview describes the current store, so it shows the
//! carried-removal warning whatever its content scope. Its serialized size is reserved before
//! the tree is trimmed, and it is never cut: when it cannot fit, the overview returns
//! `E_REDACTION_BUDGET` with the carried-specific fix.
use rdy_engine::Session;
use rdy_model::redaction::{
    BlockResidue, CarriedSpan, ChangeResidue, FramePlace, ReasonClass, RedactTarget,
    RedactionCarried, RedactionResidue,
};
use rdy_model::{Block, Change, Hash, Op, Provenance, Role};
use rdy_qa::fixtures::{project, write};
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const SECRET: &str = "The launch code is ZEBRA-7731.";

fn enc<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut buf = vec![0u8; 1 << 20];
    rdy_model::redaction::encode_into(value, &mut buf)
        .unwrap()
        .to_vec()
}

/// The store of the project at `dir`, moved aside and ingested again with one carried record
/// over the frames that hold `ZEBRA-7731`, all `held` (the checked path of a stopped load).
fn carry(dir: &Path) {
    let store = dir.join(".rdy").join(rdy_store::layout::STORE_DIR);
    let aside = dir.join("store-aside");
    std::fs::rename(&store, &aside).unwrap();
    let s = rdy_store::Store::open(&aside).unwrap();
    let mut spans: Vec<CarriedSpan> = vec![];
    let mut residues: Vec<RedactionResidue> = vec![];
    let mut target = None;
    s.reader()
        .for_each_record_from(
            Location {
                segment: 0,
                offset: 0,
            },
            s.log_end(),
            |loc, f, _| {
                let StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } = f
                else {
                    return Ok(());
                };
                let residue = if kind == kinds::BLOCK
                    && let Ok(b) = rdy_store::bounded::decode::<Block>(payload)
                    && b.body.contains("ZEBRA-7731")
                {
                    target = Some((b.id.clone(), b.version_hash()));
                    RedactionResidue::Block(BlockResidue::of(&b))
                } else if kind == kinds::CHANGE
                    && let Ok(c) = rdy_store::bounded::decode::<Change>(payload)
                    && c.ops.iter().any(|op| match op {
                        Op::Create { block } | Op::Update { block, .. } => {
                            block.body.contains("ZEBRA-7731")
                        }
                        Op::Delete { .. } => false,
                    })
                {
                    RedactionResidue::Change(ChangeResidue::of(&c))
                } else {
                    return Ok(());
                };
                spans.push(CarriedSpan {
                    place: FramePlace {
                        segment: loc.segment,
                        offset: loc.offset,
                    },
                    kind,
                    hash,
                    len: payload.len() as u32,
                    residue: Some(Hash::of(&enc(&residue))),
                    held: true,
                });
                residues.push(residue);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(spans.len(), 2, "the secret's block and change frames");
    let (id, version) = target.unwrap();
    let rec = RedactionCarried {
        request: Hash::of(b"source request"),
        apply: Hash::of(b"source apply"),
        landing: Hash::of(b"source landing"),
        approved_by: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 777,
        },
        reason_class: ReasonClass::Secret,
        targets: vec![RedactTarget::Version { id, version }],
        spans,
    };
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    drop(s);
    let mut i = rdy_store::ingest::Ingest::create(&store).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.append_carried(&residues, std::slice::from_ref(&rec))
        .unwrap();
    i.upgrade(&rdy_store::format::DumpStore::Features(
        rdy_store::format::baseline_features(),
    ))
    .unwrap();
    i.finish().unwrap();
}

/// A project with the secret landed, then carrying a removal it does not repeat; and one that
/// does not.
fn projects() -> (tempfile::TempDir, tempfile::TempDir) {
    let build = |carrying: bool| {
        let (dir, mut r) = project();
        write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Launch", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET}
            ]),
        )
        .unwrap();
        let p = r
            .propose(
                &Session::human("human:qa".into(), Role::Writer).unwrap(),
                "work",
                "Base",
                Some(TEST_SUMMARY),
            )
            .unwrap();
        r.land(
            &Session::human("human:qa".into(), Role::Approver).unwrap(),
            &p.id,
        )
        .unwrap();
        r.close().unwrap();
        if carrying {
            carry(dir.path());
        }
        dir
    };
    (build(true), build(false))
}

fn session(c: &mut McpClient) {
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "r1", "role": "reader"}),
    )
    .ok();
}

fn tokens(v: &Value) -> usize {
    serde_json::to_string(v).unwrap().len() / 4
}

#[test]
fn hc6_overview_keeps_warning_or_returns_budget_error() {
    let (carrying, plain) = projects();
    let mut c = McpClient::start(carrying.path());
    session(&mut c);

    // the whole warning is in the overview, in every content scope
    let full = c.call("rdy_overview", json!({})).ok();
    let warnings = full["redaction_warnings"].clone();
    assert_eq!(warnings.as_array().unwrap().len(), 1, "{full}");
    let w = &warnings[0];
    assert_eq!(w["code"], "W_STOP_BEFORE_REDACTION");
    assert_eq!(
        w["fix"],
        "Propose the redaction of the listed targets again, and have a person approve it."
    );
    assert_eq!(w["issues"].as_array().unwrap().len(), 1);
    let canon = c.call("rdy_overview", json!({"scope": "canon"})).ok();
    assert_eq!(canon["redaction_warnings"], warnings);

    // the warning's size is reserved first and it is never cut: the least budget that keeps it
    // is its size and the fixed parts' 300 tokens
    let least = tokens(&warnings) + 300;
    let fits = c.call("rdy_overview", json!({"max_tokens": least})).ok();
    assert_eq!(
        fits["redaction_warnings"], warnings,
        "kept whole at {least}"
    );
    let (code, message, fix) = match c.call("rdy_overview", json!({"max_tokens": least - 1})) {
        Reply::ToolError(e) => {
            let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
            (s("code"), s("message"), s("fix"))
        }
        other => panic!("expected a tool error, got {other:?}"),
    };
    assert_eq!(code, "E_REDACTION_BUDGET");
    assert_eq!(
        message,
        "The output budget is too small for the redaction warning."
    );
    assert_eq!(
        fix,
        "Increase max_tokens, or run rdy fsck for the complete warning list."
    );
    c.close_stdin();
    let (exit, _) = c.wait_exit(Duration::from_secs(30));
    assert_eq!(exit, Some(0));

    // a store with no carried record has no such member, and a small budget is no error
    let mut c = McpClient::start(plain.path());
    session(&mut c);
    let o = c.call("rdy_overview", json!({"max_tokens": 50})).ok();
    assert!(o.get("redaction_warnings").is_none(), "{o}");
    c.close_stdin();
    let (exit, _) = c.wait_exit(Duration::from_secs(30));
    assert_eq!(exit, Some(0));
}
