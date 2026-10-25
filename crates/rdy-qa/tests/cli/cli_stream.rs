//! ha15: `rdy stream check <file|-> [--repo-id <id>]`, the CLI twin of `rdy_stream_check`
//! (D-D2NPN, D-KQ4CA, D-WDKWQ; hub i-VZ93S, r-E0RJX). It checks one upload batch without opening
//! a store, so it runs before project discovery, config reads and any daemon connection.
use rdy_engine::Rdy;
use rdy_engine::stream_check::check_batch;
use rdy_model::{BlockId, Hash, RepoId, StateSnapshot};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{output_within_stdin, rdy_bin};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use rdy_store::closure::{self, END_OF_LOG, StoreSource};
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, refs_of};
use rdy_store::sync_stream::{StreamHeader, StreamRecord, StreamWriter};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

/// A hang guard, not a speed check: a child still running at this budget is killed and reaped,
/// and the test fails.
const HANG_BUDGET: Duration = Duration::from_secs(120);

/// A source project's batches: the whole closure of its tag, and the leaves alone.
struct Batches {
    repo: RepoId,
    full: Vec<u8>,
    full_keys: Vec<ObjectKey>,
    /// Records that need nothing, with the tag as the one prerequisite: a batch before its root.
    early: Vec<u8>,
    early_keys: Vec<ObjectKey>,
}

fn batches() -> Batches {
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
    let early_keys: Vec<ObjectKey> = c
        .records
        .iter()
        .copied()
        .filter(|k| {
            {
                // the frame's raw kind and stored bytes: a change is encoding 1
                let (raw, payload) = r.store().get_kind_raw(k.kind, &k.hash).unwrap();
                refs_of(raw, &payload).unwrap().is_empty()
            }
        })
        .collect();
    assert!(early_keys.iter().any(|k| k.kind == kinds::BLOCK));
    let root = ObjectKey {
        kind: kinds::TAG,
        hash: tag,
    };
    assert!(!early_keys.contains(&root));
    let early = write(
        &StreamHeader::tag_history(
            rdy_store::format::baseline_features(),
            repo,
            tag,
            vec![root],
        ),
        &early_keys,
    );
    Batches {
        repo,
        full,
        full_keys: c.records,
        early,
        early_keys,
    }
}

