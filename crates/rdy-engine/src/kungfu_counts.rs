//! The local kungfu counts: how often an agent called a tool without the ruleset that owns it, in
//! a closed, bounded file `.rdy/run/kungfu-counts.json`.
//!
//! The counts are a best-effort local signal. No count failure changes a call's result: a
//! recorder never waits on a file lock, on disk or on its worker in a reply path; it adds a miss
//! to bounded in-memory counters and wakes one background worker. The worker merges pending
//! deltas into the file under `.rdy/run/kungfu-counts.lock` (`File::try_lock`, never waited on)
//! and replaces the file with `fsutil::replace_in_dir`. A process crash can lose unflushed
//! counts. Nothing here claims store-grade durability, and nothing here touches the log, the
//! index or any other store file.
//!
//! The file is `{schema: 1, rows: [...]}`. A row is exactly `day`, `actor`, `run`, `ruleset`,
//! `not_loaded`, `lost` and `wrong`: a UTC day, the session's actor and run (null when the
//! session has no run), the owning ruleset's name and three saturating `u64` counters. A row
//! holds no value, tool argument, document id or text. The fixed bounds are the `MAX_*` and
//! `RETAIN_DAYS` constants below; hitting one loses telemetry, never a call.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{File, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rdy_store::fileid::{self, FileId};
use rdy_store::{fsutil, pio};
use serde::{Deserialize, Deserializer, Serialize};

use crate::identity::{is_actor, is_name, is_run};

/// The file's schema number.
pub const SCHEMA: u64 = 1;
/// Today and the previous 29 UTC days.
pub const RETAIN_DAYS: i64 = 30;
/// The most bytes of a count file, checked before the file is read into memory.
pub const MAX_FILE_BYTES: u64 = 8_388_608;
/// The most rows across the retained days.
pub const MAX_ROWS: usize = 16_384;
/// The most distinct rows a recorder holds before they reach the file.
pub const MAX_PENDING_ROWS: usize = 16_384;
/// The deepest JSON nesting of a count file.
pub const MAX_DEPTH: usize = 4;
/// How long the worker waits after a failed flush before it tries again.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// How long an idle worker waits before it reads the file again, so totals that another process
/// recorded show up without a local miss.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The count file's name, in `.rdy/run`.
pub const FILE_NAME: &str = "kungfu-counts.json";
/// The lock file's name, in `.rdy/run`. It is never replaced or deleted.
pub const LOCK_NAME: &str = "kungfu-counts.lock";
const RUN_DIR: &str = "run";

// ---------------------------------------------------------------------------------------------
// Days
// ---------------------------------------------------------------------------------------------

/// A UTC calendar day, as the number of days since 1970-01-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(i64);

impl Day {
    /// The day that holds `secs` seconds since the Unix epoch.
    pub fn from_unix_secs(secs: i64) -> Day {
        Day(secs.div_euclid(86_400))
    }

    /// The current UTC day.
    pub fn today() -> Day {
        let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            Err(_) => 0,
        };
        Day::from_unix_secs(secs)
    }

    /// `YYYY-MM-DD` and nothing else: a date that does not exist is `None`.
    pub fn parse(text: &str) -> Option<Day> {
        let b = text.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let digits = |r: std::ops::Range<usize>| -> Option<i64> {
            let mut n = 0i64;
            for &c in &b[r] {
                if !c.is_ascii_digit() {
                    return None;
                }
                n = n * 10 + i64::from(c - b'0');
            }
            Some(n)
        };
        let (y, m, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let day = Day(days_from_civil(y, m, d));
        (day.to_string() == text).then_some(day)
    }

    /// The day `n` days before this one.
    pub fn minus(self, n: i64) -> Day {
        Day(self.0.saturating_sub(n))
    }

    /// The first retained day when this day is today.
    fn oldest_retained(self) -> Day {
        self.minus(RETAIN_DAYS - 1)
    }

    /// Whether a row of this day is kept when `today` is the current day.
    fn retained_at(self, today: Day) -> bool {
        self <= today && self >= today.oldest_retained()
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (y, m, d) = civil_from_days(self.0);
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (month 1..=12, day 1..=31).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number: the inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------------------------

/// What went wrong with a call, in the three closed cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// The caller never loaded the owning ruleset.
    NotLoaded,
    /// The caller loaded it, but the load is gone from its context.
    Lost,
    /// The caller loaded a different ruleset than the tool's owner.
    Wrong,
}

/// The three counters of one row. Every add saturates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub not_loaded: u64,
    pub lost: u64,
    pub wrong: u64,
}

