//! wx28a: what a failed flush, a failed rename, a read-only directory and a killed process leave
//! of a project, on the platform's own files and processes.
//!
//! A process that exits proves one thing only: what it handed to the system stays readable. That
//! is no proof of durability, because the system's cache outlives the process. So the tests
//! prove two separate things. A write is acknowledged only after its sync succeeded
//! (`wx28a_failed_sync_or_rename_keeps_acknowledged_data`), and a process terminated at a barrier
//! inside a write loses no acknowledged byte (`wx28a_windows_crash_recovery_matches_the_dump`).
//! What a power cut leaves is the hardware witness's question, not a question for these tests.
use rdy_engine::{Rdy, RdyError, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::{invariants, platform, reindex_check};
use rdy_store::pio::{self, Boundary, DurableStep, Ended};
use serde_json::json;
use std::cell::Cell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

const VIEW: &str = "work";

fn store_dir(project: &Path) -> PathBuf {
    project.join(".rdy").join("store")
}

/// Every segment file of the project, in order.
fn segment_files(project: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(store_dir(project).join("segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .collect();
    files.sort();
    files
}

/// A doc and a section in the working view; returns the section.
fn seed(r: &mut Rdy) -> String {
    let res = write(r, VIEW, json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Kept", "doc_type": "constraints"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Claims"}}
    ]))
    .unwrap();
    res.created["$s"].clone()
}

fn claim(r: &mut Rdy, section: &str, text: &str) -> Result<String, RdyError> {
    let res = write(
        r,
        VIEW,
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": section, "body": text}]),
    )?;
    Ok(res.created["$c"].clone())
}

/// What a project acknowledged: the ids it returned and the log's bytes at that moment.
struct Acked {
    ids: Vec<String>,
    log: Vec<(PathBuf, Vec<u8>)>,
}

impl Acked {
    fn of(project: &Path, ids: Vec<String>) -> Acked {
        let log = segment_files(project)
            .into_iter()
            .map(|p| {
                let bytes = std::fs::read(&p).unwrap();
                (p, bytes)
            })
            .collect();
        Acked { ids, log }
    }

    /// The bytes are in place, the project opens, every id reads back, and a reindex gives the
    /// same state. Only a tail that nobody acknowledged may differ.
    fn assert_kept(&self, project: &Path, case: &str) {
        self.assert_bytes(project, case);
        let r = Rdy::open(project).unwrap_or_else(|e| panic!("{case}: reopen: {e}"));
        let work = Scope::View(VIEW.into());
        let m = r.read().materialized(&work).unwrap().clone();
        for id in &self.ids {
            assert!(
                m.get(&BlockId::parse(id).unwrap()).is_some(),
                "{case}: lost {id}"
            );
        }
        r.fsck().unwrap_or_else(|e| panic!("{case}: fsck: {e}"));
        invariants::check(&r, &work).unwrap_or_else(|v| panic!("{case}: {v:?}"));
        drop(r);
        reindex_check::reindex_reproduces(project).unwrap_or_else(|v| panic!("{case}: {v:?}"));
        self.assert_bytes(project, case);
    }

    fn assert_bytes(&self, project: &Path, case: &str) {
        for (p, bytes) in &self.log {
            let now = std::fs::read(p).unwrap_or_else(|e| panic!("{case}: {}: {e}", p.display()));
            assert!(
                now.starts_with(bytes),
                "{case}: {} no longer starts with its {} acknowledged bytes ({} now)",
                p.display(),
                bytes.len(),
                now.len()
            );
        }
        assert!(segment_files(project).len() >= self.log.len(), "{case}");
    }
}

/// A project with acknowledged claims, ready for a fault; returns its section too.
fn project_with_acked() -> (tempfile::TempDir, Rdy, String, Acked) {
    let (d, mut r) = project();
    let section = seed(&mut r);
    let ids = (0..3)
        .map(|i| claim(&mut r, &section, &format!("Kept {i}.")).unwrap())
        .collect();
    let acked = Acked::of(d.path(), ids);
    (d, r, section, acked)
}