/// A scratch directory that is no project, with a home of its own so no user config applies.
struct Scratch {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Scratch {
        Scratch {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn put(&self, name: &str, bytes: &[u8]) -> String {
        std::fs::write(self.dir.path().join(name), bytes).unwrap();
        name.to_string()
    }

    /// `rdy <args>` in the scratch directory, with `stdin` fed to it. The child is supervised:
    /// stdin is fed and the output drained on threads, and a child still running at the budget
    /// is killed and reaped.
    fn rdy(&self, args: &[&str], stdin: Option<&[u8]>) -> Output {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(args)
            .current_dir(self.dir.path())
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path());
        output_within_stdin(&mut cmd, stdin, HANG_BUDGET).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Every path under the scratch directory.
    fn listing(&self) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                out.push(p.strip_prefix(base).unwrap().display().to_string());
                if p.is_dir() {
                    walk(&p, base, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self.dir.path(), self.dir.path(), &mut out);
        out.sort();
        out
    }
}

fn out_text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err_text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The error envelope of a failed `--json` command.
fn envelope(o: &Output) -> Value {
    assert!(!o.status.success(), "{}", out_text(o));
    assert!(
        out_text(o).is_empty(),
        "no report on stdout: {}",
        out_text(o)
    );
    serde_json::from_str(err_text(o).trim()).unwrap()
}

#[test]
fn ha15_cli_check_runs_outside_a_project_with_invalid_config() {
    let b = batches();
    let s = Scratch::new();
    let file = s.put("b.rdybatch", &b.full);
    // a config that does not parse, in the directory and in the user config: a command that
    // reads config would say so; the check reads none and starts no daemon
    s.put(".rdy.yaml", b"serve: [unclosed\n");
    std::fs::create_dir_all(s.home.path().join("rdy")).unwrap();
    std::fs::write(s.home.path().join("rdy/config.yaml"), b"human: [unclosed\n").unwrap();
    let before = s.listing();

    let probe = s.rdy(&["config"], None);
    assert!(
        err_text(&probe).contains("E_CONFIG"),
        "the config is invalid, as the test needs: {} {}",
        out_text(&probe),
        err_text(&probe)
    );

    let o = s.rdy(&["stream", "check", &file], None);
    assert!(o.status.success(), "{}", err_text(&o));
    assert_eq!(
        out_text(&o).trim(),
        format!(
            "checked {} records and {} bytes",
            b.full_keys.len(),
            b.full.len() - 85
        )
    );
    assert!(err_text(&o).is_empty(), "no warning: {}", err_text(&o));
    assert_eq!(s.listing(), before, "no file, store or run directory");

    // --json prints the report alone, and no progress text goes to stdout
    let o = s.rdy(&["--json", "stream", "check", &file], None);
    assert!(o.status.success(), "{}", err_text(&o));
    assert!(err_text(&o).is_empty(), "{}", err_text(&o));
    let v: Value = serde_json::from_str(out_text(&o).trim()).unwrap();
    let report = check_batch(b.full.as_slice(), None).unwrap();
    assert_eq!(v, serde_json::to_value(&report).unwrap());
    assert_eq!(v["header"]["repo_id"], b.repo.to_string());
    assert_eq!(v["keys"][0]["kind"], b.full_keys[0].kind);
    assert_eq!(v["keys"][0]["hash"], b.full_keys[0].hash.to_string());
    assert_eq!(v["digest"].as_str().unwrap().len(), 64);
}

#[test]
fn ha15_cli_check_reads_stdin_and_reports_record_offsets() {
    let b = batches();
    let s = Scratch::new();
    let trailer_at = b.full.len() - 85;
    let n = b.full_keys.len();

    // `-` is standard input
    let o = s.rdy(&["--json", "stream", "check", "-"], Some(&b.full));
    assert!(o.status.success(), "{}", err_text(&o));
    let v: Value = serde_json::from_str(out_text(&o).trim()).unwrap();
    assert_eq!(v["records"], n);
    assert_eq!(
        v["keys"].as_array().unwrap().len(),
        n,
        "every key, in wire order"
    );
    // and a file of that name is not read in its place
    s.put("-", b"not a batch");
    let o = s.rdy(&["--json", "stream", "check", "-"], Some(&b.full));
    assert!(o.status.success(), "{}", err_text(&o));

    // the last record's payload changed: its hash no longer matches, and nothing is reported
    let mut bad = b.full.clone();
    bad[trailer_at - 1] ^= 1;
    let o = s.rdy(&["--json", "stream", "check", "-"], Some(&bad));
    let e = envelope(&o);
    assert_eq!(e["code"], "E_BAD_RECORD", "{e}");
    assert_eq!(
        e["fix"],
        "request this stream again; run rdy fsck at its source"
    );
    let at = e["issues"][0]["message"].as_str().unwrap();
    assert!(
        at.starts_with(&format!("record {n} at byte ")) && at.contains("; object "),
        "{at}"
    );
    let text = s.rdy(&["stream", "check", "-"], Some(&bad));
    assert!(!text.status.success() && out_text(&text).is_empty());
    assert!(
        err_text(&text).starts_with("error E_BAD_RECORD: "),
        "{}",
        err_text(&text)
    );

    // the trailer cut off
    let o = s.rdy(
        &["--json", "stream", "check", "-"],
        Some(&b.full[..trailer_at]),
    );
    let e = envelope(&o);
    assert_eq!(e["code"], "E_STREAM_TRUNCATED", "{e}");
    assert_eq!(e["fix"], "request the complete stream again");
    assert_eq!(
        e["issues"][0]["message"],
        format!("record {} at byte {trailer_at}; object none", n + 1)
    );

    // a file that is not there is an I/O error naming it
    let o = s.rdy(&["--json", "stream", "check", "missing.rdybatch"], None);
    let e = envelope(&o);
    assert_eq!(e["code"], "E_IO", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("missing.rdybatch"),
        "{e}"
    );

    // a repo id of the wrong shape, and one of another repo
    let o = s.rdy(
        &["--json", "stream", "check", "-", "--repo-id", "x"],
        Some(&b.full),
    );
    let e = envelope(&o);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(e["message"], "bad repo id \"x\": expected 32 hex digits");
    assert_eq!(e["fix"], "pass a repo id with 32 hex digits");
    let other = RepoId([0x42; 16]).to_string();
    let o = s.rdy(
        &["--json", "stream", "check", "-", "--repo-id", &other],
        Some(&b.full),
    );
    let e = envelope(&o);
    assert_eq!(e["code"], "E_REPO_MISMATCH", "{e}");
    assert_eq!(e["fix"], "use the holder for the stream's repo");
    let same = b.repo.to_string().to_uppercase();
    let o = s.rdy(
        &["--json", "stream", "check", "-", "--repo-id", &same],
        Some(&b.full),
    );
    assert!(o.status.success(), "{}", err_text(&o));
}

#[test]
fn ha15_early_batch_without_root_passes_all_surfaces() {
    let b = batches();
    let s = Scratch::new();
    let o = s.rdy(&["--json", "stream", "check", "-"], Some(&b.early));
    assert!(o.status.success(), "{}", err_text(&o));
    let v: Value = serde_json::from_str(out_text(&o).trim()).unwrap();
    assert_eq!(v["keys"].as_array().unwrap().len(), b.early_keys.len());
    assert_eq!(v["header"]["prerequisites"].as_array().unwrap().len(), 1);
    assert_eq!(v["header"]["prerequisites"][0]["kind"], kinds::TAG);
    let core = check_batch(b.early.as_slice(), Some(b.repo)).unwrap();
    assert_eq!(core.keys, b.early_keys);
    assert!(!core.keys.iter().any(|k| k.kind == kinds::TAG));
}

#[test]
fn ha15_cli_check_does_not_create_rdy_or_an_index() {
    let b = batches();
    let s = Scratch::new();
    let file = s.put("b.rdybatch", &b.full);
    let before = s.listing();
    for args in [
        vec!["stream", "check", file.as_str()],
        vec!["--json", "stream", "check", file.as_str()],
    ] {
        let o = s.rdy(&args, None);
        assert!(o.status.success(), "{}", err_text(&o));
    }
    let mut bad = b.full.clone();
    bad.truncate(b.full.len() - 1);
    s.put("bad.rdybatch", &bad);
    let o = s.rdy(&["stream", "check", "bad.rdybatch"], None);
    assert!(!o.status.success());
    let mut expect = before;
    expect.push("bad.rdybatch".to_string());
    expect.sort();
    assert_eq!(
        s.listing(),
        expect,
        "no .rdy, store, index or run directory"
    );
    assert!(!s.dir.path().join(".rdy").exists());
}

#[test]
fn ha15_a_child_that_stops_reading_is_killed_at_its_budget_not_waited_on() {
    // more than a pipe holds, to a command that never reads: the feed would block a plain write
    let mut cmd = Command::new("sleep");
    cmd.arg("600");
    let r = output_within_stdin(
        &mut cmd,
        Some(&vec![0u8; 4 << 20]),
        Duration::from_millis(200),
    );
    assert!(
        r.is_err(),
        "a child past its budget is an error, not a wait: {r:?}"
    );
}

/// A batch of one snapshot record whose block and root TAG are both absent: it lists the two as
/// its prerequisites, as the store tests encode records.
fn two_prerequisite_batch() -> Vec<u8> {
    let snapshot = StateSnapshot {
        blocks: BTreeMap::from([(BlockId::parse("b-00001").unwrap(), Hash([0x31; 32]))]),
    };
    let payload = postcard::to_allocvec(&snapshot).unwrap();
    let record = StreamRecord::Present {
        encoding: 0,
        key: ObjectKey {
            kind: kinds::SNAPSHOT,
            hash: Hash::of(&payload),
        },
        payload,
    };
    let root = Hash([0x41; 32]);
    let prerequisites = vec![
        ObjectKey {
            kind: kinds::BLOCK,
            hash: Hash([0x31; 32]),
        },
        ObjectKey {
            kind: kinds::TAG,
            hash: root,
        },
    ];
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        RepoId([7; 16]),
        root,
        prerequisites,
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    w.record(&record).unwrap();
    w.finish().unwrap().0
}

#[test]
fn plb2_check_text_counts_prerequisites() {
    let b = batches();
    let two = two_prerequisite_batch();
    let s = Scratch::new();
    for (name, bytes, prerequisites, suffix) in [
        ("zero", &b.full, 0, ""),
        (
            "one",
            &b.early,
            1,
            "; it needs 1 prerequisite from other batches or the holder",
        ),
        (
            "two",
            &two,
            2,
            "; it needs 2 prerequisites from other batches or the holder",
        ),
    ] {
        // the numbers come from the engine's own check of the fixture
        let report = check_batch(bytes.as_slice(), None).unwrap();
        assert_eq!(report.header.prerequisites.len(), prerequisites, "{name}");
        assert_eq!(report.bytes, bytes.len() as u64 - 85, "{name}");
        let want = format!(
            "checked {} records and {} bytes{suffix}\n",
            report.records, report.bytes
        );
        let file = s.put(&format!("{name}.rdybatch"), bytes);
        for (args, stdin) in [
            (vec!["stream", "check", file.as_str()], None),
            (vec!["stream", "check", "-"], Some(bytes.as_slice())),
        ] {
            let o = s.rdy(&args, stdin);
            assert!(o.status.success(), "{name}: {}", err_text(&o));
            assert_eq!(out_text(&o), want, "{name} {args:?}");
            assert!(err_text(&o).is_empty(), "no warning: {}", err_text(&o));
        }
        // JSON stays the engine's report: no new field
        let o = s.rdy(&["--json", "stream", "check", "-"], Some(bytes));
        assert!(o.status.success(), "{name}: {}", err_text(&o));
        let v: Value = serde_json::from_str(out_text(&o).trim()).unwrap();
        assert_eq!(v, serde_json::to_value(&report).unwrap(), "{name}");
    }
}