impl Counts {
    fn bump(&mut self, case: Case) {
        let c = match case {
            Case::NotLoaded => &mut self.not_loaded,
            Case::Lost => &mut self.lost,
            Case::Wrong => &mut self.wrong,
        };
        *c = c.saturating_add(1);
    }

    fn add(&mut self, o: &Counts) {
        self.not_loaded = self.not_loaded.saturating_add(o.not_loaded);
        self.lost = self.lost.saturating_add(o.lost);
        self.wrong = self.wrong.saturating_add(o.wrong);
    }

    /// Whether every counter is at least `o`'s.
    fn covers(&self, o: &Counts) -> bool {
        self.not_loaded >= o.not_loaded && self.lost >= o.lost && self.wrong >= o.wrong
    }

    /// The sum of the three counters, saturated.
    pub fn total(&self) -> u64 {
        self.not_loaded
            .saturating_add(self.lost)
            .saturating_add(self.wrong)
    }
}

/// A row's key. The derived order is the output order: day, actor, run (none first), ruleset.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub day: Day,
    pub actor: String,
    pub run: Option<String>,
    pub ruleset: String,
}

/// Rows by key.
pub type Rows = BTreeMap<Key, Counts>;

/// A recorder's totals over the retained days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    /// Counts on today's UTC day.
    pub today: Counts,
    /// Counts over every retained day.
    pub total: Counts,
    /// Distinct sessions (actor and run) with a row on today's day.
    pub today_sessions: u64,
    /// Distinct sessions (actor and run) over every retained day.
    pub sessions: u64,
}

/// The summary of `rows` when `today` is the current day. Rows outside the retained days are
/// ignored.
pub fn summarize(rows: &Rows, today: Day) -> Summary {
    let mut s = Summary::default();
    let mut all: BTreeSet<(&str, Option<&str>)> = BTreeSet::new();
    let mut now: BTreeSet<(&str, Option<&str>)> = BTreeSet::new();
    for (k, c) in rows {
        if !k.day.retained_at(today) || c.total() == 0 {
            continue;
        }
        s.total.add(c);
        all.insert((&k.actor, k.run.as_deref()));
        if k.day == today {
            s.today.add(c);
            now.insert((&k.actor, k.run.as_deref()));
        }
    }
    s.sessions = all.len() as u64;
    s.today_sessions = now.len() as u64;
    s
}

/// The one summary line of `rdy status` and `rdy_overview`, over every retained day, or `None`
/// when every count is zero. Every caller uses this formatter.
pub fn summary_line(s: &Summary) -> Option<String> {
    let t = &s.total;
    (t.total() > 0).then(|| {
        format!(
            "kungfu: {} not loaded, {} lost, {} wrong, in {} sessions.",
            t.not_loaded, t.lost, t.wrong, s.sessions
        )
    })
}

/// The retained rows as JSON objects with exactly the file's row fields, in key order.
pub fn rows_json(rows: &Rows, today: Day) -> Vec<serde_json::Value> {
    rows.iter()
        .filter(|(k, c)| k.day.retained_at(today) && c.total() > 0)
        .map(|(k, c)| {
            serde_json::json!({
                "day": k.day.to_string(),
                "actor": k.actor,
                "run": k.run,
                "ruleset": k.ruleset,
                "not_loaded": c.not_loaded,
                "lost": c.lost,
                "wrong": c.wrong,
            })
        })
        .take(MAX_ROWS)
        .collect()
}