/// Runs `op` with `fail` as the test backend of the durable steps, and removes it again.
fn with_durable<T>(
    fail: impl FnMut(&DurableStep, Boundary) -> std::io::Result<()> + 'static,
    op: impl FnOnce() -> T,
) -> T {
    pio::on_durable(Some(Box::new(fail)));
    let out = op();
    pio::on_durable(None);
    out
}

/// A write is acknowledged only after its sync, and a failed flush or rename at any durable step
/// of a roll, a reindex or a dump fails that operation and costs no acknowledged byte.
#[test]
fn wx28a_failed_sync_or_rename_keeps_acknowledged_data() {
    // the segment's sync: the write fails, and so does every later write of the handle
    let (d, mut r, section, acked) = project_with_acked();
    r.store_mut().fail_next_sync();
    let refused = claim(&mut r, &section, "Never acknowledged.");
    assert!(refused.is_err(), "a failed sync acknowledged: {refused:?}");
    let again = claim(&mut r, &section, "Nor this one.");
    assert!(again.is_err(), "a poisoned store wrote: {again:?}");
    rdy_store::unclean_closes_for_tests(d.path(), || drop(r));
    acked.assert_kept(d.path(), "failed segment sync");

    // every durable step of each operation, each failed in its turn on either side of its call
    type Op = fn(&mut Rdy, &Path, &str) -> Result<(), RdyError>;
    let ops: [(&str, Op); 3] = [
        ("roll", |r, _, section| {
            r.store_mut().set_segment_max(1);
            claim(r, section, "Rolled.").map(|_| ())
        }),
        ("reindex", |r, _, _| r.reindex()),
        ("dump", |r, project, _| {
            r.read().dump(&project.join("out.dump")).map(|_| ())
        }),
    ];
    for (name, op) in ops {
        let mut failed_steps = 0;
        for at in [Boundary::Before, Boundary::After] {
            for nth in 0.. {
                let (d, mut r, section, acked) = project_with_acked();
                let seen = Rc::new(Cell::new(0usize));
                let fired = Rc::new(Cell::new(false));
                let (count, fire) = (seen.clone(), fired.clone());
                let result = with_durable(
                    move |_, boundary| {
                        if boundary != at {
                            return Ok(());
                        }
                        count.set(count.get() + 1);
                        if count.get() == nth + 1 {
                            fire.set(true);
                            return Err(std::io::Error::other("injected durable failure"));
                        }
                        Ok(())
                    },
                    || op(&mut r, d.path(), &section),
                );
                if !fired.get() {
                    result.unwrap_or_else(|e| panic!("{name}: no step failed: {e}"));
                    drop(r);
                    break;
                }
                failed_steps += 1;
                let case = format!("{name}: step {nth} failed {at:?} its call");
                assert!(result.is_err(), "{case}: the operation succeeded");
                rdy_store::unclean_closes_for_tests(d.path(), || drop(r));
                acked.assert_kept(d.path(), &case);
            }
        }
        assert!(failed_steps >= 2, "{name}: it took no durable step");
    }
}

