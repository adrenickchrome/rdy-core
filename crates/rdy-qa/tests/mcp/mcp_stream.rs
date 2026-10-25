//! ha15: `rdy_stream_check {file, repo_id?}` gives an agent the store-free batch check (D-D2NPN,
//! D-KQ4CA, D-WDKWQ; hub i-VZ93S, r-E0RJX). It reads one upload batch file through the session
//! project's path resolver, opens no second store, and returns the batch's keys only after the
//! whole batch passes.
use rdy_engine::Rdy;
use rdy_engine::stream_check::check_batch;
use rdy_engine::stream_error::to_rdy;
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{ERROR_ROWS, StreamCheckParams};
use rdy_model::{Answer, AnswerAct, BlockId, Hash, Provenance, RepoId, Role, StateSnapshot};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{output_within_stdin, rdy_bin};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::closure::{self, END_OF_LOG, StoreSource};
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, refs_of};
use rdy_store::sync_stream::{self, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// A hang guard, not a speed check: a child still running at this budget is killed and reaped,
/// and the test fails.
const HANG_BUDGET: Duration = Duration::from_secs(120);

/// A served project holding the golden corpus, landed and tagged `v1.0.0`, with the batches of
/// its tag history.
struct Source {
    s: Served,
    repo: RepoId,
    full: Vec<u8>,
    full_keys: Vec<ObjectKey>,
    early: Vec<u8>,
    early_keys: Vec<ObjectKey>,
}

fn source() -> Source {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap()
        .1;
    let repo = r.read().repo_id().unwrap();
    let (full, full_keys, early, early_keys) = {
        let reader = r.store().reader();
        let c = closure::closure(&reader, tag, &BTreeSet::new()).unwrap();
        let write = |header: &StreamHeader, records: &[ObjectKey]| {
            let mut source = StoreSource {
                reader: &reader,
                end: END_OF_LOG,
            };
            closure::write_batch(&mut source, header, records, Vec::new())
                .unwrap()
                .0
        };
        let full = write(
            &StreamHeader::tag_history(
                rdy_store::format::baseline_features(),
                repo,
                tag,
                c.prerequisites.clone(),
            ),
            &c.records,
        );
        let leaves: Vec<ObjectKey> = c
            .records
            .iter()
            .copied()
            .filter(|k| {
                refs_of(k.kind, &r.store().get_kind(k.kind, &k.hash).unwrap())
                    .unwrap()
                    .is_empty()
            })
            .collect();
        assert!(leaves.iter().any(|k| k.kind == kinds::BLOCK));
        let root = ObjectKey {
            kind: kinds::TAG,
            hash: tag,
        };
        assert!(!leaves.contains(&root));
        let early = write(
            &StreamHeader::tag_history(
                rdy_store::format::baseline_features(),
                repo,
                tag,
                vec![root],
            ),
            &leaves,
        );
        (full, c.records, early, leaves)
    };
    r.close().unwrap();
    Source {
        s,
        repo,
        full,
        full_keys,
        early,
        early_keys,
    }
}

fn agent(c: &mut McpClient, role: &str) {
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": role}),
    )
    .ok();
}

/// The code, message and fix of a tool error.
fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// Every path under `dir` with its bytes (a directory maps to none).
fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if p.is_dir() {
                stack.push(p);
                out.insert(rel, None);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    out
}