/// The rows a status shows for the project at `rdy_dir`: the process recorder's memory snapshot
/// when one exists, else one bounded best-effort read of the count file. It never waits for a
/// flush or a lock, never makes a recorder, and never fails: any problem reads as no rows.
pub fn status_rows(rdy_dir: &Path) -> Rows {
    let Ok(dir) = pio::open_dir(rdy_dir) else {
        return Rows::new();
    };
    if let Ok(id) = FileId::of_file(&dir) {
        let found = registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .and_then(Weak::upgrade);
        if let Some(h) = found {
            return Recorder(h).rows();
        }
    }
    let today = Day::today();
    let Ok(run) = pio::open_dir_at(&dir, OsStr::new(RUN_DIR)) else {
        return Rows::new();
    };
    if !is_private(&run).unwrap_or(false) {
        return Rows::new();
    }
    let mut rows = read_counts(&run, today).unwrap_or_default();
    retain(&mut rows, today);
    rows
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFile {
    schema: u64,
    rows: Vec<WireRow>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRow {
    day: String,
    actor: String,
    #[serde(deserialize_with = "required_option")]
    run: Option<String>,
    ruleset: String,
    not_loaded: u64,
    lost: u64,
    wrong: u64,
}

/// A `run` field that must be present: null or a string. (A plain `Option` field would default
/// to none when the key is missing.)
fn required_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}

/// Whether the JSON text nests deeper than [`MAX_DEPTH`]. Strings are skipped; nothing is
/// allocated.
fn too_deep(bytes: &[u8]) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &c in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// The rows of a count file read on `today`, or `None` for a damaged file: too big, too deep,
/// not valid JSON, not the closed object, a bad identity or day, or a repeated key. Days outside
/// the retained window (expired or in the future) are dropped, not damage. Rows over
/// [`MAX_ROWS`] are dropped by [`cap_rows`].
pub fn parse_file(bytes: &[u8], today: Day) -> Option<Rows> {
    if bytes.len() as u64 > MAX_FILE_BYTES || too_deep(bytes) {
        return None;
    }
    let file: WireFile = serde_json::from_slice(bytes).ok()?;
    if file.schema != SCHEMA {
        return None;
    }
    let mut rows = Rows::new();
    let mut seen: BTreeSet<(String, String, Option<String>, String)> = BTreeSet::new();
    for r in file.rows {
        let day = Day::parse(&r.day)?;
        if !(is_actor(&r.actor) && r.run.as_deref().is_none_or(is_run) && is_name(&r.ruleset)) {
            return None;
        }
        // a repeated key is damage even when its day has expired
        if !seen.insert((
            r.day.clone(),
            r.actor.clone(),
            r.run.clone(),
            r.ruleset.clone(),
        )) {
            return None;
        }
        if !day.retained_at(today) {
            continue;
        }
        let key = Key {
            day,
            actor: r.actor,
            run: r.run,
            ruleset: r.ruleset,
        };
        let counts = Counts {
            not_loaded: r.not_loaded,
            lost: r.lost,
            wrong: r.wrong,
        };
        rows.insert(key, counts);
    }
    cap_rows(&mut rows);
    Some(rows)
}

/// The bytes of a count file: rows in key order. Callers cap the rows first ([`cap_rows`]).
pub fn to_bytes(rows: &Rows) -> Vec<u8> {
    let file = WireFile {
        schema: SCHEMA,
        rows: rows
            .iter()
            .map(|(k, c)| WireRow {
                day: k.day.to_string(),
                actor: k.actor.clone(),
                run: k.run.clone(),
                ruleset: k.ruleset.clone(),
                not_loaded: c.not_loaded,
                lost: c.lost,
                wrong: c.wrong,
            })
            .collect(),
    };
    // a closed struct of strings and integers always serializes
    serde_json::to_vec(&file).unwrap_or_default()
}

/// Drops the rows over [`MAX_ROWS`]: newest days stay first, then ascending key order within a
/// day. The result is the same on every run.
pub fn cap_rows(rows: &mut Rows) {
    if rows.len() <= MAX_ROWS {
        return;
    }
    let mut keys: Vec<&Key> = rows.keys().collect();
    keys.sort_by(|a, b| b.day.cmp(&a.day).then_with(|| a.cmp(b)));
    let drop: Vec<Key> = keys.split_off(MAX_ROWS).into_iter().cloned().collect();
    for k in drop {
        rows.remove(&k);
    }
}

/// Keeps the retained days of `rows` on `today`, then applies the row cap.
fn retain(rows: &mut Rows, today: Day) {
    rows.retain(|k, _| k.day.retained_at(today));
    cap_rows(rows);
}

/// Adds every row of `src` into `dst`, saturating.
fn merge_into(dst: &mut Rows, src: &Rows) {
    for (k, c) in src {
        dst.entry(k.clone()).or_default().add(c);
    }
}

// ---------------------------------------------------------------------------------------------
// Test hooks
// ---------------------------------------------------------------------------------------------

/// What an injected fault does to one flush's replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// No fault.
    None,
    /// The write fails before the replacement starts. Nothing reached the file.
    BeforeReplace,
    /// The replacement completes, then reports a failure (an uncertain post-rename sync).
    AfterReplace,
    /// The replacement reports a failure and the file is unchanged.
    LostReplace,
}