/// A read-only directory is a real failure, refused by the system: a roll cannot make its
/// segment and a reindex cannot publish its index. Each fails, and no acknowledged byte moves.
#[test]
fn wx28a_a_read_only_store_fails_and_keeps_acknowledged_data() {
    type Op = fn(&mut Rdy, &str) -> Result<(), RdyError>;
    type Directory = fn(&Path) -> PathBuf;
    let cases: [(&str, Directory, Op); 2] = [
        (
            "a roll in read-only segments",
            |p| store_dir(p).join("segments"),
            |r, section| {
                r.store_mut().set_segment_max(1);
                claim(r, section, "Rolled.").map(|_| ())
            },
        ),
        ("a reindex in a read-only store", store_dir, |r, _| {
            r.reindex()
        }),
    ];
    for (case, directory, op) in cases {
        let (d, mut r, section, acked) = project_with_acked();
        let directory = directory(d.path());
        let guard = platform::ReadOnlyDir::new(&directory).unwrap();
        // an account the system lets through (root, an elevated token) has no refusal to test
        let probe = directory.join("wx28a-probe");
        let refused = std::fs::write(&probe, b"").is_err();
        let _ = std::fs::remove_file(&probe);
        let result = op(&mut r, &section);
        if refused {
            assert!(result.is_err(), "{case}: the system's refusal was lost");
        }
        rdy_store::unclean_closes_for_tests(d.path(), || drop(r));
        drop(guard);
        acked.assert_kept(d.path(), case);
    }

    // a closed store in a read-only directory: an open needs its sentinel, so it is refused
    let (d, r, _section, acked) = project_with_acked();
    r.close().unwrap();
    let store = store_dir(d.path());
    let guard = platform::ReadOnlyDir::new(&store).unwrap();
    let probe = store.join("wx28a-probe");
    let refused = std::fs::write(&probe, b"").is_err();
    let _ = std::fs::remove_file(&probe);
    if refused {
        let open = Rdy::open(d.path()).map(|_| ());
        assert!(open.is_err(), "an open with no sentinel: {open:?}");
        acked.assert_bytes(d.path(), "a refused open");
    }
    drop(guard);
    acked.assert_kept(d.path(), "after a refused open");
}

/// A child that holds a store, killed when it is dropped, whatever the test did.
struct Retained(Child);

impl Drop for Retained {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").unwrap();
    out.flush().unwrap();
}

/// The barrier: says so, then waits until the parent terminates this process. An ended parent
/// closes this stream, so a parent that failed leaves no process behind.
fn stop_at_barrier() -> ! {
    say("BARRIER");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    std::process::exit(4)
}

#[test]
#[ignore = "wx28a_windows_crash_recovery_matches_the_dump runs this in a child process"]
fn wx28a_crash_child() {
    let Ok(project) = std::env::var("RDY_WX28A_PROJECT") else {
        return;
    };
    let section = std::env::var("RDY_WX28A_SECTION").unwrap();
    let mode = std::env::var("RDY_WX28A_MODE").unwrap();
    let dump = std::env::var("RDY_WX28A_DUMP").unwrap();
    let mut r = Rdy::open(Path::new(&project)).unwrap();
    for i in 0..5 {
        let id = claim(&mut r, &section, &format!("Acknowledged {i}.")).unwrap();
        say(&format!("ACK {id}"));
    }
    // the reference: a dump of exactly what was acknowledged, made before the barrier
    r.read().dump(Path::new(&dump)).unwrap();
    let end = r.store().log_end();
    say(&format!("END {} {}", end.segment, end.offset));
    match mode.as_str() {
        // frames that reached the file, in a batch no marker closes
        "an open batch" => {
            r.store_mut().set_held(true).unwrap();
            r.store_mut().put_blob(b"never acknowledged").unwrap();
            stop_at_barrier()
        }
        // a real write, stopped at the flush of the segment it rolled to
        "a rolling write" => {
            r.store_mut().set_segment_max(1);
            pio::on_durable(Some(Box::new(|step, _| {
                if matches!(step, DurableStep::Flush(_)) {
                    stop_at_barrier()
                }
                Ok(())
            })));
            let _ = claim(&mut r, &section, "Never acknowledged.");
            say("NO BARRIER");
            std::process::exit(3)
        }
        other => panic!("unknown mode {other}"),
    }
}

