//! The two sync perf rows, and the generator of their input.
//!
//! D-3PNRK asks for a 500 MB push in at most one minute, from start to end on production. The
//! rows here run on a loopback hub ([`crate::sync_hub`] in its perf mode), so they are reported
//! measurements, not that target:
//!
//! - `sync_push_500mb`: the real CLI pushes the canon of the frozen full dump to an empty hub
//!   repo (zstd put bodies, the known, put and write routes);
//! - `sync_pull_full_history_500mb`: the real CLI clones the same canon from the hub by URL into
//!   an absent directory: no base, no held history, every reachable object fetched, then fsck.
//!
//! The input is one frozen dump of a generated store ([`write_inputs`]). The generator uses base
//! APIs only, so its files can be carried into an older build that writes the same history.
//! Perf rows never regenerate it: [`measure`] checks the file against its frozen digest.
use crate::fixtures::{approver, writer};
use crate::perf::Measure;
use crate::sync_hub::{Capture, Home, Hub, PerfStats, REMOTE_PATH, command, command_in};
use rdy_engine::plan::{ItemState, PlanAnswerInput, PlanRow};
use rdy_engine::{Rdy, Scope, pin_clock};
use rdy_model::Hash;
use rdy_model::plan::{PlanAct, PlanKind};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::StreamPurpose;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------------------------

/// The seed of the generator's ids ("syncperf").
const SEED: u64 = 0x7379_6e63_7065_7266;
/// The pinned clock of the generator, in milliseconds.
const CLOCK_MS: i64 = 1_800_000_000_000;
/// Claims each repeated landing updates, and the dependents of the first one.
pub const EDITED: usize = 20;
pub const DEPENDENTS: usize = 10;
/// The exact size of each edited claim's body.
pub const BODY_BYTES: usize = 2_048;
/// The exact size of each answer's reason.
pub const REASON_BYTES: usize = 128;
/// The log size a frozen input must reach, and the most it may reach.
pub const LOG_LOW: u64 = 500_000_000;
pub const LOG_HIGH: u64 = 516_777_216;
/// The repeated landings the generator may make before it refuses.
pub const MAX_LANDINGS: u64 = 10_000;
/// The largest dump file a measurement reads.
pub const DUMP_FILE_MAX: u64 = 2_147_483_648;
/// Cold repetitions of each row.
pub const RUNS: usize = 5;

/// The row names.
pub const PUSH_OP: &str = "sync_push_500mb";
pub const PULL_OP: &str = "sync_pull_full_history_500mb";
/// Both rows, in report order.
pub const SYNC_OPS: [&str; 2] = [PUSH_OP, PULL_OP];

/// The supplemental target text of the push row (D-3PNRK): never a pass or fail of the loopback.
pub const PUSH_TARGET: &str = "Production push target: at most 60000 ms for client and hub together. This run uses a loopback hub.";

/// The file name of the supplemental report.
pub const REPORT_FILE: &str = "sync-500mb.json";

/// The environment names of the inputs and the report.
pub const ENV_DUMP: &str = "RDY_QA_SYNC_DUMP";
pub const ENV_GENESIS: &str = "RDY_QA_SYNC_GENESIS_DUMP";
pub const ENV_SHA256: &str = "RDY_QA_SYNC_DUMP_SHA256";
pub const ENV_REPORT: &str = "RDY_QA_SYNC_REPORT_DIR";

#[cfg(test)]
const ERR_NEEDS_OUTPUTS: &str =
    "sync perf input generation needs RDY_QA_SYNC_DUMP and RDY_QA_SYNC_GENESIS_DUMP";
const ERR_SAME_PATH: &str = "sync perf input paths must differ";
const ERR_CONFIRMS: &str = "sync perf input needs exactly 10 open confirm items";
const ERR_FROZEN: &str =
    "sync perf needs the frozen full dump; log size must be 500000000 through 516777216 bytes";
const ERR_DIGEST: &str = "sync perf needs RDY_QA_SYNC_DUMP_SHA256 as 64 lowercase hex digits";
const ERR_REPORT_DIR: &str = "sync perf needs RDY_QA_SYNC_REPORT_DIR";
const ERR_REPORT_WRITE: &str = "sync perf could not write sync-500mb.json";

// ---------------------------------------------------------------------------------------------
// The generator
// ---------------------------------------------------------------------------------------------

/// What the generator found in the store it wrote. `landings` counts every landing of canon, the
/// setup landing included; `repeated_landings` leaves it out. `log_bytes` is the segment files'
/// length; `bytes_by_kind` the physical frame bytes by logical kind (markers and ref updates
/// included); `history_bytes` and `history_records` the batch of the complete reachable canon
/// closure, as a full push would send it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputFacts {
    pub full_sha256: String,
    pub genesis_sha256: String,
    pub landings: u64,
    pub repeated_landings: u64,
    pub log_bytes: u64,
    pub bytes_by_kind: BTreeMap<u8, u64>,
    pub history_bytes: u64,
    pub history_records: u64,
    pub canon: Hash,
}

impl InputFacts {
    /// The facts as the JSON the one-time writer prints.
    pub fn json(&self) -> Value {
        json!({
            "full_sha256": self.full_sha256,
            "genesis_sha256": self.genesis_sha256,
            "landings": self.landings,
            "repeated_landings": self.repeated_landings,
            "log_bytes": self.log_bytes,
            "bytes_by_kind": self.bytes_by_kind,
            "history_bytes": self.history_bytes,
            "history_records": self.history_records,
            "canon": self.canon.to_string(),
        })
    }
}

/// The log size a generator stops in, and its landing guard. The release workload uses
/// [`RELEASE_BOUNDS`]; the tests use a tiny range, through a private entry point.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    low: u64,
    high: u64,
    max_landings: u64,
}

const RELEASE_BOUNDS: Bounds = Bounds {
    low: LOG_LOW,
    high: LOG_HIGH,
    max_landings: MAX_LANDINGS,
};

/// What the generator does next after a completed repeated landing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    More,
    Done,
}

/// The stop rule: the first total at or above the lower bound ends the run, and a total past the
/// upper bound refuses; falling short after the last allowed landing refuses too.
fn after_landing(b: &Bounds, repeated: u64, total: u64) -> Result<Step, String> {
    if total >= b.low {
        if total > b.high {
            return Err(format!("sync perf input ended past {} log bytes", b.high));
        }
        return Ok(Step::Done);
    }
    if repeated >= b.max_landings {
        return Err(format!(
            "sync perf input did not reach {} log bytes in {} landings",
            b.low, b.max_landings
        ));
    }
    Ok(Step::More)
}

/// Repeated landings until [`after_landing`] says stop. `land(n)` makes landing `n` (from 1) and
/// returns the log size after it. Returns the number of repeated landings made.
fn run_until_stop(
    b: &Bounds,
    mut land: impl FnMut(u64) -> Result<u64, String>,
) -> Result<u64, String> {
    let mut repeated = 0;
    loop {
        repeated += 1;
        let total = land(repeated)?;
        if after_landing(b, repeated, total)? == Step::Done {
            return Ok(repeated);
        }
    }
}

/// One step of the generator, as it was done: what tests read to pin the shape of the workload.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    /// The setup write: its creates, and the sizes of the edited claims' bodies.
    Setup { creates: usize, bodies: Vec<usize> },
    /// A repeated landing's write: one update body size for each claim updated.
    Write { view: String, bodies: Vec<usize> },
    /// One `propose`.
    Propose { view: String },
    /// One `answer_plan` call: the answers it carried and their reasons.
    Answer { view: String, reasons: Vec<String> },
    /// One landing.
    Land { view: String },
}

/// `len` bytes of `unit` repeated and cut: the recipe of every body and reason.
fn recipe(landing: u64, key: &str, index: usize, len: usize) -> String {
    let unit = format!("landing={landing:08};{key}={index:02};");
    let mut text = unit.repeat(len / unit.len() + 1);
    text.truncate(len);
    text
}

/// A drop guard of the generator's seeded ids.
struct SeededIds;