/// How one worker cycle ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cycle {
    /// The file was read; nothing was written.
    Loaded,
    /// The pending counts reached the file.
    Flushed,
    /// Another holder has the file lock. The counts stay pending.
    Contended,
    /// A flush failed before the replacement. The counts stay pending.
    Failed,
    /// The replacement failed with its outcome unknown. The next flush reads the file first.
    Uncertain,
    /// The worker ended after its final flush attempt.
    Stopped,
}

type ClockFn = Arc<dyn Fn() -> i64 + Send + Sync>;
type GateFn = Arc<dyn Fn() + Send + Sync>;
type FaultFn = Arc<dyn Fn() -> Fault + Send + Sync>;
type CycleFn = Arc<dyn Fn(Cycle) + Send + Sync>;

/// Seams for tests: an injected clock (seconds since the Unix epoch), a gate called by the worker
/// after it takes its snapshot and before it touches the disk, an injected replacement fault, a
/// call at the end of each worker cycle, and the retry interval. A normal recorder has none.
#[doc(hidden)]
#[derive(Clone)]
pub struct Hooks {
    pub clock: Option<ClockFn>,
    pub before_flush: Option<GateFn>,
    pub fault: Option<FaultFn>,
    pub after_cycle: Option<CycleFn>,
    pub retry: Duration,
    pub poll: Duration,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            clock: None,
            before_flush: None,
            fault: None,
            after_cycle: None,
            retry: RETRY_INTERVAL,
            poll: POLL_INTERVAL,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The recorder
// ---------------------------------------------------------------------------------------------

/// A batch whose replacement failed with an unknown outcome. `expected` holds the merged value of
/// each of its keys: a file whose rows cover them has the batch in it already.
struct Carried {
    deltas: Rows,
    expected: Rows,
}

#[derive(Default)]
struct State {
    /// The file's rows as the worker last read or wrote them.
    disk: Rows,
    /// Deltas added since the last snapshot.
    pending: Rows,
    /// The snapshot the worker is flushing now.
    batch: Rows,
    carried: Option<Carried>,
    wake: bool,
    nudge: bool,
    stop: bool,
    dropped: u64,
    summary: Option<(Day, Summary)>,
}

struct Core {
    /// The opened `.rdy` directory: the project identity every disk call is anchored to.
    dir: File,
    run_path: PathBuf,
    state: Mutex<State>,
    cv: Condvar,
    hooks: Hooks,
}

/// The last owner of a recorder: dropping it stops the worker. It never waits for it.
struct Handle {
    core: Arc<Core>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        let mut st = self.core.lock();
        st.stop = true;
        drop(st);
        self.core.cv.notify_all();
    }
}

/// A shared handle to one project's count recorder. Clones share the recorder; the worker stops
/// after the last clone is dropped.
#[derive(Clone)]
pub struct Recorder(Arc<Handle>);