/// `rdy --json stream check <args>` in a scratch directory with `stdin`: whether it succeeded,
/// its stdout and its stderr. The child is supervised: stdin is fed and the output drained on
/// threads, and a child still running at the budget is killed and reaped.
fn cli(args: &[&str], stdin: &[u8]) -> (bool, String, String) {
    let scratch = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(rdy_bin());
    cmd.args(["--json", "stream", "check"])
        .args(args)
        .current_dir(scratch.path())
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .env("HOME", scratch.path())
        .env("XDG_CONFIG_HOME", scratch.path());
    let o =
        output_within_stdin(&mut cmd, Some(stdin), HANG_BUDGET).unwrap_or_else(|e| panic!("{e}"));
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn ha15_cli_mcp_and_core_return_identical_ordered_keys() {
    let src = source();
    std::fs::write(src.s.dir.path().join("b.rdybatch"), &src.full).unwrap();
    let core = serde_json::to_value(check_batch(src.full.as_slice(), None).unwrap()).unwrap();
    assert_eq!(
        core["keys"],
        serde_json::to_value(&src.full_keys).unwrap(),
        "wire order"
    );

    // the chat path in process first (a daemon would hold the store), then through the daemon,
    // at either role
    {
        let mut c = McpClient::start(src.s.dir.path());
        agent(&mut c, "reader");
        let v = c
            .call(
                "rdy_stream_check",
                json!({"file": "b.rdybatch", "repo_id": src.repo.to_string()}),
            )
            .ok();
        assert_eq!(v, core);
    }
    for role in ["reader", "writer"] {
        let mut c = McpClient::start_proxy(&src.s);
        agent(&mut c, role);
        let v = c
            .call("rdy_stream_check", json!({"file": "b.rdybatch"}))
            .ok();
        assert_eq!(v, core, "{role}");
    }

    // the fallback
    let (ok, out, err) = cli(&["-"], &src.full);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v, core);
    assert_eq!(
        v["keys"][0],
        json!({"kind": src.full_keys[0].kind, "hash": src.full_keys[0].hash.to_string()}),
        "numeric kinds and full hex hashes"
    );
}

#[test]
fn ha15_early_batch_without_root_passes_all_surfaces() {
    let src = source();
    std::fs::write(src.s.dir.path().join("early.rdybatch"), &src.early).unwrap();
    let core = check_batch(src.early.as_slice(), Some(src.repo)).unwrap();
    assert_eq!(core.keys, src.early_keys);
    assert!(
        !core.keys.iter().any(|k| k.kind == kinds::TAG),
        "the batch holds no root"
    );
    let want = serde_json::to_value(&core).unwrap();

    let mut c = McpClient::start_proxy(&src.s);
    agent(&mut c, "reader");
    let v = c
        .call("rdy_stream_check", json!({"file": "early.rdybatch"}))
        .ok();
    assert_eq!(v, want);
    assert_eq!(v["header"]["prerequisites"].as_array().unwrap().len(), 1);

    let (ok, out, err) = cli(&["-"], &src.early);
    assert!(ok, "{err}");
    assert_eq!(serde_json::from_str::<Value>(out.trim()).unwrap(), want);
}