impl SeededIds {
    fn new() -> SeededIds {
        rdy_model::seed_ids_for_tests(SEED);
        SeededIds
    }
}

impl Drop for SeededIds {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

/// The bytes of the segment files under `project`'s store, summed.
fn segment_bytes(project: &Path) -> Result<u64, String> {
    fn walk(dir: &Path, total: &mut u64) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(&path, total)?;
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("seg-") && name.ends_with(".log") {
                *total += entry.metadata()?.len();
            }
        }
        Ok(())
    }
    let mut total = 0;
    let dir = project.join(".rdy").join(rdy_store::layout::STORE_DIR);
    walk(&dir, &mut total).map_err(|e| format!("sync perf could not read the store: {e}"))?;
    Ok(total)
}

/// The lowercase hex SHA256 of the file at `path`, streamed.
fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn rdy_failed(what: &str, e: impl std::fmt::Display) -> String {
    format!("sync perf input: {what}: {e}")
}

/// The ids of the open items of `view`'s plan, which must be exactly ten confirm items, in item
/// order.
fn confirm_items(r: &Rdy, view: &str) -> Result<Vec<String>, String> {
    let reply = r
        .read()
        .view_plan(view)
        .map_err(|e| rdy_failed("read the plan", e))?;
    let mut items = Vec::new();
    for row in reply.items {
        match row {
            PlanRow::Live(i) if i.kind == PlanKind::Confirm && i.state == ItemState::Open => {
                items.push(i.id)
            }
            _ => return Err(ERR_CONFIRMS.to_string()),
        }
    }
    if items.len() != DEPENDENTS {
        return Err(ERR_CONFIRMS.to_string());
    }
    items.sort();
    Ok(items)
}

/// Checks that both outputs are absent, distinct and have an existing parent, before either is
/// written.
fn check_outputs(full: &Path, genesis: &Path) -> Result<(), String> {
    if full == genesis {
        return Err(ERR_SAME_PATH.to_string());
    }
    for path in [full, genesis] {
        if path.symlink_metadata().is_ok() {
            return Err(format!(
                "sync perf input already exists: {}",
                path.display()
            ));
        }
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        if !parent.is_dir() {
            return Err(format!(
                "sync perf input parent does not exist: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Writes the frozen inputs of the sync rows: the full dump at `full` and the genesis dump
/// (before the first write) at `genesis`, from one deterministic store of repeated landings that
/// stops at the first log size of 500,000,000 bytes or more. Both paths must be absent and
/// distinct, with existing parents; nothing is overwritten.
pub fn write_inputs(full: &Path, genesis: &Path) -> Result<InputFacts, String> {
    generate(full, genesis, &RELEASE_BOUNDS, |_| {})
}

/// [`write_inputs`] with the size range of `b`, observed by `observe`.
fn generate(
    full: &Path,
    genesis: &Path,
    b: &Bounds,
    mut observe: impl FnMut(Event),
) -> Result<InputFacts, String> {
    check_outputs(full, genesis)?;
    let _ids = SeededIds::new();
    let _clock = pin_clock(CLOCK_MS);
    let (dir, mut r) = crate::fixtures::project();
    // the genesis dump comes before the first write or landing
    r.read()
        .dump(genesis)
        .map_err(|e| rdy_failed("write the genesis dump", e))?;

    // setup: one draft doc, the edited claims, then the dependents of the first one, in one write
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "sync-perf", "title": "Sync performance", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$e", "parent": "$d", "fields": {"title": "Edited"}}),
    ];
    let mut bodies = Vec::new();
    for i in 0..EDITED {
        let body = recipe(0, "block", i, BODY_BYTES);
        bodies.push(body.len());
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$e{i}"), "parent": "$e", "body": body}));
    }
    ops.push(json!({"op": "create", "type": "section", "id": "$p", "parent": "$d", "fields": {"title": "Dependents"}}));
    for _ in 0..DEPENDENTS {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$p", "body": "This still follows {{$e0}}."}));
    }
    observe(Event::Setup {
        creates: ops.len(),
        bodies,
    });
    let setup_view = "sync-00000000";
    let w = crate::fixtures::write(&mut r, setup_view, Value::Array(ops))
        .map_err(|e| rdy_failed("write the setup", e))?;
    let ids: Vec<String> = (0..EDITED)
        .map(|i| w.created[&format!("$e{i}")].clone())
        .collect();
    crate::fixtures::land(&mut r, setup_view).map_err(|e| rdy_failed("land the setup", e))?;
    observe(Event::Land {
        view: setup_view.to_string(),
    });

    let repeated = run_until_stop(b, |l| {
        repeated_landing(&mut r, &ids, l, &mut observe)?;
        segment_bytes(dir.path())
    })?;

    r.read()
        .dump(full)
        .map_err(|e| rdy_failed("write the full dump", e))?;
    let log = r.log_bytes().map_err(|e| rdy_failed("count the log", e))?;
    let read = r.read();
    let canon = read
        .canon_head()
        .ok_or_else(|| "sync perf input has no canon".to_string())?;
    let (history_records, history_bytes) = history_totals(&read, canon)?;
    drop(read);
    let facts = InputFacts {
        full_sha256: file_sha256(full).map_err(|e| rdy_failed("digest the full dump", e))?,
        genesis_sha256: file_sha256(genesis)
            .map_err(|e| rdy_failed("digest the genesis dump", e))?,
        landings: repeated + 1,
        repeated_landings: repeated,
        log_bytes: segment_bytes(dir.path())?,
        bytes_by_kind: log.by_kind.iter().map(|k| (k.kind, k.bytes)).collect(),
        history_bytes,
        history_records,
        canon,
    };
    r.close().map_err(|e| rdy_failed("close the project", e))?;
    Ok(facts)
}

/// One repeated landing `l`: 20 updates in one write, one proposal, ten answers (one version
/// each) and the landing.
fn repeated_landing(
    r: &mut Rdy,
    ids: &[String],
    l: u64,
    observe: &mut impl FnMut(Event),
) -> Result<(), String> {
    let view = format!("sync-{l:08}");
    let mut bodies = Vec::new();
    let ops: Vec<Value> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let body = recipe(l, "block", i, BODY_BYTES);
            bodies.push(body.len());
            let base = crate::fixtures::version_of(r, &Scope::Canon, id);
            json!({"op": "update", "id": id, "base": base, "body": body})
        })
        .collect();
    observe(Event::Write {
        view: view.clone(),
        bodies,
    });
    crate::fixtures::write(r, &view, Value::Array(ops))
        .map_err(|e| rdy_failed("write a landing", e))?;
    let items = confirm_items(r, &view)?;
    let summary = crate::intake::summary_for(r, &view);
    observe(Event::Propose { view: view.clone() });
    let p = r
        .propose(&writer(), &view, &view, summary)
        .map_err(|e| rdy_failed("propose", e))?;
    for (i, item) in items.into_iter().enumerate() {
        let reason = recipe(l, "answer", i, REASON_BYTES);
        observe(Event::Answer {
            view: view.clone(),
            reasons: vec![reason.clone()],
        });
        r.answer_plan(
            &writer(),
            &view,
            &[PlanAnswerInput {
                item,
                act: PlanAct::Confirm,
                reason: Some(reason),
            }],
        )
        .map_err(|e| rdy_failed("answer the plan", e))?;
    }
    crate::intake::confirm_before_landing(r, &approver(), &p);
    r.land(&approver(), &p.id)
        .map_err(|e| rdy_failed("land", e))?;
    observe(Event::Land { view });
    Ok(())
}

/// The records and bytes of the complete reachable canon closure of `canon`, as one full batch.
fn history_totals(read: &rdy_engine::ReadState, canon: Hash) -> Result<(u64, u64), String> {
    let report = rdy_engine::stream_export::write_history(
        read,
        StreamPurpose::CanonHistory,
        canon,
        None,
        &BTreeSet::new(),
        std::io::sink(),
    )
    .map_err(|e| rdy_failed("count the canon history", e))?;
    Ok((report.records, report.bytes))
}

/// The reachable canon closure of `canon`, as its keys.
fn history_keys(read: &rdy_engine::ReadState, canon: Hash) -> Result<BTreeSet<ObjectKey>, String> {
    let found = rdy_store::closure::history_closure_before(
        read.reader(),
        StreamPurpose::CanonHistory,
        canon,
        &BTreeSet::new(),
        None,
        read.seq(),
    )
    .map_err(|e| rdy_failed("walk the canon history", e))?;
    Ok(found.records.into_iter().collect())
}

/// Reads the output paths of the one-time writer from `env` and writes the inputs.
#[cfg(test)]
fn write_inputs_from(env: &dyn Fn(&str) -> Option<String>) -> Result<InputFacts, String> {
    let (Some(full), Some(genesis)) = (env(ENV_DUMP), env(ENV_GENESIS)) else {
        return Err(ERR_NEEDS_OUTPUTS.to_string());
    };
    if full.is_empty() || genesis.is_empty() {
        return Err(ERR_NEEDS_OUTPUTS.to_string());
    }
    write_inputs(Path::new(&full), Path::new(&genesis))
}

// ---------------------------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------------------------

/// What a measurement reads from its environment.
#[derive(Clone, Debug)]
struct Config {
    dump: PathBuf,
    sha256: String,
    report_dir: PathBuf,
}

impl Config {
    fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Config, String> {
        let dump = env(ENV_DUMP)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ERR_FROZEN.to_string())?;
        let sha256 = env(ENV_SHA256)
            .filter(|v| v.len() == 64 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .ok_or_else(|| ERR_DIGEST.to_string())?;
        let report_dir = env(ENV_REPORT)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ERR_REPORT_DIR.to_string())?;
        Ok(Config {
            dump: PathBuf::from(dump),
            sha256,
            report_dir: PathBuf::from(report_dir),
        })
    }
}

/// The limits a measurement checks its input against. The release limits are [`Limits::RELEASE`];
/// the tests use a tiny file and log range, through a private entry point.
#[derive(Clone, Copy, Debug)]
struct Limits {
    file_max: u64,
    log_low: u64,
    log_high: u64,
}

impl Limits {
    const RELEASE: Limits = Limits {
        file_max: DUMP_FILE_MAX,
        log_low: LOG_LOW,
        log_high: LOG_HIGH,
    };
}

/// Checks the frozen file before anything reads it into a store: it is readable, no larger than
/// `file_max`, and its digest is the frozen one.
fn check_file(cfg: &Config, limits: &Limits) -> Result<(), String> {
    let meta = std::fs::metadata(&cfg.dump).map_err(|_| ERR_FROZEN.to_string())?;
    if !meta.is_file() || meta.len() > limits.file_max {
        return Err(ERR_FROZEN.to_string());
    }
    let digest = file_sha256(&cfg.dump).map_err(|_| ERR_FROZEN.to_string())?;
    if digest != cfg.sha256 {
        return Err(ERR_FROZEN.to_string());
    }
    Ok(())
}

/// What the loaded input holds: the facts every run checks its hub and its clone against.
struct Input {
    canon: Hash,
    repo_id: String,
    keys: BTreeSet<ObjectKey>,
    log_bytes: u64,
    history_bytes: u64,
    history_records: u64,
}

/// A copy of `from`, file by file.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("sync perf could not prepare a copy: {e}");
    std::fs::create_dir_all(to).map_err(failed)?;
    for entry in std::fs::read_dir(from).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(failed)?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(failed)?;
        }
    }
    Ok(())
}