type Registry = Mutex<HashMap<FileId, Weak<Handle>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Recorder {
    /// The process's recorder for the project at `rdy_dir`, made on first use and shared by every
    /// caller after. It is bound to the identity of the opened `.rdy` directory: a copied or
    /// replaced directory gets its own recorder, and never an old worker's counts. Call it
    /// outside the request path; it opens one directory and starts one thread.
    pub fn for_project(rdy_dir: &Path) -> io::Result<Recorder> {
        Recorder::for_project_with(rdy_dir, Hooks::default())
    }

    #[doc(hidden)]
    pub fn for_project_with(rdy_dir: &Path, hooks: Hooks) -> io::Result<Recorder> {
        let dir = pio::open_dir(rdy_dir)?;
        let id = FileId::of_file(&dir)?;
        let mut reg = registry().lock().unwrap_or_else(PoisonError::into_inner);
        reg.retain(|_, w| w.strong_count() > 0);
        if let Some(h) = reg.get(&id).and_then(Weak::upgrade) {
            return Ok(Recorder(h));
        }
        let rec = Recorder::start(dir, rdy_dir.join(RUN_DIR), hooks)?;
        reg.insert(id, Arc::downgrade(&rec.0));
        Ok(rec)
    }

    /// A recorder outside the process registry, for tests that need independent recorders.
    #[doc(hidden)]
    pub fn detached(rdy_dir: &Path, hooks: Hooks) -> io::Result<Recorder> {
        Recorder::start(pio::open_dir(rdy_dir)?, rdy_dir.join(RUN_DIR), hooks)
    }

    fn start(dir: File, run_path: PathBuf, hooks: Hooks) -> io::Result<Recorder> {
        let core = Arc::new(Core {
            dir,
            run_path,
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            hooks,
        });
        let worker = Arc::clone(&core);
        std::thread::Builder::new()
            .name("rdy-kungfu-counts".into())
            .spawn(move || worker.run())?;
        Ok(Recorder(Arc::new(Handle { core })))
    }

    /// Whether both handles are one recorder.
    pub fn same_recorder(&self, other: &Recorder) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Counts one miss for the session (`actor`, `run`) against the owning `ruleset`. An identity
    /// outside the actor, run and name grammars is not counted. Nothing here touches the disk or
    /// waits on a lock held across disk work. Returns whether the miss was counted: a miss is
    /// lost when its identity is invalid or [`MAX_PENDING_ROWS`] distinct rows are waiting.
    pub fn record(&self, actor: &str, run: Option<&str>, ruleset: &str, case: Case) -> bool {
        if !(is_actor(actor) && run.is_none_or(is_run) && is_name(ruleset)) {
            return false;
        }
        let core = &self.0.core;
        let key = Key {
            day: core.today(),
            actor: actor.to_owned(),
            run: run.map(str::to_owned),
            ruleset: ruleset.to_owned(),
        };
        let mut st = core.lock();
        if let Some(c) = st.pending.get_mut(&key) {
            c.bump(case);
        } else if st.pending.len() >= MAX_PENDING_ROWS {
            st.dropped = st.dropped.saturating_add(1);
            return false;
        } else {
            let mut c = Counts::default();
            c.bump(case);
            st.pending.insert(key, c);
        }
        st.summary = None;
        st.wake = true;
        drop(st);
        core.cv.notify_one();
        true
    }

    /// The totals over the retained days: the file's rows as last read, plus every count not yet
    /// flushed. The result is cached until a count or the file changes or the day turns.
    pub fn summary(&self) -> Summary {
        let core = &self.0.core;
        let today = core.today();
        let mut st = core.lock();
        if let Some((day, s)) = st.summary
            && day == today
        {
            return s;
        }
        let s = summarize(&st.view(), today);
        st.summary = Some((today, s));
        s
    }

    /// Every retained row, as the file holds it now plus every count not yet flushed.
    pub fn rows(&self) -> Rows {
        let core = &self.0.core;
        let today = core.today();
        let mut rows = core.lock().view();
        retain(&mut rows, today);
        rows
    }

    /// The misses lost to the pending-row bound.
    pub fn dropped(&self) -> u64 {
        self.0.core.lock().dropped
    }

    /// Wakes the worker as if a count had arrived, past any retry wait.
    #[doc(hidden)]
    pub fn nudge(&self) {
        let core = &self.0.core;
        core.lock().nudge = true;
        core.cv.notify_all();
    }

    /// Whether no count waits for the disk: nothing pending, in flight or uncertain.
    #[doc(hidden)]
    pub fn settled(&self) -> bool {
        let st = self.0.core.lock();
        st.pending.is_empty() && st.batch.is_empty() && st.carried.is_none()
    }
}

impl State {
    /// The disk rows with every count that is not in them yet.
    fn view(&self) -> Rows {
        let mut rows = self.disk.clone();
        if let Some(c) = &self.carried {
            merge_into(&mut rows, &c.deltas);
        }
        merge_into(&mut rows, &self.batch);
        merge_into(&mut rows, &self.pending);
        rows
    }
}

/// How a flush attempt ended before or after the replacement.
enum Fail {
    /// The lock is held elsewhere.
    Contended,
    /// A failure before the replacement. `disk` holds the rows read, if the read happened, and
    /// `landed` says that the read proved an uncertain batch is in the file.
    Before { disk: Option<Rows>, landed: bool },
    /// The replacement failed with its outcome unknown.
    Replace { carried: Carried },
}

struct Done {
    merged: Rows,
}

impl Core {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn today(&self) -> Day {
        match &self.hooks.clock {
            Some(clock) => Day::from_unix_secs(clock()),
            None => Day::today(),
        }
    }

