//! kfu5 (FW-MH.kungfu, D-Y18E1): `rdy status` shows the local kungfu miss counts. One formatter
//! gives the one summary line, `--json` adds the retained daily rows, and nothing is shown when
//! every count is zero. A damaged count file never fails the status, and the counts stay out of the
//! store, a dump and a clone.
use rdy_engine::kungfu_counts::{
    self as kc, Counts, Day, Hooks, Key, MAX_FILE_BYTES, MAX_ROWS, Recorder, Rows,
};
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(60);

fn counts_path(root: &Path) -> PathBuf {
    root.join(".rdy/run").join(kc::FILE_NAME)
}

fn counts(not_loaded: u64, lost: u64, wrong: u64) -> Counts {
    Counts {
        not_loaded,
        lost,
        wrong,
    }
}

fn key(day: Day, actor: &str, run: Option<&str>, ruleset: &str) -> Key {
    Key {
        day,
        actor: actor.into(),
        run: run.map(Into::into),
        ruleset: ruleset.into(),
    }
}

/// Makes the private run directory through the public recorder, then puts `rows` in the count
/// file as they are. The recorder's own count is overwritten.
fn seed(root: &Path, rows: &Rows) {
    let rec = Recorder::detached(&root.join(".rdy"), Hooks::default()).expect("a recorder");
    rec.record("agent:seed", None, "read", kc::Case::Lost);
    let start = Instant::now();
    while !rec.settled() {
        assert!(start.elapsed() < DEADLINE, "the seed count never settled");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(rec);
    std::fs::write(counts_path(root), kc::to_bytes(rows)).expect("write the count file");
}

/// The rows the text and json tests share: three live sessions over two days, one zero row, and
/// one expired day.
fn sample() -> Rows {
    let today = Day::today();
    let mut rows = Rows::new();
    rows.insert(key(today, "agent:a1", Some("r1"), "write"), counts(2, 1, 0));
    rows.insert(key(today, "agent:a1", Some("r1"), "read"), counts(1, 0, 0));
    rows.insert(key(today, "agent:a2", None, "bootstrap"), counts(0, 2, 1));
    rows.insert(
        key(today.minus(5), "agent:a3", Some("r9"), "approvals"),
        counts(1, 0, 0),
    );
    rows.insert(key(today, "agent:a4", Some("r1"), "write"), counts(0, 0, 0));
    rows.insert(
        key(today.minus(30), "agent:a9", Some("old"), "write"),
        counts(7, 7, 7),
    );
    rows
}

const LINE: &str = "kungfu: 4 not loaded, 3 lost, 1 wrong, in 3 sessions.";

fn status_json(s: &Served) -> Value {
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    v
}

fn status_text(s: &Served) -> String {
    let out = s.rdy(&["status"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn kfu5_status_and_overview_share_one_line() {
    let s = Served::new();
    // zero omission: no file, then a file whose rows are all zero
    let text = status_text(&s);
    assert!(!text.contains("kungfu"), "{text}");
    let v = status_json(&s);
    assert!(
        v.get("kungfu").is_none() && v.get("kungfu_counts").is_none(),
        "{v}"
    );
    let mut zero = Rows::new();
    zero.insert(key(Day::today(), "agent:z", None, "write"), counts(0, 0, 0));
    seed(s.dir.path(), &zero);
    let v = status_json(&s);
    assert!(
        v.get("kungfu").is_none() && v.get("kungfu_counts").is_none(),
        "{v}"
    );
    assert!(!status_text(&s).contains("kungfu"));

    // retained-day totals: today and the previous 29 days; the expired day and the zero row
    // add nothing, and a session counts once across days and owners
    seed(s.dir.path(), &sample());
    let text = status_text(&s);
    assert_eq!(text.matches("kungfu").count(), 1, "one line only: {text}");
    assert_eq!(
        text.lines().filter(|l| *l == LINE).count(),
        1,
        "exact text: {text}"
    );
    let v = status_json(&s);
    assert_eq!(v["kungfu"], LINE, "{v}");

    // the overview of a server in the same project carries the same string; the daemon lets
    // the store go first
    s.stop();
    let mut c = rdy_qa::mcp::McpClient::start(s.dir.path());
    let opened = c.call(
        "rdy_session",
        json!({"actor": "agent:ov", "run": "o1", "role": "writer"}),
    );
    assert!(opened.is_ok(), "{opened:?}");
    let start = Instant::now();
    let overview = loop {
        let o = c.call("rdy_overview", json!({})).ok();
        if o["kungfu"] == LINE {
            break o;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "the overview never showed {LINE}: {o}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(overview["kungfu"], v["kungfu"]);
    assert!(
        overview.get("kungfu_counts").is_none(),
        "rows stay out of the overview"
    );
}

#[test]
fn kfu5_status_json_gives_bounded_daily_rows() {
    let s = Served::new();
    let today = Day::today();
    seed(s.dir.path(), &sample());
    let v = status_json(&s);
    let d5 = today.minus(5).to_string();
    let d0 = today.to_string();
    // oldest day first; within a day by actor, run (none first), owner; no zero and no expired
    let want = json!([
        {"day": d5, "actor": "agent:a3", "run": "r9", "ruleset": "approvals",
         "not_loaded": 1, "lost": 0, "wrong": 0},
        {"day": d0, "actor": "agent:a1", "run": "r1", "ruleset": "read",
         "not_loaded": 1, "lost": 0, "wrong": 0},
        {"day": d0, "actor": "agent:a1", "run": "r1", "ruleset": "write",
         "not_loaded": 2, "lost": 1, "wrong": 0},
        {"day": d0, "actor": "agent:a2", "run": null, "ruleset": "bootstrap",
         "not_loaded": 0, "lost": 2, "wrong": 1},
    ]);
    assert_eq!(v["kungfu_counts"], want, "{v}");
    for row in v["kungfu_counts"].as_array().unwrap() {
        let mut names: Vec<&String> = row.as_object().unwrap().keys().collect();
        names.sort();
        assert_eq!(
            names,
            [
                "actor",
                "day",
                "lost",
                "not_loaded",
                "ruleset",
                "run",
                "wrong"
            ]
        );
    }
    // no value of any ruleset
    assert!(!v.to_string().contains("kf-"), "{v}");
    let again = status_json(&s);
    assert_eq!(again["kungfu_counts"], want, "deterministic order");

    // bounded: more distinct rows than the cap read back as the cap, newest days first, and
    // each actor/run/owner/day key appears once
    let mut many = Rows::new();
    for i in 0..(MAX_ROWS + 100) {
        let day = if i < 100 { today.minus(1) } else { today };
        many.insert(
            key(day, &format!("agent:m{i:05}"), Some("r"), "read"),
            counts(1, 0, 0),
        );
    }
    seed(s.dir.path(), &many);
    let v = status_json(&s);
    let rows = v["kungfu_counts"].as_array().unwrap();
    assert_eq!(rows.len(), MAX_ROWS);
    assert!(
        rows.iter().all(|r| r["day"] == d0.as_str()),
        "newest days first"
    );
    let mut seen = std::collections::BTreeSet::new();
    for r in rows {
        let k = (
            r["day"].to_string(),
            r["actor"].to_string(),
            r["run"].to_string(),
            r["ruleset"].to_string(),
        );
        assert!(seen.insert(k), "a repeated key: {r}");
    }
}

fn tree_bytes(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            tree_bytes(&p, out);
        } else if let Ok(b) = std::fs::read(&p) {
            out.push((p, b));
        }
    }
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// A guard: the counts were never part of the store or a dump, so these hold on first run.
#[test]
fn kfu5_counts_stay_out_of_store_dump_and_clone() {
    let s = Served::new();
    let marker = "agent:kfu5marker";
    let mut rows = Rows::new();
    rows.insert(
        key(Day::today(), marker, Some("kfu5run"), "write"),
        counts(3, 0, 0),
    );
    // flush through a real recorder first, so the counts are on disk as a server leaves them
    {
        let rec = Recorder::detached(&s.dir.path().join(".rdy"), Hooks::default()).unwrap();
        assert!(rec.record(marker, Some("kfu5run"), "write", kc::Case::NotLoaded));
        let start = Instant::now();
        while !rec.settled() {
            assert!(start.elapsed() < DEADLINE, "the count never settled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    seed(s.dir.path(), &rows);
    assert_eq!(
        status_json(&s)["kungfu"],
        "kungfu: 3 not loaded, 0 lost, 0 wrong, in 1 sessions."
    );

    // the store holds no count
    let mut files = vec![];
    tree_bytes(&s.dir.path().join(".rdy/store"), &mut files);
    assert!(!files.is_empty());
    for (p, b) in &files {
        assert!(!holds(b, marker) && !holds(b, "kungfu"), "{}", p.display());
    }

    // the dump holds none; a load and a clone from it carry none
    let scratch = short_tempdir();
    let file = scratch.path().join("p.rdydump");
    let o = s.rdy(&["dump", "--out", file.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let dump = std::fs::read(&file).unwrap();
    assert!(!holds(&dump, marker) && !holds(&dump, "kungfu"));
    s.stop();
    let env = s.env_pairs();
    let loaded = scratch.path().join("loaded");
    std::fs::create_dir(&loaded).unwrap();
    let out = human_command(
        &loaded,
        &env,
        &["--no-daemon", "load", file.to_str().unwrap()],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cloned = scratch.path().join("cloned");
    let out = human_command(
        scratch.path(),
        &env,
        &[
            "--no-daemon",
            "clone",
            file.to_str().unwrap(),
            cloned.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (what, dir) in [("load", &loaded), ("clone", &cloned)] {
        assert!(!counts_path(dir).exists(), "{what} carried the count file");
        let mut all = vec![];
        tree_bytes(&dir.join(".rdy"), &mut all);
        for (p, b) in &all {
            assert!(!holds(b, marker), "{what}: {}", p.display());
        }
        let out = human_command(dir, &env, &["--no-daemon", "--json", "status"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{what}");
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            v.get("kungfu").is_none() && v.get("kungfu_counts").is_none(),
            "{what}: {v}"
        );
    }
}

/// A guard: a bad count file was always read as empty, so the first run passes.
#[test]
fn kfu5_bad_counts_do_not_fail_status() {
    let s = Served::new();
    let root = s.dir.path();
    let path = counts_path(root);
    let good = {
        let mut rows = Rows::new();
        rows.insert(key(Day::today(), "agent:g", None, "write"), counts(1, 0, 0));
        kc::to_bytes(&rows)
    };
    seed(root, &Rows::new());
    let plain = |s: &Served| {
        let v = status_json(s);
        assert!(
            v.get("kungfu").is_none() && v.get("kungfu_counts").is_none(),
            "{v}"
        );
        assert!(!status_text(s).contains("kungfu"));
    };

    // missing
    std::fs::remove_file(&path).unwrap();
    plain(&s);
    // corrupt
    std::fs::write(&path, b"{\"schema\": 1, \"rows\": [ not json").unwrap();
    plain(&s);
    std::fs::write(&path, b"{\"schema\": 2, \"rows\": []}").unwrap();
    plain(&s);
    // oversized: one byte over the cap, with valid rows at its start
    let mut big = good.clone();
    big.resize(MAX_FILE_BYTES as usize + 1, b' ');
    std::fs::write(&path, &big).unwrap();
    plain(&s);
    // inaccessible: a directory where the file belongs
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    plain(&s);
    std::fs::remove_dir(&path).unwrap();

    // concurrently replaced: a good file and a bad one swap while status runs. Every status
    // succeeds and shows the line or nothing.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flipper = {
        let stop = stop.clone();
        let path = path.clone();
        let tmp = root.join(".rdy/run/flip.tmp");
        let good = good.clone();
        std::thread::spawn(move || {
            let mut bad_next = true;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let bytes: &[u8] = if bad_next { b"garbage" } else { &good };
                if std::fs::write(&tmp, bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                bad_next = !bad_next;
            }
        })
    };
    for _ in 0..8 {
        let v = status_json(&s);
        match v.get("kungfu") {
            None => assert!(v.get("kungfu_counts").is_none(), "{v}"),
            Some(line) => assert_eq!(
                line, "kungfu: 1 not loaded, 0 lost, 0 wrong, in 1 sessions.",
                "{v}"
            ),
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    flipper.join().unwrap();
}