/// Loads the frozen dump into `base` and reads what the runs check against. The log size must be
/// within the limits.
fn load_input(cfg: &Config, limits: &Limits, base: &Path) -> Result<Input, String> {
    std::fs::create_dir_all(base).map_err(|_| ERR_FROZEN.to_string())?;
    rdy_engine::load::load(&cfg.dump, base).map_err(|_| ERR_FROZEN.to_string())?;
    let log_bytes = segment_bytes(base)?;
    if log_bytes < limits.log_low || log_bytes > limits.log_high {
        return Err(ERR_FROZEN.to_string());
    }
    let r = Rdy::open(base).map_err(|_| ERR_FROZEN.to_string())?;
    let read = r.read();
    let canon = read.canon_head().ok_or_else(|| ERR_FROZEN.to_string())?;
    let repo_id = read
        .repo_id()
        .map_err(|_| ERR_FROZEN.to_string())?
        .to_string();
    let keys = history_keys(&read, canon)?;
    let (history_records, history_bytes) = history_totals(&read, canon)?;
    drop(read);
    r.close().map_err(|_| ERR_FROZEN.to_string())?;
    Ok(Input {
        canon,
        repo_id,
        keys,
        log_bytes,
        history_bytes,
        history_records,
    })
}

/// What one repetition counted at the hub, and how long the client took.
#[derive(Clone, Debug)]
struct Sample {
    elapsed_ms: f64,
    stats: PerfStats,
}

/// The one thing a sampler is given to time: a real CLI invocation.
type Run<'a> = &'a mut dyn FnMut() -> Result<(), String>;

/// Times `run` and returns its milliseconds; the injected samplers of the tests return what they
/// like, after running it.
type Sampler<'a> = &'a mut dyn FnMut(Run<'_>) -> Result<f64, String>;

/// The wall-clock sampler of the measurement.
fn clock(run: Run<'_>) -> Result<f64, String> {
    let started = Instant::now();
    run()?;
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

/// The hang guard of one CLI invocation, never a speed check.
const HANG_GUARD: Duration = Duration::from_secs(3600);

/// Runs the command and checks that it succeeded, saying nothing of its output.
fn run_cli(cmd: &mut std::process::Command, what: &str) -> Result<(), String> {
    let out = crate::fixtures::output_within(cmd, HANG_GUARD)
        .map_err(|_| format!("sync perf: {what} did not finish"))?;
    if !out.status.success() {
        return Err(format!("sync perf: {what} failed"));
    }
    Ok(())
}

/// Stops the project's daemon, if one runs.
fn stop_daemon(project: &Path, home: &Home) {
    let _ = command(project, home, &["serve", "--stop"]).output();
}

/// A project directory that stops its daemon when it goes.
struct Client<'a> {
    dir: tempfile::TempDir,
    home: &'a Home,
}

impl Client<'_> {
    fn project(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
}

impl Drop for Client<'_> {
    fn drop(&mut self) {
        stop_daemon(self.dir.path(), self.home);
    }
}

/// One push repetition: a copy of the loaded input, an empty hub repo and a person signed in
/// there, prepared outside the timer; the push is the timed run.
fn push_once(
    base: &Path,
    input: &Input,
    cfg: &Config,
    limits: &Limits,
    sample: Sampler<'_>,
) -> Result<Sample, String> {
    // the frozen file is the same file, checked outside the timer
    check_file(cfg, limits)?;
    let hub = Hub::start_with(Capture::Perf);
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let client = Client {
        dir: crate::daemon::short_tempdir(),
        home: &home,
    };
    copy_tree(base, &client.project())?;
    crate::sync_hub::add_share_remote(
        &client.project(),
        &home,
        &format!("{}{REMOTE_PATH}", hub.origin()),
    )
    .map_err(|_| "sync perf could not prepare a remote".to_string())?;
    stop_daemon(&client.project(), &home);
    let elapsed_ms = sample(&mut || {
        run_cli(
            &mut command(&client.project(), &home, &["--json", "push"]),
            "the push",
        )
    })?;
    stop_daemon(&client.project(), &home);
    // after the timer: the hub holds canon and the whole reachable history, sent as zstd
    let stats = hub.perf()?;
    if hub.canon() != Some(input.canon.to_string()) {
        return Err("sync perf: the hub does not hold the pushed canon".to_string());
    }
    let held = hub.shared.repo.lock().unwrap().held.clone();
    if held != input.keys {
        return Err("sync perf: the hub's objects are not the canon history".to_string());
    }
    if stats.write_requests != 1 || stats.put_requests == 0 {
        return Err("sync perf: the push did not use the put and write routes".to_string());
    }
    hub.finish();
    Ok(Sample { elapsed_ms, stats })
}