/// A process terminated at a barrier inside a write loses no acknowledged byte: the reopened
/// project passes every check, its dump is byte for byte the dump the process made of what it
/// had acknowledged, and a load of that dump and a reindex of either project give it again.
#[test]
fn wx28a_windows_crash_recovery_matches_the_dump() {
    for mode in ["an open batch", "a rolling write"] {
        let base = tempfile::tempdir().unwrap();
        let project = base.path().join("p");
        std::fs::create_dir(&project).unwrap();
        let mut r = Rdy::init(&project, "rdy-standard").unwrap();
        let section = seed(&mut r);
        r.close().unwrap();
        let reference = base.path().join("before.dump");
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                &rdy_qa::test_name!("wx28a_crash_child"),
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env("RDY_WX28A_PROJECT", &project)
            .env("RDY_WX28A_SECTION", &section)
            .env("RDY_WX28A_MODE", mode)
            .env("RDY_WX28A_DUMP", &reference)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = Retained(child);
        let mut lines = BufReader::new(child.0.stdout.take().unwrap()).lines();
        let mut ids = vec![];
        let mut end: Option<(usize, u64)> = None;
        loop {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("{mode}: the child stopped before its barrier"))
                .unwrap();
            if let Some(i) = line.find("ACK ") {
                ids.push(line[i + 4..].to_string());
            } else if let Some(rest) = line.strip_prefix("END ") {
                let (segment, offset) = rest.split_once(' ').unwrap();
                end = Some((segment.parse().unwrap(), offset.parse().unwrap()));
            } else if line == "BARRIER" {
                break;
            }
            assert_ne!(line, "NO BARRIER", "{mode}");
        }
        assert_eq!(ids.len(), 5, "{mode}");
        let (segment, offset) = end.expect("the child named the log's end");

        // what is acknowledged, read while the child still holds the store
        let mut log: Vec<(PathBuf, Vec<u8>)> = vec![];
        for (n, path) in segment_files(&project).into_iter().enumerate() {
            let mut bytes = std::fs::read(&path).unwrap();
            if n == segment {
                if mode == "an open batch" {
                    assert!(
                        bytes.len() as u64 > offset,
                        "{mode}: no unacknowledged frame reached the file"
                    );
                }
                bytes.truncate(offset as usize);
            }
            if n <= segment {
                log.push((path, bytes));
            }
        }
        let acked = Acked { ids, log };

        // the barrier is real: the child holds the store now
        let held = Rdy::open(&project).map(|_| ());
        assert!(held.is_err(), "{mode}: the child does not hold the store");

        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        assert_eq!(pio::ended(&status), Ended::Killed, "{mode}: {status:?}");
        assert!(
            !lines
                .map_while(Result::ok)
                .any(|l| l.contains("ACK ") || l.contains("NO BARRIER")),
            "{mode}: the child went on past its barrier"
        );

        let r = Rdy::open(&project).unwrap_or_else(|e| panic!("{mode}: reopen: {e}"));
        let report = r.store().open_report().clone();
        assert!(report.unclean_shutdown, "{mode}: {report:?}");
        assert!(!report.index_rebuilt, "{mode}: {report:?}");
        let after = base.path().join("after.dump");
        r.read().dump(&after).unwrap();
        r.close().unwrap();
        let reference = std::fs::read(&reference).unwrap();
        assert_eq!(
            reference,
            std::fs::read(&after).unwrap(),
            "{mode}: the recovered dump differs from the acknowledged dump"
        );
        acked.assert_kept(&project, mode);

        // a reindex and a load give the same dump again
        let again = base.path().join("reindexed.dump");
        let r = Rdy::open(&project).unwrap();
        r.read().dump(&again).unwrap();
        r.close().unwrap();
        assert_eq!(reference, std::fs::read(&again).unwrap(), "{mode}: reindex");
        let loaded = base.path().join("loaded");
        std::fs::create_dir(&loaded).unwrap();
        rdy_engine::load::load(&after, &loaded).unwrap_or_else(|e| panic!("{mode}: load: {e}"));
        reindex_check::reindex_reproduces(&loaded).unwrap_or_else(|v| panic!("{mode}: {v:?}"));
        let of_loaded = base.path().join("loaded.dump");
        let r = Rdy::open(&loaded).unwrap();
        r.read().dump(&of_loaded).unwrap();
        r.close().unwrap();
        assert_eq!(
            reference,
            std::fs::read(&of_loaded).unwrap(),
            "{mode}: load"
        );
    }
}
