//! hc6 (D-QF2HQ: "rdy status and rdy fsck repeat it while a span with held: true still holds its
//! payload"; D-KQ4CA: the CLI keeps parity with the tools). A store that carries a removal it
//! does not repeat warns in `rdy status` and `rdy fsck`, in text and in JSON, and the MCP
//! overview shows the same current warning. Fsck still succeeds: a warning is not damage.
//!
//! The store is built through the checked path of a stopped load: the project's store is moved
//! aside and ingested again with one carried record over the secret block's frame and the change
//! frame that wrote it, both still held.
use rdy_engine::{Rdy, Session};
use rdy_model::redaction::{
    BlockResidue, CarriedSpan, ChangeResidue, FramePlace, ReasonClass, RedactTarget,
    RedactionCarried, RedactionResidue,
};
use rdy_model::{Block, Change, Hash, Op, Provenance, Role};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const SECRET: &str = "The launch code is ZEBRA-7731.";
const FIX: &str =
    "Propose the redaction of the listed targets again, and have a person approve it.";

fn enc<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut buf = vec![0u8; 1 << 20];
    rdy_model::redaction::encode_into(value, &mut buf)
        .unwrap()
        .to_vec()
}

/// The store of the project at `dir`, moved aside and ingested again with one carried record
/// over the frames that hold `ZEBRA-7731`, all `held`. Returns the record's hash, the source
/// apply's and landing's hashes, and the target's selector.
fn carry(dir: &Path) -> (Hash, Hash, Hash, String) {
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
    let selector = format!("{id}@{version}");
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
    i.upgrade(rdy_store::format::Format::CURRENT.store_format)
        .unwrap();
    i.finish().unwrap();
    (Hash::of(&enc(&rec)), rec.apply, rec.landing, selector)
}

/// A served project that carries the removal, its daemon stopped.
fn carrying() -> (Served, Hash, Hash, Hash, String) {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
                {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET}
            ]),
        )
        .unwrap();
        let p = r
            .propose(
                &Session::human("human:qa".into(), Role::Writer),
                "work",
                "Base",
                None,
            )
            .unwrap();
        r.land(&Session::human("human:qa".into(), Role::Approver), &p.id)
            .unwrap();
        r.close().unwrap();
    }
    let (carried, apply, landing, selector) = carry(s.dir.path());
    (s, carried, apply, landing, selector)
}

fn message(apply: Hash, landing: Hash) -> String {
    format!(
        "This store stops before redaction apply {apply} of its source, approved by human:founder at landing {landing}. It holds 2 removed payloads again, and 0 stay removed."
    )
}

fn text_of(o: &std::process::Output) -> (bool, String) {
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn hc6_cli_status_and_fsck_print_the_warning_once_with_its_targets() {
    let (s, _, apply, landing, selector) = carrying();
    let msg = message(apply, landing);

    let (ok, status) = text_of(&s.rdy(&["status"]));
    assert!(ok, "{status}");
    assert_eq!(
        status
            .matches(&format!("warning: W_STOP_BEFORE_REDACTION: {msg}; {FIX}"))
            .count(),
        1,
        "{status}"
    );
    assert_eq!(status.matches(&msg).count(), 1, "printed once: {status}");
    assert!(
        status.contains(&format!("{FIX}\n  target: {selector}")),
        "the target follows its warning: {status}"
    );

    // fsck still succeeds, and prints the warning then its target
    let (ok, fsck) = text_of(&s.rdy(&["fsck"]));
    assert!(ok, "{fsck}");
    assert!(
        fsck.contains(&format!(
            "\nW_STOP_BEFORE_REDACTION: {msg}\nfix: {FIX}\n  target: {selector}"
        )),
        "{fsck}"
    );

    // and every run repeats it
    let (_, again) = text_of(&s.rdy(&["status"]));
    assert!(again.contains(&msg));
}

#[test]
fn hc6_cli_and_mcp_show_the_same_current_warning() {
    let (s, carried, apply, landing, selector) = carrying();
    let (ok, status) = s.json(&["status", "--json"]);
    assert!(ok, "{status}");
    let (ok, fsck) = s.json(&["fsck", "--json"]);
    assert!(ok, "{fsck}");
    s.stop();

    let want = json!([{
        "code": "W_STOP_BEFORE_REDACTION",
        "message": message(apply, landing),
        "fix": FIX,
        "request": Hash::of(b"source request").to_string(),
        "apply": apply.to_string(),
        "landing": landing.to_string(),
        "carried": carried.to_string(),
        "issues": [{"target": selector}],
    }]);
    // the structured warnings of status and fsck, and the string in status's list
    assert_eq!(status["redaction_warnings"], want);
    assert_eq!(fsck["warnings"], want);
    assert!(
        status["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!(format!(
                "W_STOP_BEFORE_REDACTION: {}; {FIX}",
                message(apply, landing)
            ))),
        "{status}"
    );

    let mut c = McpClient::start(s.dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "r1", "role": "reader"}),
    )
    .ok();
    let overview: Value = c.call("rdy_overview", json!({})).ok();
    assert_eq!(overview["redaction_warnings"], want);
    c.close_stdin();
    let (code, _) = c.wait_exit(Duration::from_secs(30));
    assert_eq!(code, Some(0));
}