/// One clone repetition: a hub that serves the input, an absent target and a person signed in;
/// the URL clone is the timed run.
fn pull_once(
    served: &std::sync::Arc<rdy_engine::ReadState>,
    input: &Input,
    cfg: &Config,
    limits: &Limits,
    sample: Sampler<'_>,
) -> Result<Sample, String> {
    check_file(cfg, limits)?;
    let hub = Hub::start_with(Capture::Perf);
    hub.serve(std::sync::Arc::clone(served));
    hub.set_canon(Some(input.canon), Some(input.repo_id.clone()));
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let parent = crate::daemon::short_tempdir();
    let target = parent.path().join("clone");
    let url = format!("{}{REMOTE_PATH}", hub.origin());
    let target_arg = target.to_string_lossy().into_owned();
    let elapsed_ms = sample(&mut || {
        run_cli(
            &mut command_in(
                parent.path(),
                &home,
                &["--json", "clone", &url, &target_arg],
            ),
            "the clone",
        )
    })?;
    stop_daemon(&target, &home);
    let stats = hub.perf()?;
    // the fetch had no base and no held history, and the clone holds the whole canon history
    if stats.known_with_base != 0 {
        return Err("sync perf: the clone named a base".to_string());
    }
    let clone = Rdy::open(&target).map_err(|_| "sync perf: the clone does not open".to_string())?;
    let read = clone.read();
    let same =
        read.canon_head() == Some(input.canon) && history_keys(&read, input.canon)? == input.keys;
    drop(read);
    clone
        .close()
        .map_err(|_| "sync perf: the clone does not close".to_string())?;
    if !same {
        return Err("sync perf: the clone's canon history is not the served one".to_string());
    }
    if stats.get_keys != input.keys.len() as u64 {
        return Err("sync perf: the clone did not fetch every reachable object".to_string());
    }
    hub.finish();
    Ok(Sample { elapsed_ms, stats })
}

/// The row of `samples`: median and p95 of the sorted times, as the other perf rows read them.
fn summarize(op: &str, samples: &[Sample]) -> Measure {
    let mut t: Vec<f64> = samples.iter().map(|s| s.elapsed_ms).collect();
    t.sort_by(|a, b| a.total_cmp(b));
    Measure {
        op: op.to_string(),
        median_ms: t[t.len() / 2],
        p95_ms: t[((t.len() * 95) / 100).min(t.len() - 1)],
        runs: t.len(),
    }
}

/// One run of the supplemental report.
#[derive(Serialize)]
struct RunRow {
    op: &'static str,
    run: usize,
    elapsed_ms: f64,
    raw_object_bytes: u64,
    wire_object_bytes: u64,
    put_requests: u64,
    get_requests: u64,
    write_requests: u64,
    server_elapsed_ms: f64,
}

/// `sync-500mb.json`: the facts of the input and every run, in this order.
#[derive(Serialize)]
struct Report {
    source_sha256: String,
    rdy: String,
    target: String,
    log_bytes: u64,
    history_bytes: u64,
    history_records: u64,
    runs: Vec<RunRow>,
}

/// Writes the report to `dir` through a temporary file and a rename.
fn write_report(dir: &Path, report: &Report) -> Result<(), String> {
    let failed = || ERR_REPORT_WRITE.to_string();
    std::fs::create_dir_all(dir).map_err(|_| failed())?;
    let bytes = serde_json::to_vec_pretty(report).map_err(|_| failed())?;
    let tmp = dir.join(format!("{REPORT_FILE}.tmp"));
    let mut file = std::fs::File::create(&tmp).map_err(|_| failed())?;
    file.write_all(&bytes).map_err(|_| failed())?;
    file.write_all(b"\n").map_err(|_| failed())?;
    file.sync_all().map_err(|_| failed())?;
    drop(file);
    std::fs::rename(&tmp, dir.join(REPORT_FILE)).map_err(|_| failed())
}

/// The two sync rows, five cold repetitions each, on the frozen full dump named by
/// `RDY_QA_SYNC_DUMP` and `RDY_QA_SYNC_DUMP_SHA256`; the supplemental report goes to
/// `RDY_QA_SYNC_REPORT_DIR`. A missing or wrong input is an error, never a skipped row.
pub fn measure() -> Result<Vec<Measure>, String> {
    measure_from(&|k| std::env::var(k).ok())
}

/// [`measure`] on the environment `env`: the release limits, five runs, the wall clock.
fn measure_from(env: &dyn Fn(&str) -> Option<String>) -> Result<Vec<Measure>, String> {
    let cfg = Config::from_env(env)?;
    measure_rows(&cfg, &Limits::RELEASE, RUNS, &mut clock)
}