    fn report(&self, cycle: Cycle) {
        if let Some(f) = &self.hooks.after_cycle {
            f(cycle);
        }
    }

    /// The worker: load the file, then flush after each wake until the last handle is gone.
    fn run(self: Arc<Self>) {
        let first = self.cycle();
        self.report(first);
        let mut retry_at: Option<Instant> = self.retry_after(first);
        let mut poll_at = Instant::now() + self.hooks.poll;
        loop {
            let mut st = self.lock();
            loop {
                if st.stop || st.nudge {
                    break;
                }
                let now = Instant::now();
                let due = st.wake && retry_at.is_none_or(|t| now >= t);
                // an idle worker re-reads the file; one with counts waiting follows its retry
                if due || (!st.wake && now >= poll_at) {
                    break;
                }
                let until = match retry_at {
                    Some(t) if st.wake => t,
                    _ if st.wake => {
                        st = self.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
                        continue;
                    }
                    _ => poll_at,
                };
                st = self
                    .cv
                    .wait_timeout(st, until.saturating_duration_since(now))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            let stopping = st.stop;
            st.wake = false;
            st.nudge = false;
            drop(st);
            let out = self.cycle();
            self.report(out);
            if stopping {
                self.report(Cycle::Stopped);
                return;
            }
            retry_at = self.retry_after(out);
            poll_at = Instant::now() + self.hooks.poll;
        }
    }

    /// After a failed cycle: keep the counts due, and set the earliest retry.
    fn retry_after(&self, out: Cycle) -> Option<Instant> {
        match out {
            Cycle::Contended | Cycle::Failed | Cycle::Uncertain => {
                self.lock().wake = true;
                Some(Instant::now() + self.hooks.retry)
            }
            _ => None,
        }
    }

    /// One pass. With nothing to write it reads the file; otherwise it flushes a snapshot.
    fn cycle(&self) -> Cycle {
        let today = self.today();
        {
            let mut st = self.lock();
            let taken = std::mem::take(&mut st.pending);
            merge_into(&mut st.batch, &taken);
            if st.batch.is_empty() && st.carried.is_none() {
                drop(st);
                return self.refresh(today);
            }
        }
        if let Some(gate) = &self.hooks.before_flush {
            gate();
        }
        let (batch, carried) = {
            let st = self.lock();
            (
                st.batch.clone(),
                st.carried
                    .as_ref()
                    .map(|c| (c.deltas.clone(), c.expected.clone())),
            )
        };
        let outcome = self.flush(today, &batch, carried.as_ref());
        let mut st = self.lock();
        st.summary = None;
        match outcome {
            Ok(done) => {
                st.disk = done.merged;
                st.batch.clear();
                st.carried = None;
                Cycle::Flushed
            }
            Err(fail) => {
                let (kind, landed, disk) = match fail {
                    Fail::Contended => (Cycle::Contended, false, None),
                    Fail::Before { disk, landed } => (Cycle::Failed, landed, disk),
                    Fail::Replace { carried } => {
                        st.batch.clear();
                        st.carried = Some(carried);
                        return Cycle::Uncertain;
                    }
                };
                if let Some(rows) = disk {
                    st.disk = rows;
                }
                if landed {
                    st.carried = None;
                }
                // the snapshot goes back to pending, kept within the row bound
                let back = std::mem::take(&mut st.batch);
                for (k, c) in back {
                    if let Some(p) = st.pending.get_mut(&k) {
                        p.add(&c);
                    } else if st.pending.len() >= MAX_PENDING_ROWS {
                        st.dropped = st.dropped.saturating_add(1);
                    } else {
                        st.pending.insert(k, c);
                    }
                }
                kind
            }
        }
    }

    /// Reads the file into the cached rows, without the lock: a replacement is one rename.
    fn refresh(&self, today: Day) -> Cycle {
        let read = self
            .open_run(false)
            .and_then(|run| run.map_or(Ok(Rows::new()), |run| read_counts(&run, today)));
        let mut st = self.lock();
        match read {
            Ok(rows) => {
                st.disk = rows;
                st.summary = None;
                Cycle::Loaded
            }
            // an unsafe or unreadable file is shown as empty; the next flush reports its failure
            Err(_) => Cycle::Loaded,
        }
    }

    /// Opens `.rdy/run` through the project handle. A directory that is not private, or a link,
    /// is an error. `create` makes a missing directory.
    fn open_run(&self, create: bool) -> io::Result<Option<File>> {
        let name = OsStr::new(RUN_DIR);
        let opened = match pio::open_dir_at(&self.dir, name) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                match pio::create_private_dir_at(&self.dir, name) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e),
                }
                pio::open_dir_at(&self.dir, name)?
            }
            other => other?,
        };
        if !is_private(&opened)? {
            return Err(io::Error::other("the run directory is not private"));
        }
        Ok(Some(opened))
    }

    /// Merges `batch` (and an uncertain `carried` batch the file does not hold yet) into the file,
    /// under the lock file's lock.
    fn flush(
        &self,
        today: Day,
        batch: &Rows,
        carried: Option<&(Rows, Rows)>,
    ) -> Result<Done, Fail> {
        let before = |_: io::Error| Fail::Before {
            disk: None,
            landed: false,
        };
        let run = self
            .open_run(true)
            .map_err(before)?
            .ok_or_else(|| before(io::Error::from(io::ErrorKind::NotFound)))?;
        let lock = open_lock(&run).map_err(before)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Fail::Contended),
            Err(TryLockError::Error(e)) => return Err(before(e)),
        }
        let _held = Held(lock);
        let mut disk = read_counts(&run, today).map_err(before)?;
        let landed = carried.is_some_and(|(_, expected)| {
            expected
                .iter()
                .filter(|(k, _)| k.day.retained_at(today))
                .all(|(k, want)| disk.get(k).is_some_and(|have| have.covers(want)))
        });
        let mut deltas = batch.clone();
        if let (Some((old, _)), false) = (carried, landed) {
            merge_into(&mut deltas, old);
        }
        let mut merged = disk.clone();
        merge_into(&mut merged, &deltas);
        retain(&mut merged, today);
        retain(&mut disk, today);
        let fault = self.hooks.fault.as_ref().map_or(Fault::None, |f| f());
        if fault == Fault::BeforeReplace {
            return Err(Fail::Before {
                disk: Some(disk),
                landed,
            });
        }
        let expected: Rows = deltas
            .keys()
            .filter_map(|k| merged.get(k).map(|c| (k.clone(), *c)))
            .collect();
        let bytes = to_bytes(&merged);
        let replaced = if fault == Fault::LostReplace {
            Err(io::Error::other("injected lost replacement"))
        } else {
            fsutil::replace_in_dir(&run, &self.run_path, OsStr::new(FILE_NAME), &bytes, None)
        };
        let replaced = match (fault, replaced) {
            (Fault::AfterReplace, Ok(())) => Err(io::Error::other("injected sync failure")),
            (_, r) => r,
        };
        match replaced {
            Ok(()) => Ok(Done { merged }),
            Err(_) => Err(Fail::Replace {
                carried: Carried { deltas, expected },
            }),
        }
    }
}