#[test]
fn ha15_stream_params_are_strict_with_nearest_key() {
    let bad = |args: Value| {
        parse_params::<StreamCheckParams>("rdy_stream_check", args.as_object().unwrap().clone())
            .expect_err("a refusal")
    };
    let e = bad(json!({"fil": "b"}));
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("fil"), "{}", e.message);
    assert!(e.fix.contains("did you mean \"file\""), "{}", e.fix);
    let e = bad(json!({"file": "b", "repo": "x"}));
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.fix.contains("did you mean \"repo_id\""), "{}", e.fix);
    for args in [
        json!({}),
        json!({"file": 3}),
        json!({"file": "b", "repo_id": 3}),
        json!({"file": "b", "path": "b"}),
    ] {
        assert_eq!(bad(args.clone()).code, "E_BAD_INPUT", "{args}");
    }
    let ok = parse_params::<StreamCheckParams>(
        "rdy_stream_check",
        json!({"file": "b", "repo_id": "00000000000000000000000000000000"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(ok.is_ok(), "{ok:?}");

    // over the wire: a session is required, a typo reads nothing, and the repo id is parsed as
    // every repo id is
    let src = source();
    std::fs::write(src.s.dir.path().join("b.rdybatch"), &src.full).unwrap();
    let mut c = McpClient::start_proxy(&src.s);
    let (code, ..) = refused(c.call("rdy_stream_check", json!({"file": "b.rdybatch"})));
    assert_eq!(code, "E_NO_SESSION");
    agent(&mut c, "reader");
    let (code, _, fix) = refused(c.call("rdy_stream_check", json!({"fil": "b.rdybatch"})));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(fix.contains("did you mean \"file\""), "{fix}");
    let (code, message, fix) = refused(c.call(
        "rdy_stream_check",
        json!({"file": "b.rdybatch", "repo_id": "x"}),
    ));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(message, "bad repo id \"x\": expected 32 hex digits");
    assert_eq!(fix, "pass a repo id with 32 hex digits");
    let other = RepoId([0x42; 16]).to_string();
    let (code, _, fix) = refused(c.call(
        "rdy_stream_check",
        json!({"file": "b.rdybatch", "repo_id": other}),
    ));
    assert_eq!(code, "E_REPO_MISMATCH");
    assert_eq!(fix, "use the holder for the stream's repo");

    // `-` is an ordinary file name here, and a missing file is an I/O error naming it
    let (code, message, _) = refused(c.call("rdy_stream_check", json!({"file": "-"})));
    assert_eq!(code, "E_IO");
    assert!(message.contains('-'), "{message}");
    std::fs::write(src.s.dir.path().join("-"), &src.full).unwrap();
    c.call("rdy_stream_check", json!({"file": "-"})).ok();

    // the tool says what it does, exactly
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_stream_check")
        .expect("rdy_stream_check is listed");
    assert_eq!(
        t["description"],
        "Check one upload batch without opening a store. Return its record keys in order. Complete upload checks run when all batches are taken."
    );
}

#[test]
fn ha15_check_does_not_create_rdy_or_an_index() {
    let src = source();
    std::fs::write(src.s.dir.path().join("b.rdybatch"), &src.full).unwrap();
    let mut bad = src.full.clone();
    bad.truncate(bad.len() - 85);
    std::fs::write(src.s.dir.path().join("bad.rdybatch"), &bad).unwrap();
    let mut c = McpClient::start(src.s.dir.path());
    agent(&mut c, "reader");
    // one read first, so what the server writes for its own session is already there
    c.call("rdy_overview", json!({})).ok();
    let before = tree(src.s.dir.path());
    c.call("rdy_stream_check", json!({"file": "b.rdybatch"}))
        .ok();
    let (code, ..) = refused(c.call("rdy_stream_check", json!({"file": "bad.rdybatch"})));
    assert_eq!(code, "E_STREAM_TRUNCATED");
    assert_eq!(
        tree(src.s.dir.path()),
        before,
        "the check wrote no file, record, index or marker"
    );
}

#[test]
fn ha15_a_pending_pack_notice_stays_outside_the_report() {
    const PACK_A: &str = "name: proj\nversion: '1'\nextends: rdy-standard\n";
    const PACK_BAD: &str = "name: proj\nversion: '1'\nextends: no-such-pack\n";
    let src = source();
    let dir = src.s.dir.path();
    std::fs::write(dir.join("b.rdybatch"), &src.full).unwrap();
    let core = serde_json::to_value(check_batch(src.full.as_slice(), None).unwrap()).unwrap();
    let packs = dir.join(".rdy/packs");
    std::fs::create_dir_all(&packs).unwrap();
    let config = std::fs::read_to_string(dir.join(".rdy.yaml")).unwrap_or_default();
    let config: String = config
        .lines()
        .filter(|l| !l.starts_with("pack:"))
        .map(|l| format!("{l}\n"))
        .collect();
    let mut c = McpClient::start_proxy(&src.s);
    agent(&mut c, "reader");
    let check = json!({"file": "b.rdybatch"});
    assert_eq!(c.call("rdy_stream_check", check.clone()).ok(), core);

    // a pack edit followed by a write: the pack reloads and this session has not heard of it
    std::fs::write(packs.join("proj.yaml"), PACK_A).unwrap();
    std::fs::write(dir.join(".rdy.yaml"), format!("pack: proj\n{config}")).unwrap();
    let out = src.s.rdy(&["new", "doc", "fresh", "--title", "Fresh"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = c.call("rdy_stream_check", check.clone()).ok();
    assert!(
        v.get("notice").is_none(),
        "the notice is not a report key: {v}"
    );
    assert_eq!(v, core, "exactly the core report");
    // the notice is deferred, not lost: the next tool reply carries it
    let next = c.call("rdy_overview", json!({})).ok();
    assert!(
        next["notice"]
            .as_str()
            .is_some_and(|n| n.starts_with("pack_changed")),
        "{next}"
    );

    // a standing pack error is repeated on every reply, and still stays out of the report
    std::fs::write(packs.join("proj.yaml"), PACK_BAD).unwrap();
    let out = src.s.rdy(&["new", "doc", "again", "--title", "Again"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for _ in 0..2 {
        let v = c.call("rdy_stream_check", check.clone()).ok();
        assert_eq!(v, core, "a standing pack error stays out of the report");
    }
}

#[test]
fn ha15_hostile_batch_refuses_and_mcp_remains_alive() {
    let src = source();
    let dir = src.s.dir.path();
    // a valid prelude and header, then a record frame that claims more than any record may hold
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        RepoId([7; 16]),
        Hash([0x11; 32]),
        vec![ObjectKey {
            kind: kinds::TAG,
            hash: Hash([0x11; 32]),
        }],
    );
    let head = {
        let b = StreamWriter::new(Vec::new(), &header)
            .unwrap()
            .finish()
            .unwrap()
            .0;
        b[..b.len() - 85].to_vec()
    };
    let frame = |len: u32, kind: u8| {
        let mut f = len.to_le_bytes().to_vec();
        f.push(kind);
        f.extend_from_slice(&[0u8; 32]);
        f
    };
    let mut over_record = head.clone();
    over_record.extend(frame(16_777_217, kinds::BLOCK));
    let mut over_stream = head.clone();
    over_stream.extend(frame(u32::MAX, kinds::BLOCK));
    let mut over_header = b"RDYSYNC\n".to_vec();
    over_header.extend_from_slice(&2u32.to_le_bytes());
    over_header.extend(frame(u32::MAX, 0xF0));
    // the first valid bytes of a real batch, then nothing: the input just ends
    let cut = src.full[..src.full.len() / 2].to_vec();

    std::fs::write(dir.join("ok.rdybatch"), &src.full).unwrap();
    let mut c = McpClient::start_proxy(&src.s);
    agent(&mut c, "reader");
    for (name, bytes, code) in [
        ("record", &over_record, "E_LIMIT"),
        ("stream", &over_stream, "E_LIMIT"),
        ("header", &over_header, "E_LIMIT"),
        ("cut", &cut, "E_STREAM_TRUNCATED"),
    ] {
        std::fs::write(dir.join(format!("{name}.rdybatch")), bytes).unwrap();
        let (got, message, _) = refused(c.call(
            "rdy_stream_check",
            json!({"file": format!("{name}.rdybatch")}),
        ));
        assert_eq!(got, code, "{name}: {message}");
        // the refusal leaves the server answering, and a valid batch still passes
        let v = c
            .call("rdy_stream_check", json!({"file": "ok.rdybatch"}))
            .ok();
        assert_eq!(v["records"], src.full_keys.len(), "{name}");
        c.alive();

        // the same batch on the CLI: a supervised child that exits with the same refusal and
        // prints no report
        let (ok, out, err) = cli(&["-"], bytes);
        assert!(!ok && out.is_empty(), "{name}: {out}");
        let e: Value = serde_json::from_str(err.trim()).unwrap();
        assert_eq!(e["code"], code, "{name}: {e}");
    }
}

// ---------------------------------------------------------------------------------------------
// plb2: the batch dependency faults, named on every surface

const ROOT: Hash = Hash([0x41; 32]);

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

/// A present record of `kind` holding `value`, encoded as the store tests encode it.
fn present<T: serde::Serialize>(kind: u8, value: &T) -> StreamRecord {
    let payload = postcard::to_allocvec(value).unwrap();
    StreamRecord::Present {
        encoding: 0,
        key: key(kind, Hash::of(&payload)),
        payload,
    }
}

/// `records` as one batch rooted at [`ROOT`] that lists `prerequisites` (sorted here).
fn plb2_batch(mut prerequisites: Vec<ObjectKey>, records: &[StreamRecord]) -> Vec<u8> {
    prerequisites.sort();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        RepoId([7; 16]),
        ROOT,
        prerequisites,
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    for r in records {
        w.record(r).unwrap();
    }
    w.finish().unwrap().0
}

/// One fault case: its name, the records, the prerequisites that miss the dependency, the
/// exact prerequisites, and the expected message and issue.
struct Case {
    name: &'static str,
    records: Vec<StreamRecord>,
    faulty: Vec<ObjectKey>,
    exact: Vec<ObjectKey>,
    message: String,
    issue: String,
}

/// The frame offset of the first record after a header with `k` prerequisites and the baseline
/// manifest (183 bytes, ok4).
fn first_at(k: u64) -> u64 {
    12 + 37 + 57 + 183 + 33 * k
}

fn plb2_cases() -> Vec<Case> {
    let root = key(kinds::TAG, ROOT);
    let block = key(kinds::BLOCK, Hash([0x31; 32]));
    let snapshot = present(
        kinds::SNAPSHOT,
        &StateSnapshot {
            blocks: BTreeMap::from([(BlockId::parse("b-00001").unwrap(), block.hash)]),
        },
    );
    let answer = present(
        kinds::ANSWER,
        &Answer {
            item: "P-x@1".into(),
            act: AnswerAct::Confirm,
            option: None,
            label: None,
            options: vec![],
            card_hash: None,
            targets: vec![Hash([0x5a; 32])],
            person: Provenance {
                actor: "agent:w".into(),
                run: None,
                role: Role::Writer,
                at_ms: 1,
            },
            relayed_by: None,
            relay_evidence: None,
            evidence_kind: None,
            note: None,
        },
    );
    let apply = key(kinds::REDACTION_APPLY, Hash([0x16; 32]));
    let residue = key(kinds::REDACTION_RESIDUE, Hash([0x15; 32]));
    let dropped_block = key(kinds::BLOCK, Hash([0x32; 32]));
    let with_residue = StreamRecord::Dropped {
        encoding: 0,
        key: dropped_block,
        original_len: 40,
        apply: apply.hash,
        residue: Some(residue.hash),
    };
    let chunk = key(kinds::BLOB_CHUNK, Hash([0x06; 32]));
    let dropped_chunk = StreamRecord::Dropped {
        encoding: 0,
        key: chunk,
        original_len: 5,
        apply: apply.hash,
        residue: None,
    };
    let extra = key(kinds::LANDING, Hash([0x30; 32]));
    let at = |k: u64, n: u64, object: &ObjectKey| {
        format!("record {n} at byte {}; object {object}", first_at(k))
    };
    vec![
        Case {
            name: "typed",
            message: format!(
                "stream record 1 at byte {}: unlisted dependency 1:{}",
                first_at(1),
                "31".repeat(32)
            ),
            issue: at(1, 1, &snapshot.key()),
            records: vec![snapshot.clone()],
            faulty: vec![root],
            exact: vec![block, root],
        },
        Case {
            name: "untyped",
            message: format!(
                "stream record 1 at byte {}: unlisted dependency {}",
                first_at(1),
                "5a".repeat(32)
            ),
            issue: at(1, 1, &answer.key()),
            records: vec![answer.clone()],
            faulty: vec![root],
            exact: vec![key(kinds::BLOCK, Hash([0x5a; 32])), root],
        },
        Case {
            name: "residue",
            message: format!(
                "stream record 1 at byte {}: unlisted dependency 21:{}",
                first_at(2),
                "15".repeat(32)
            ),
            issue: at(2, 1, &dropped_block),
            records: vec![with_residue],
            faulty: vec![root, apply],
            exact: vec![root, residue, apply],
        },
        Case {
            name: "apply",
            message: format!(
                "stream record 1 at byte {}: unlisted dependency 22:{}",
                first_at(1),
                "16".repeat(32)
            ),
            issue: at(1, 1, &chunk),
            records: vec![dropped_chunk],
            faulty: vec![root],
            exact: vec![root, apply],
        },
        Case {
            name: "root",
            message:
                "stream record 0 at byte 12: root tag is not in the batch or its prerequisites"
                    .into(),
            issue: "record 0 at byte 12; object none".into(),
            records: vec![],
            faulty: vec![],
            exact: vec![root],
        },
        Case {
            name: "unneeded",
            message: format!(
                "stream record 0 at byte 12: unneeded prerequisite 4:{}",
                "30".repeat(32)
            ),
            issue: "record 0 at byte 12; object none".into(),
            records: vec![],
            faulty: vec![extra, root],
            exact: vec![root],
        },
    ]
}

/// The code, message, fix and positioned issues of an error's JSON.
fn surface(e: &Value) -> Value {
    json!({
        "code": e["code"],
        "message": e["message"],
        "fix": e["fix"],
        "issues": e["issues"],
    })
}

#[test]
fn plb2_batch_faults_match_mcp_and_cli() {
    let s = Served::new();
    let mut c = McpClient::start(s.dir.path());
    agent(&mut c, "reader");
    for case in plb2_cases() {
        let name = case.name;
        let bytes = plb2_batch(case.faulty, &case.records);
        // the store's error, as the engine maps it
        let store = sync_stream::check_batch(bytes.as_slice()).unwrap_err();
        let core = serde_json::to_value(to_rdy(store)).unwrap();
        let engine =
            serde_json::to_value(check_batch(bytes.as_slice(), None).unwrap_err()).unwrap();
        assert_eq!(surface(&engine), surface(&core), "{name}");
        assert_eq!(core["code"], "E_BAD_RECORD", "{name}");
        assert_eq!(core["message"], case.message, "{name}");
        assert_eq!(
            core["fix"], "request this stream again; run rdy fsck at its source",
            "{name}"
        );
        assert_eq!(
            core["issues"],
            json!([{
                "severity": "error",
                "code": "E_BAD_RECORD",
                "block": null,
                "message": case.issue,
                "fix": "request this stream again; run rdy fsck at its source",
            }]),
            "{name}"
        );

        // the chat path: the tool error itself, issues and all
        let file = format!("{name}.rdybatch");
        std::fs::write(s.dir.path().join(&file), &bytes).unwrap();
        let Reply::ToolError(mcp) = c.call("rdy_stream_check", json!({"file": file})) else {
            panic!("{name}: expected a tool error")
        };
        assert_eq!(surface(&mcp), surface(&core), "{name}");

        // the fallback
        let (ok, out, err) = cli(&["-"], &bytes);
        assert!(!ok && out.is_empty(), "{name}: {out}");
        let cli_error: Value = serde_json::from_str(err.trim()).unwrap();
        assert_eq!(surface(&cli_error), surface(&core), "{name}");
    }
}

#[test]
fn plb2_listed_dependencies_still_pass_the_stateless_check() {
    let s = Served::new();
    let mut c = McpClient::start(s.dir.path());
    agent(&mut c, "reader");
    for case in plb2_cases() {
        let name = case.name;
        let mut exact = case.exact.clone();
        exact.sort();
        let bytes = plb2_batch(case.exact, &case.records);
        let core = check_batch(bytes.as_slice(), None).unwrap();
        assert_eq!(core.header.prerequisites, exact, "{name}");
        assert_eq!(
            core.keys,
            case.records
                .iter()
                .map(StreamRecord::key)
                .collect::<Vec<_>>(),
            "{name}"
        );
        let want = serde_json::to_value(&core).unwrap();

        let file = format!("{name}.rdybatch");
        std::fs::write(s.dir.path().join(&file), &bytes).unwrap();
        let v = c.call("rdy_stream_check", json!({"file": file})).ok();
        assert_eq!(v, want, "{name}");
        assert_eq!(
            v["header"]["prerequisites"],
            serde_json::to_value(&exact).unwrap(),
            "{name}"
        );

        let (ok, out, err) = cli(&["-"], &bytes);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            serde_json::from_str::<Value>(out.trim()).unwrap(),
            want,
            "{name}"
        );
    }
}

#[test]
fn plb2_schema_names_batch_dependency_faults() {
    const WHEN: &str = "a sync batch has an invalid object, frame or header, an unlisted dependency, an unneeded prerequisite, a root tag that is not in the batch or its prerequisites, a dependency cycle or a repeated object";
    const FIX: &str = "request this stream again; run rdy fsck at its source";
    let rows: Vec<_> = ERROR_ROWS
        .iter()
        .filter(|(code, _, _)| *code == "E_BAD_RECORD")
        .collect();
    assert_eq!(rows, vec![&("E_BAD_RECORD", WHEN, FIX)]);

    let s = Served::new();
    let mut c = McpClient::start(s.dir.path());
    agent(&mut c, "reader");
    let schema = c.call("rdy_schema", json!({})).ok();
    let errors = schema["errors"].as_object().unwrap();
    assert_eq!(
        errors.keys().filter(|k| *k == "E_BAD_RECORD").count(),
        1,
        "one E_BAD_RECORD row"
    );
    assert_eq!(errors["E_BAD_RECORD"], json!({"when": WHEN, "fix": FIX}));
}