/// [`measure`] on `cfg`, with `runs` repetitions and the sampler `sample`.
fn measure_rows(
    cfg: &Config,
    limits: &Limits,
    runs: usize,
    sample: Sampler<'_>,
) -> Result<Vec<Measure>, String> {
    check_file(cfg, limits)?;
    let work =
        tempfile::tempdir().map_err(|_| "sync perf could not make a directory".to_string())?;
    let base = work.path().join("base");
    let input = load_input(cfg, limits, &base)?;

    let mut rows = Vec::new();
    let mut report_runs = Vec::new();
    let mut take = |op: &'static str, samples: &[Sample], rows: &mut Vec<Measure>| {
        rows.push(summarize(op, samples));
        for (i, s) in samples.iter().enumerate() {
            report_runs.push(RunRow {
                op,
                run: i + 1,
                elapsed_ms: s.elapsed_ms,
                raw_object_bytes: s.stats.raw_object_bytes,
                wire_object_bytes: s.stats.wire_object_bytes,
                put_requests: s.stats.put_requests,
                get_requests: s.stats.get_requests,
                write_requests: s.stats.write_requests,
                server_elapsed_ms: s.stats.server_elapsed.as_secs_f64() * 1000.0,
            });
        }
    };

    let mut pushes = Vec::new();
    for _ in 0..runs {
        pushes.push(push_once(&base, &input, cfg, limits, sample)?);
    }
    take(PUSH_OP, &pushes, &mut rows);

    // the hub serves the loaded project itself; it is opened once and only read
    let served = Rdy::open(&base).map_err(|_| ERR_FROZEN.to_string())?;
    let read = served.read();
    let mut pulls = Vec::new();
    for _ in 0..runs {
        pulls.push(pull_once(&read, &input, cfg, limits, sample)?);
    }
    drop(read);
    served
        .close()
        .map_err(|_| "sync perf could not close the input".to_string())?;
    take(PULL_OP, &pulls, &mut rows);

    write_report(
        &cfg.report_dir,
        &Report {
            source_sha256: cfg.sha256.clone(),
            rdy: rdy_engine::render_inputs::RDY_VERSION.to_string(),
            target: PUSH_TARGET.to_string(),
            log_bytes: input.log_bytes,
            history_bytes: input.history_bytes,
            history_records: input.history_records,
            runs: report_runs,
        },
    )?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::{Mode, Status, TargetClass, compare, target_class};
    use crate::sync_hub::{PERF_ENTRIES, PERF_OVERFLOW, Proj, SYNC_BASE};
    use std::net::TcpStream;
    use std::sync::{Arc, OnceLock};

    /// The size range of the tiny inputs of these tests: the setup and one or two landings.
    const TINY: Bounds = Bounds {
        low: 300_000,
        high: 4_000_000,
        max_landings: 20,
    };

    /// One tiny generated input, made once: its dump, genesis dump and facts.
    struct Tiny {
        _dir: tempfile::TempDir,
        full: PathBuf,
        genesis: PathBuf,
        facts: InputFacts,
        events: Vec<Event>,
    }

    fn tiny() -> &'static Tiny {
        static TINY_INPUT: OnceLock<Tiny> = OnceLock::new();
        TINY_INPUT.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let (full, genesis) = (
                dir.path().join("full.rdydump"),
                dir.path().join("g.rdydump"),
            );
            let mut events = Vec::new();
            let facts = generate(&full, &genesis, &TINY, |e| events.push(e)).expect("tiny input");
            Tiny {
                _dir: dir,
                full,
                genesis,
                facts,
                events,
            }
        })
    }

    /// r5905: the generator has one fixed shape and one stop rule. It runs twice to the same
    /// bytes. Each repeated landing is one write of 20 updates of 2,048 bytes, one proposal, ten
    /// answers (one plan version each, 128-byte reasons) and one landing; the stop rule is a
    /// byte source of its own, so the bounds are checked without a large store.
    #[test]
    fn r5905_generator_has_fixed_shape_and_stop() {
        let first = tiny();
        let dir = tempfile::tempdir().unwrap();
        let (full, genesis) = (dir.path().join("f"), dir.path().join("g"));
        let mut events = Vec::new();
        let again = generate(&full, &genesis, &TINY, |e| events.push(e)).expect("a second run");
        assert_eq!(first.facts, again, "the facts repeat");
        assert_eq!(first.events, events, "the steps repeat");
        assert_eq!(
            std::fs::read(&first.full).unwrap(),
            std::fs::read(&full).unwrap()
        );
        assert_eq!(
            std::fs::read(&first.genesis).unwrap(),
            std::fs::read(&genesis).unwrap()
        );
        assert_eq!(first.facts.full_sha256, file_sha256(&first.full).unwrap());
        assert_eq!(
            first.facts.genesis_sha256,
            file_sha256(&first.genesis).unwrap()
        );
        for digest in [&first.facts.full_sha256, &first.facts.genesis_sha256] {
            assert_eq!(digest.len(), 64);
            assert!(
                digest
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            );
        }

        // the setup: 33 creates (doc, two sections, 20 edited claims, 10 dependents), bodies of 2,048
        let facts = &first.facts;
        let events = &first.events;
        assert_eq!(
            events[0],
            Event::Setup {
                creates: 3 + EDITED + DEPENDENTS,
                bodies: vec![BODY_BYTES; EDITED]
            }
        );
        assert_eq!(
            events[1],
            Event::Land {
                view: "sync-00000000".into()
            }
        );
        // each repeated landing: write, propose, ten answers, land
        assert!(facts.repeated_landings >= 1);
        assert_eq!(facts.landings, facts.repeated_landings + 1);
        assert_eq!(events.len() as u64, 2 + 13 * facts.repeated_landings);
        for l in 1..=facts.repeated_landings {
            let view = format!("sync-{l:08}");
            let at = (2 + 13 * (l - 1)) as usize;
            assert_eq!(
                events[at],
                Event::Write {
                    view: view.clone(),
                    bodies: vec![BODY_BYTES; EDITED]
                }
            );
            assert_eq!(events[at + 1], Event::Propose { view: view.clone() });
            let mut reasons = BTreeSet::new();
            for i in 0..DEPENDENTS {
                let Event::Answer {
                    view: v,
                    reasons: r,
                } = &events[at + 2 + i]
                else {
                    panic!("answer {i} of {view}: {:?}", events[at + 2 + i]);
                };
                assert_eq!((v, r.len()), (&view, 1), "one answer in each call");
                assert_eq!(r[0].len(), REASON_BYTES);
                assert!(r[0].starts_with(&format!("landing={l:08};answer={i:02};")));
                reasons.insert(r[0].clone());
            }
            assert_eq!(reasons.len(), DEPENDENTS, "ten distinct answers");
            assert_eq!(events[at + 12], Event::Land { view });
        }
        let bodies: BTreeSet<String> = (0..EDITED)
            .map(|i| recipe(7, "block", i, BODY_BYTES))
            .collect();
        assert_eq!(bodies.len(), EDITED);
        assert!(bodies.iter().all(|b| b.len() == BODY_BYTES && b.is_ascii()));
        assert!(
            recipe(7, "block", 3, BODY_BYTES).starts_with("landing=00000007;block=03;landing=")
        );

        // the store holds what the steps did: that many landings; the last one sealed ten items
        assert!(facts.log_bytes >= TINY.low && facts.log_bytes <= TINY.high);
        assert!(facts.history_records > 0 && facts.history_bytes > 0);
        let load_dir = tempfile::tempdir().unwrap();
        let project = load_dir.path().join("p");
        std::fs::create_dir_all(&project).unwrap();
        let loaded = rdy_engine::load::load(&first.full, &project).expect("load the full dump");
        assert_eq!(loaded.landings as u64, facts.landings);
        let r = Rdy::open(&project).unwrap();
        assert_eq!(r.read().canon_head(), Some(facts.canon));
        let q = rdy_engine::plan::PlanQuery {
            landing: Some(facts.canon.to_string()),
            max_tokens: Some(rdy_engine::plan::PLAN_MAX_TOKENS),
            ..Default::default()
        };
        let sealed = r.read().change_plan(&q, "main").expect("the sealed plan");
        let items = sealed
            .items
            .iter()
            .filter(|row| matches!(row, PlanRow::Sealed(_)))
            .count();
        assert_eq!(
            items, DEPENDENTS,
            "the last landing sealed its ten confirm items"
        );
        r.close().unwrap();

        // the stop rule, on a counting byte source: the first total at or above the lower bound
        let b = Bounds {
            low: 100,
            high: 150,
            max_landings: 10,
        };
        let mut calls = 0;
        let n = run_until_stop(&b, |n| {
            calls += 1;
            Ok(n * 40)
        });
        assert_eq!(
            (n, calls),
            (Ok(3), 3),
            "stops after the first landing at the target"
        );
        for (total, ok) in [(100, true), (150, true), (151, false)] {
            let n = run_until_stop(&b, |_| Ok(total));
            assert_eq!(n.is_ok(), ok, "{total}");
        }
        let mut calls = 0;
        let n = run_until_stop(&b, |n| {
            calls += 1;
            Ok(n * 80)
        });
        assert_eq!(
            n,
            Err("sync perf input ended past 150 log bytes".to_string())
        );
        assert_eq!(
            calls, 2,
            "refused at the first landing past the upper bound"
        );
        let mut calls = 0;
        let n = run_until_stop(&b, |n| {
            calls += 1;
            Ok(n)
        });
        assert_eq!(
            n,
            Err("sync perf input did not reach 100 log bytes in 10 landings".to_string())
        );
        assert_eq!(calls, 10);
        assert_eq!(
            run_until_stop(&b, |_| Err("a failed landing".into())),
            Err("a failed landing".into())
        );
        // only the production constants select the real workload
        assert_eq!(
            (
                RELEASE_BOUNDS.low,
                RELEASE_BOUNDS.high,
                RELEASE_BOUNDS.max_landings
            ),
            (500_000_000, 516_777_216, 10_000)
        );
        assert_eq!(
            run_until_stop(&RELEASE_BOUNDS, |_| Ok(516_777_217)),
            Err("sync perf input ended past 516777216 log bytes".to_string())
        );
        let mut calls = 0u64;
        let n = run_until_stop(&RELEASE_BOUNDS, |_| {
            calls += 1;
            Ok(499_999_999)
        });
        assert_eq!(
            n,
            Err("sync perf input did not reach 500000000 log bytes in 10000 landings".to_string())
        );
        assert_eq!(calls, 10_000);
        assert_eq!(run_until_stop(&RELEASE_BOUNDS, |_| Ok(500_000_000)), Ok(1));
        assert_eq!(run_until_stop(&RELEASE_BOUNDS, |_| Ok(516_777_216)), Ok(1));

        // the first total at the target ends the run: a lower bound of exactly that total gives
        // the same landings, and one byte more asks for another
        let stop_at = |low: u64| {
            let d = tempfile::tempdir().unwrap();
            let b = Bounds { low, ..TINY };
            generate(&d.path().join("f"), &d.path().join("g"), &b, |_| {})
                .map(|f| f.repeated_landings)
        };
        assert_eq!(stop_at(facts.log_bytes), Ok(facts.repeated_landings));
        assert_eq!(
            stop_at(facts.log_bytes + 1),
            Ok(facts.repeated_landings + 1)
        );
        let d = tempfile::tempdir().unwrap();
        let guard = Bounds {
            max_landings: 1,
            low: facts.log_bytes + 1,
            ..TINY
        };
        assert_eq!(
            generate(&d.path().join("f"), &d.path().join("g"), &guard, |_| {}),
            Err(format!(
                "sync perf input did not reach {} log bytes in 1 landings",
                facts.log_bytes + 1
            ))
        );
        // the generator reaches the store through base APIs only, so its files carry into an
        // older build: no span map, dedup, delta-plan or compact call
        let src = include_str!("sync_perf.rs");
        let section =
            &src[src.find("// The generator\n").unwrap()..src.find("// The rows\n").unwrap()];
        for word in [
            "dedup",
            "spanmap",
            "plan_delta",
            "compact(",
            "punch",
            "snapshot_upgrade",
        ] {
            assert!(!section.to_lowercase().contains(word), "{word}");
        }
        // a refused run leaves the ids and the clock as it found them
        assert!(!rdy_model::ids_seeded_for_tests());
    }

    /// r5905: no input is overwritten. The checks come before either file is written, with the
    /// exact errors; the generator never touches the second path when the first is refused.
    #[test]
    fn r5905_generator_never_overwrites_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let (full, genesis) = (dir.path().join("full"), dir.path().join("genesis"));
        let texts = |f: &Path, g: &Path| generate(f, g, &TINY, |_| {}).unwrap_err();

        std::fs::write(&full, b"keep").unwrap();
        assert_eq!(
            texts(&full, &genesis),
            format!("sync perf input already exists: {}", full.display())
        );
        assert!(!genesis.exists(), "the other path is not written");
        assert_eq!(std::fs::read(&full).unwrap(), b"keep");
        std::fs::remove_file(&full).unwrap();

        std::fs::write(&genesis, b"keep").unwrap();
        assert_eq!(
            texts(&full, &genesis),
            format!("sync perf input already exists: {}", genesis.display())
        );
        assert!(!full.exists());
        assert_eq!(std::fs::read(&genesis).unwrap(), b"keep");
        std::fs::remove_file(&genesis).unwrap();

        assert_eq!(texts(&full, &full), "sync perf input paths must differ");
        let missing = dir.path().join("nowhere").join("full");
        assert_eq!(
            texts(&missing, &genesis),
            format!(
                "sync perf input parent does not exist: {}",
                missing.display()
            )
        );
        let missing_g = dir.path().join("nowhere").join("g");
        assert_eq!(
            texts(&full, &missing_g),
            format!(
                "sync perf input parent does not exist: {}",
                missing_g.display()
            )
        );
        assert!(!full.exists() && !genesis.exists());

        // the writer's inputs come from the environment, both of them
        let env = |full: Option<&str>, genesis: Option<&str>| {
            let (f, g) = (full.map(String::from), genesis.map(String::from));
            move |k: &str| match k {
                ENV_DUMP => f.clone(),
                ENV_GENESIS => g.clone(),
                _ => None,
            }
        };
        for (f, g) in [
            (None, None),
            (Some("a"), None),
            (None, Some("b")),
            (Some(""), Some("b")),
        ] {
            assert_eq!(
                write_inputs_from(&env(f, g)).unwrap_err(),
                "sync perf input generation needs RDY_QA_SYNC_DUMP and RDY_QA_SYNC_GENESIS_DUMP"
            );
        }
        let same = full.to_str().unwrap();
        assert_eq!(
            write_inputs_from(&env(Some(same), Some(same))).unwrap_err(),
            "sync perf input paths must differ"
        );

        // full and genesis are one repo; genesis holds no landing
        let t = tiny();
        let load = |file: &Path| {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("p");
            std::fs::create_dir_all(&p).unwrap();
            rdy_engine::load::load(file, &p).expect("load");
            let r = Rdy::open(&p).unwrap();
            let out = (
                r.read().canon_head(),
                r.read().repo_id().unwrap().to_string(),
            );
            r.close().unwrap();
            out
        };
        let (full_canon, full_repo) = load(&t.full);
        let (genesis_canon, genesis_repo) = load(&t.genesis);
        assert_eq!(full_canon, Some(t.facts.canon));
        assert_eq!(genesis_canon, None, "genesis has no canon landing");
        assert_eq!(full_repo, genesis_repo);

        // the one-time writer is ignored: only the orchestrator runs it
        let src = include_str!("sync_perf.rs");
        let ignored = ["#[ignore]", "    fn r5905_write_sync_perf_inputs_once"].join("\n");
        assert!(src.contains(&ignored), "the writer is ignored");
    }

    /// The one-time writer of the frozen inputs. The orchestrator runs it once, on the build that
    /// freezes them: `RDY_QA_SYNC_DUMP` and `RDY_QA_SYNC_GENESIS_DUMP` name two absent files. It
    /// prints the facts as JSON.
    #[test]
    #[ignore]
    fn r5905_write_sync_perf_inputs_once() {
        let facts = write_inputs_from(&|k| std::env::var(k).ok()).unwrap_or_else(|e| panic!("{e}"));
        println!("{}", serde_json::to_string_pretty(&facts.json()).unwrap());
    }

    /// A config on the tiny input, a report directory and limits that fit it exactly.
    fn tiny_config(report: &Path) -> (Config, Limits) {
        let t = tiny();
        (
            Config {
                dump: t.full.clone(),
                sha256: t.facts.full_sha256.clone(),
                report_dir: report.to_path_buf(),
            },
            Limits {
                file_max: u64::MAX,
                log_low: t.facts.log_bytes,
                log_high: t.facts.log_bytes,
            },
        )
    }

    /// r5905: both rows run the real CLI on a tiny input, five samples each, with a sampler that
    /// replaces the clock. The push puts the canon history on an empty hub; the clone is a URL
    /// clone into an absent directory with no base, and fetches every reachable object. Each run
    /// checks canon and the complete object inventory itself. No elapsed time is asserted.
    #[test]
    fn r5905_rows_measure_full_history() {
        let report = tempfile::tempdir().unwrap();
        let (cfg, limits) = tiny_config(report.path());
        let mut calls = 0u32;
        let mut sampler = |run: Run<'_>| -> Result<f64, String> {
            run()?;
            calls += 1;
            Ok(f64::from(calls))
        };
        let rows = measure_rows(&cfg, &limits, RUNS, &mut sampler).expect("both rows");
        assert_eq!(calls, 10);
        assert_eq!(
            rows.iter()
                .map(|m| (m.op.as_str(), m.runs))
                .collect::<Vec<_>>(),
            [("sync_push_500mb", 5), ("sync_pull_full_history_500mb", 5)]
        );
        // the injected samples 1..5 and 6..10
        assert_eq!((rows[0].median_ms, rows[0].p95_ms), (3.0, 5.0));
        assert_eq!((rows[1].median_ms, rows[1].p95_ms), (8.0, 10.0));

        let text = std::fs::read_to_string(report.path().join(REPORT_FILE)).unwrap();
        assert!(!report.path().join(format!("{REPORT_FILE}.tmp")).exists());
        let v: Value = serde_json::from_str(&text).unwrap();
        // the fields, in order
        let at = |needle: &str| {
            text.find(&format!("\"{needle}\""))
                .unwrap_or_else(|| panic!("{needle}"))
        };
        let order = [
            "source_sha256",
            "rdy",
            "target",
            "log_bytes",
            "history_bytes",
            "history_records",
            "runs",
        ];
        assert!(order.windows(2).all(|w| at(w[0]) < at(w[1])), "{order:?}");
        let run_order = [
            "op",
            "run",
            "elapsed_ms",
            "raw_object_bytes",
            "wire_object_bytes",
            "put_requests",
            "get_requests",
            "write_requests",
            "server_elapsed_ms",
        ];
        let first_run = &text[at("runs")..];
        let pos = |needle: &str| first_run.find(&format!("\"{needle}\"")).unwrap();
        assert!(
            run_order.windows(2).all(|w| pos(w[0]) < pos(w[1])),
            "{run_order:?}"
        );
        assert!(
            !text.contains("publication_file_bytes") && !text.contains("client_peak_rss_bytes")
        );
        assert_eq!(v["source_sha256"], cfg.sha256);
        assert_eq!(v["target"], PUSH_TARGET);
        assert_eq!(v["log_bytes"], tiny().facts.log_bytes);
        assert_eq!(v["history_bytes"], tiny().facts.history_bytes);
        assert_eq!(v["history_records"], tiny().facts.history_records);
        let runs = v["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 10);
        for (i, run) in runs.iter().enumerate() {
            let (op, n) = if i < 5 {
                (PUSH_OP, i + 1)
            } else {
                (PULL_OP, i - 4)
            };
            assert_eq!(
                (run["op"].as_str(), run["run"].as_u64()),
                (Some(op), Some(n as u64))
            );
            assert!(run["raw_object_bytes"].as_u64().unwrap() > 0, "{run}");
            assert!(run["wire_object_bytes"].as_u64().unwrap() > 0, "{run}");
            if op == PUSH_OP {
                assert!(run["put_requests"].as_u64().unwrap() >= 1, "{run}");
                assert_eq!(run["write_requests"], 1);
                assert_eq!(run["get_requests"], 0);
                // the put bodies went as zstd
                assert!(
                    run["wire_object_bytes"].as_u64() < run["raw_object_bytes"].as_u64(),
                    "{run}"
                );
            } else {
                assert!(run["get_requests"].as_u64().unwrap() >= 1, "{run}");
                assert_eq!(run["put_requests"], 0);
                assert_eq!(run["write_requests"], 0);
            }
        }
    }

    /// r5905: the frozen input's identity and size are checked before any request. A wrong
    /// digest, a file past the limit, a log size at either side of its range and an absent file
    /// each refuse with the one error, and the sampler never runs.
    #[test]
    fn r5905_fixture_identity_and_size_are_checked() {
        let report = tempfile::tempdir().unwrap();
        let (cfg, limits) = tiny_config(report.path());
        let frozen = ERR_FROZEN.to_string();
        let run = |cfg: &Config, limits: &Limits| {
            let mut samples = 0;
            let mut sampler = |_: Run<'_>| -> Result<f64, String> {
                samples += 1;
                Ok(1.0)
            };
            let out = measure_rows(cfg, limits, RUNS, &mut sampler);
            (out.map(|_| ()), samples)
        };
        let bad = |c: &Config, l: &Limits| assert_eq!(run(c, l), (Err(frozen.clone()), 0));

        // a wrong digest, an absent file, a file past the limit
        bad(
            &Config {
                sha256: "0".repeat(64),
                ..cfg.clone()
            },
            &limits,
        );
        bad(
            &Config {
                dump: report.path().join("absent"),
                ..cfg.clone()
            },
            &limits,
        );
        let len = std::fs::metadata(&cfg.dump).unwrap().len();
        bad(
            &cfg,
            &Limits {
                file_max: len - 1,
                ..limits
            },
        );
        // the log size: each side of its range
        let log = tiny().facts.log_bytes;
        bad(
            &cfg,
            &Limits {
                log_low: log + 1,
                log_high: log + 10,
                ..limits
            },
        );
        bad(
            &cfg,
            &Limits {
                log_low: log - 10,
                log_high: log - 1,
                ..limits
            },
        );
        // the release limits refuse a tiny log, with the same text
        bad(&cfg, &Limits::RELEASE);
        assert_eq!(
            ERR_FROZEN,
            "sync perf needs the frozen full dump; log size must be 500000000 through 516777216 bytes"
        );
        assert_eq!(
            (
                Limits::RELEASE.file_max,
                Limits::RELEASE.log_low,
                Limits::RELEASE.log_high
            ),
            (2_147_483_648, 500_000_000, 516_777_216)
        );
        // a file at the limit is read; the digest is of its bytes
        assert!(
            check_file(
                &cfg,
                &Limits {
                    file_max: len,
                    ..limits
                }
            )
            .is_ok()
        );

        // the environment: the digest is 64 lowercase hex digits; the genesis is never read
        let env = |dump: Option<&str>, sha: Option<&str>, dir: Option<&str>| {
            let (d, s, r) = (
                dump.map(String::from),
                sha.map(String::from),
                dir.map(String::from),
            );
            move |k: &str| match k {
                ENV_DUMP => d.clone(),
                ENV_SHA256 => s.clone(),
                ENV_REPORT => r.clone(),
                ENV_GENESIS => Some("/nowhere/genesis".to_string()),
                _ => None,
            }
        };
        let good = "ab".repeat(32);
        for sha in [
            None,
            Some(""),
            Some("AB"),
            Some(&"AB".repeat(32)[..]),
            Some(&"ab".repeat(31)[..]),
            Some(&"ag".repeat(32)[..]),
        ] {
            let e = Config::from_env(&env(Some("d"), sha, Some("r"))).unwrap_err();
            assert_eq!(
                e,
                "sync perf needs RDY_QA_SYNC_DUMP_SHA256 as 64 lowercase hex digits"
            );
        }
        assert_eq!(
            Config::from_env(&env(None, Some(&good), Some("r"))).unwrap_err(),
            frozen
        );
        assert_eq!(
            Config::from_env(&env(Some("d"), Some(&good), None)).unwrap_err(),
            "sync perf needs RDY_QA_SYNC_REPORT_DIR"
        );
        let c = Config::from_env(&env(Some("d"), Some(&good), Some("r"))).unwrap();
        assert_eq!(
            (c.dump, c.report_dir),
            (PathBuf::from("d"), PathBuf::from("r"))
        );

        // the entry that selects a release workload takes no input of its own
        let src = include_str!("sync_perf.rs");
        let body = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        assert!(!body.contains("pub fn measure_rows") && !body.contains("pub fn measure_from"));
        assert!(body.contains("measure_rows(&cfg, &Limits::RELEASE, RUNS, &mut clock)"));
        assert_eq!(
            body.matches("Limits::RELEASE").count(),
            2,
            "its definition and its one use"
        );
    }

    /// A TLS client of one hub: one request, one reply.
    struct Client {
        config: Arc<rustls::ClientConfig>,
        port: u16,
    }

    impl Client {
        fn new(hub: &Hub) -> Client {
            use rustls::pki_types::{CertificateDer, pem::PemObject};
            let mut roots = rustls::RootCertStore::empty();
            for cert in CertificateDer::pem_slice_iter(hub.ca_pem.as_bytes()) {
                roots.add(cert.unwrap()).unwrap();
            }
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let port = hub.origin().rsplit(':').next().unwrap().parse().unwrap();
            Client {
                config: Arc::new(config),
                port,
            }
        }

        /// The status and body of one request.
        fn send(
            &self,
            method: &str,
            target: &str,
            headers: &[(&str, &str)],
            body: &[u8],
        ) -> (u16, Vec<u8>) {
            let name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();
            let conn = rustls::ClientConnection::new(Arc::clone(&self.config), name).unwrap();
            let sock = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, sock);
            let mut head = format!(
                "{method} {target} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: {}\r\nconnection: close\r\n",
                body.len()
            );
            for (n, v) in headers {
                head.push_str(&format!("{n}: {v}\r\n"));
            }
            head.push_str("\r\n");
            tls.write_all(head.as_bytes()).unwrap();
            tls.write_all(body).unwrap();
            tls.flush().unwrap();
            let mut reply = Vec::new();
            let _ = tls.read_to_end(&mut reply);
            let status = std::str::from_utf8(&reply[9..12]).unwrap().parse().unwrap();
            let at = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            (status, reply[at + 4..].to_vec())
        }
    }

    /// r5905: a perf-mode hub keeps counters and at most 4,096 entries, never a body. A zstd put
    /// body is decoded under the batch limits; the 4,097th request refuses and the stats then say
    /// so. A default-mode hub still keeps every body.
    #[test]
    fn r5905_perf_capture_does_not_retain_bodies() {
        // a batch: the history of one landing of a small project
        let proj = Proj::new(&["a"]);
        let r = Rdy::open(proj.path()).unwrap();
        let read = r.read();
        let mut batch = Vec::new();
        let report = rdy_engine::stream_export::write_history(
            &read,
            StreamPurpose::CanonHistory,
            read.canon_head().unwrap(),
            None,
            &BTreeSet::new(),
            &mut batch,
        )
        .unwrap();
        drop(read);
        r.close().unwrap();
        let framed = zstd::encode_all(&batch[..], 3).unwrap();
        assert!(framed.len() < batch.len());

        let hub = Hub::start_with(Capture::Perf);
        let client = Client::new(&hub);
        let put = |push: &str| format!("{SYNC_BASE}/objects/put?push={push}");
        // the state step says the hub takes zstd
        let (status, body) = client.send(
            "POST",
            &format!("{SYNC_BASE}/state"),
            &[],
            br#"{"views":[],"rdy":"0.58.0"}"#,
        );
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["zstd"],
            true
        );
        // a compressed put and an identity put both work
        let (status, body) = client.send(
            "POST",
            &put("one"),
            &[("content-encoding", "zstd")],
            &framed,
        );
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let reply: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply["records"], report.records);
        assert_eq!(reply["bytes"], batch.len());
        let (status, _) = client.send("POST", &put("two"), &[], &batch);
        assert_eq!(status, 200);
        // a body that is not a frame, and an encoding the hub does not take, are refused
        let (status, _) = client.send(
            "POST",
            &put("three"),
            &[("content-encoding", "zstd")],
            b"not a frame",
        );
        assert_eq!(status, 422);
        let (status, _) = client.send(
            "POST",
            &put("four"),
            &[("content-encoding", "gzip")],
            &batch,
        );
        assert_eq!(status, 422);

        let stats = hub.perf().unwrap();
        assert_eq!(stats.requests, 5);
        assert_eq!(stats.put_requests, 4);
        assert_eq!(
            stats.raw_object_bytes,
            2 * batch.len() as u64 + 11 + batch.len() as u64
        );
        assert_eq!(
            stats.wire_object_bytes,
            (framed.len() + batch.len() + 11 + batch.len()) as u64
        );
        assert!(stats.entries.iter().all(|e| e.method == "POST"));
        assert_eq!(stats.entries[1].status, 200);
        assert_eq!(stats.entries[1].request_bytes, framed.len() as u64);
        assert_eq!(stats.entries[2].request_bytes, batch.len() as u64);
        assert!(stats.server_elapsed > Duration::ZERO);
        assert!(
            hub.requests().is_empty(),
            "no request, and so no body, is kept"
        );

        // the bound: 4,096 entries stay, the 4,097th refuses, and the stats say so for good
        let mut answered = 0;
        while stats.entries.len() + answered < PERF_ENTRIES {
            let (status, _) = client.send("GET", REMOTE_PATH, &[], b"");
            assert_eq!(status, 200);
            answered += 1;
        }
        assert_eq!(hub.perf().unwrap().entries.len(), PERF_ENTRIES);
        let (status, body) = client.send("GET", REMOTE_PATH, &[], b"");
        assert_eq!(status, 500);
        assert!(String::from_utf8_lossy(&body).contains(PERF_OVERFLOW));
        assert_eq!(
            hub.perf().unwrap_err(),
            "sync perf request log exceeds 4096 entries"
        );
        let (status, _) = client.send("GET", REMOTE_PATH, &[], b"");
        assert_eq!(status, 500);
        assert!(
            hub.perf().is_err(),
            "an overflowed log never reports success"
        );
        hub.finish();

        // the default mode: bodies are kept, and a zstd put is not advertised or decoded
        let hub = Hub::start();
        let client = Client::new(&hub);
        let (status, body) = client.send(
            "POST",
            &format!("{SYNC_BASE}/state"),
            &[],
            br#"{"views":[],"rdy":"0.58.0"}"#,
        );
        assert_eq!(status, 200);
        assert!(
            serde_json::from_slice::<Value>(&body)
                .unwrap()
                .get("zstd")
                .is_none()
        );
        let (status, _) = client.send("POST", &put("one"), &[], &batch);
        assert_eq!(status, 200);
        let kept = hub.requests();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[1].body, batch);
        assert!(hub.perf().is_err());
        hub.finish();
    }

    /// r5905: the Gate arm of qa-perf appends the two reported rows after the workload, and the
    /// renderer-only arm does not; an absent or malformed input fails the run, it never skips.
    /// Both names are informational in the comparator, with the report's fields and errors.
    #[test]
    fn r5905_gate_binary_appends_reported_sync_rows() {
        let main = include_str!("bin/qa-perf.rs");
        let gate = &main[main.find("PerfMode::Gate =>").unwrap()
            ..main.find("PerfMode::Renderer10k =>").unwrap()];
        let renderer = &main[main.find("PerfMode::Renderer10k =>").unwrap()..];
        assert!(gate.contains("run_workload()") && gate.contains("rdy_qa::sync_perf::measure()"));
        assert!(gate.contains("rows.extend(sync)") && gate.contains("std::process::exit(1)"));
        assert!(!renderer.contains("sync_perf"));
        // run_workload never measures the rows: correctness tests call it
        let perf = include_str!("perf.rs");
        let from = perf.find("pub fn run_workload(").unwrap();
        let body = &perf[from..from + perf[from..].find("\n}\n").unwrap()];
        assert!(!body.contains("sync_perf") && !body.contains(PUSH_OP));

        // absent and malformed inputs fail, in order, before any file is read
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(measure_from(&env(&[])).unwrap_err(), ERR_FROZEN);
        assert_eq!(
            measure_from(&env(&[(ENV_DUMP, "/nowhere")])).unwrap_err(),
            "sync perf needs RDY_QA_SYNC_DUMP_SHA256 as 64 lowercase hex digits"
        );
        const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(
            measure_from(&env(&[(ENV_DUMP, "/nowhere"), (ENV_SHA256, SHA)])).unwrap_err(),
            "sync perf needs RDY_QA_SYNC_REPORT_DIR"
        );
        assert_eq!(
            measure_from(&env(&[
                (ENV_DUMP, "/nowhere"),
                (ENV_SHA256, SHA),
                (ENV_REPORT, "/nowhere/r")
            ]))
            .unwrap_err(),
            ERR_FROZEN
        );
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let report = Report {
            source_sha256: SHA.into(),
            rdy: "0".into(),
            target: PUSH_TARGET.into(),
            log_bytes: 0,
            history_bytes: 0,
            history_records: 0,
            runs: vec![],
        };
        assert_eq!(
            write_report(&blocker, &report).unwrap_err(),
            "sync perf could not write sync-500mb.json"
        );

        // the comparator: both rows are informational, recorded in every mode, never baselined
        assert_eq!(
            SYNC_OPS,
            ["sync_push_500mb", "sync_pull_full_history_500mb"]
        );
        let baseline: Value = serde_json::from_str(include_str!("../qa-baseline.json")).unwrap();
        for op in SYNC_OPS {
            assert_eq!(target_class(op), Some(TargetClass::NoTarget), "{op}");
            assert_eq!(crate::perf::spec_ceiling_ms(op), None, "{op}");
            assert_eq!(
                target_class(&format!("{op}_50k")),
                Some(TargetClass::NoTarget)
            );
            for class in baseline["classes"].as_object().unwrap().values() {
                assert!(class["ops"].get(op).is_none(), "{op} is not baselined");
            }
            let m = Measure {
                op: op.into(),
                median_ms: 1e9,
                p95_ms: 1e9,
                runs: 5,
            };
            for mode in [Mode::Compare, Mode::Record] {
                let v = compare(None, std::slice::from_ref(&m), mode);
                assert_eq!((v[0].status, v[0].pass), (Status::Recorded, true), "{op}");
                let text = v[0].spec_target.clone().unwrap();
                assert!(text.starts_with("recorded: "), "{text}");
                assert_eq!(text.contains(PUSH_TARGET), op == PUSH_OP, "{text}");
            }
        }
        assert_eq!(
            PUSH_TARGET,
            "Production push target: at most 60000 ms for client and hub together. This run uses a loopback hub."
        );
        // the rows and the other names stay apart
        assert_eq!(target_class("sync_push_500mb_extra"), None);
    }
}