/// A held lock file. Dropping it unlocks explicitly: the lock belongs to the open file
/// description, which a child process shares until it execs.
struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether this account owns the open file or directory `f` and it is private, read from the
/// handle.
fn is_private(f: &File) -> io::Result<bool> {
    let privacy = pio::privacy(f)?;
    Ok(privacy.mine && privacy.private)
}

/// Opens the lock file in the private run directory, making it when absent. A link, a file that
/// is not private or a file with other links is refused.
fn open_lock(run: &File) -> io::Result<File> {
    let name = OsStr::new(LOCK_NAME);
    let file = match pio::open_read_at(run, name) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => match pio::create_private_at(run, name) {
            Ok(f) => {
                pio::make_private_file(&f)?;
                f
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => pio::open_read_at(run, name)?,
            Err(e) => return Err(e),
        },
        Err(e) => return Err(e),
    };
    let links = fileid::links_of(&file, Path::new(LOCK_NAME))?;
    if !file.metadata()?.is_file() || !is_private(&file)? || links != 1 {
        return Err(io::Error::other("the count lock file is not private"));
    }
    Ok(file)
}

/// Reads the count file of the run directory. A missing file and a damaged or oversized regular
/// file read as empty; a link or a file that is not regular is an error. The file is closed when
/// this returns.
fn read_counts(run: &File, today: Day) -> io::Result<Rows> {
    let mut file = match pio::open_read_at(run, OsStr::new(FILE_NAME)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Rows::new()),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("the count file is not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Ok(Rows::new());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok(parse_file(&bytes, today).unwrap_or_default())
}
