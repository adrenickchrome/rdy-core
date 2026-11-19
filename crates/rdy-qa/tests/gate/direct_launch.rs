//! The gate launches area binaries directly after its one `cargo test --no-run` build: each
//! binary gets the working directory and environment `cargo test` would give it, and the
//! SERIAL_AREAS never run beside another area.
use rdy_qa::gate_steps::{
    AreaEnv, ENV_PROBE_ARGS, ENV_PROBE_BINARY, ENV_PROBE_TEST, ENV_PROBE_VAR, PILOT_AREAS,
    SERIAL_AREAS, area_jobs, capture_area_env, cargo, direct_areas, launch_area, parse_artifacts,
    parse_env_probe, run_areas, run_bounded,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Runs a Cargo command from the workspace root and fails the test when it fails.
fn cargo_ok(mut c: std::process::Command) -> String {
    c.current_dir(root());
    let out = run_bounded(c);
    assert!(out.ok, "{}", out.combined());
    out.stdout
}

fn probe_record(file: &Path) -> AreaEnv {
    let text = std::fs::read_to_string(file).expect("the probe wrote its record");
    parse_env_probe(&text).expect("probe record")
}

/// The variables that differ between two environments, with both values.
fn differences(a: &AreaEnv, b: &AreaEnv) -> Vec<String> {
    let keys: std::collections::BTreeSet<&String> = a.vars.keys().chain(b.vars.keys()).collect();
    keys.into_iter()
        .filter(|k| a.vars.get(*k) != b.vars.get(*k))
        .map(|k| format!("{k}: {:?} / {:?}", a.vars.get(k), b.vars.get(k)))
        .collect()
}

/// Under `cargo qa` the Cargo calls here see the same `CARGO_PKG_*` values as the gate's own
/// builds. Run from a plain shell they do not, and a build script that tracks those values (ring)
/// rebuilds once, with its dependents.
#[test]
fn direct_env_matches_cargo() {
    // the env probe's binary, as the gate's discovery names it
    let json = cargo_ok(cargo(&[
        "test",
        "-p",
        "rdy-qa",
        "--release",
        "--test",
        ENV_PROBE_BINARY,
        "--no-run",
        "--message-format=json",
    ]));
    let exe = parse_artifacts(&json)
        .into_iter()
        .find(|e| e.name == ENV_PROBE_BINARY)
        .expect("the model binary");
    let tmp = tempfile::tempdir().expect("tempdir");

    // the probe started directly, with the environment the gate asks of Cargo
    let mut env = capture_area_env(|mut c| {
        c.current_dir(root());
        run_bounded(c)
    })
    .expect("capture the area environment");
    let direct_file = tmp.path().join("direct.json");
    env.vars.insert(
        ENV_PROBE_VAR.to_string(),
        direct_file.to_string_lossy().into_owned(),
    );
    let run = launch_area(
        &exe.path,
        &env,
        &[ENV_PROBE_TEST, "--exact"],
        "env probe",
        Duration::from_secs(600),
    );
    assert!(run.out.ok, "{}", run.out.combined());
    let direct = probe_record(&direct_file);

    // the probe started by `cargo test`
    let cargo_file = tmp.path().join("cargo.json");
    let mut c = cargo(&ENV_PROBE_ARGS);
    c.env(ENV_PROBE_VAR, &cargo_file);
    cargo_ok(c);
    let by_cargo = probe_record(&cargo_file);

    assert_eq!(direct.cwd, by_cargo.cwd);
    assert!(
        by_cargo.cwd.ends_with(Path::new("crates").join("rdy-qa")),
        "{}",
        by_cargo.cwd.display()
    );
    let diff = differences(&direct, &by_cargo);
    assert!(
        diff.is_empty(),
        "the environments differ:\n{}",
        diff.join("\n")
    );
    // what the tests read from Cargo is there
    assert!(by_cargo.vars.contains_key("CARGO_MANIFEST_DIR"));
    assert!(by_cargo.vars.contains_key("CARGO_PKG_VERSION"));
    assert!(
        by_cargo
            .vars
            .keys()
            .any(|k| k.starts_with("CARGO_BIN_EXE_")),
        "no CARGO_BIN_EXE_ variable"
    );

    // This test itself runs under Cargo, so a child that inherited this process's directory and
    // variables would also match. The launched binary gets exactly the given ones: a variable
    // left out is absent, and the given directory is its directory.
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let mut other = env.clone();
    other.cwd = elsewhere.path().canonicalize().expect("canonical tempdir");
    other.vars.remove("CARGO_MANIFEST_DIR");
    let other_file = tmp.path().join("other.json");
    other.vars.insert(
        ENV_PROBE_VAR.to_string(),
        other_file.to_string_lossy().into_owned(),
    );
    let run = launch_area(
        &exe.path,
        &other,
        &[ENV_PROBE_TEST, "--exact"],
        "env probe",
        Duration::from_secs(600),
    );
    assert!(run.out.ok, "{}", run.out.combined());
    let seen = probe_record(&other_file);
    assert_eq!(
        seen.cwd.canonicalize().expect("canonical cwd"),
        other.cwd,
        "the directory is not the given one"
    );
    assert!(
        !seen.vars.contains_key("CARGO_MANIFEST_DIR"),
        "the binary inherited a variable it was not given"
    );
}

/// What the fake runs saw: the areas running now, the most at once, and each overlap.
#[derive(Default)]
struct Seen {
    running: Vec<String>,
    most: usize,
    overlaps: Vec<String>,
}

#[test]
fn serial_areas_never_overlap() {
    let areas = rdy_qa::report::test_areas(&root().join("crates/rdy-qa/tests"));
    // every serial and pilot name is a real area binary
    for name in SERIAL_AREAS.iter().chain(PILOT_AREAS) {
        assert!(areas.iter().any(|a| a == name), "{name} is not an area");
    }
    let alone = |a: &String| SERIAL_AREAS.contains(&a.as_str());
    let shared: Vec<&String> = areas.iter().filter(|a| !alone(a)).collect();
    assert!(shared.len() >= 2, "{areas:?}");
    // the first two shared areas meet each other while they run, which proves the workers run
    // side by side; every other area takes a fake time from its name
    let (a_tx, a_rx) = mpsc::channel::<()>();
    let (b_tx, b_rx) = mpsc::channel::<()>();
    let meet = Mutex::new(BTreeMap::from([
        (shared[0].clone(), (a_tx, b_rx)),
        (shared[1].clone(), (b_tx, a_rx)),
    ]));
    let met = Mutex::new(vec![]);
    let seen = Mutex::new(Seen::default());
    let jobs = 3;
    let results = run_areas(&areas, alone, jobs, |area| {
        {
            let mut s = seen.lock().unwrap();
            if !s.running.is_empty() && (alone(area) || s.running.iter().any(alone)) {
                let line = format!("{area} started beside {:?}", s.running);
                s.overlaps.push(line);
            }
            s.running.push(area.clone());
            s.most = s.most.max(s.running.len());
        }
        let pair = meet.lock().unwrap().remove(area);
        if let Some((tx, rx)) = pair {
            let _ = tx.send(());
            met.lock()
                .unwrap()
                .push(rx.recv_timeout(Duration::from_secs(30)).is_ok());
        } else {
            std::thread::sleep(Duration::from_millis(2 * area.len() as u64));
        }
        seen.lock().unwrap().running.retain(|r| r != area);
        format!("ran {area}")
    });
    let s = seen.into_inner().unwrap();
    assert!(s.overlaps.is_empty(), "{:#?}", s.overlaps);
    assert!(s.most <= jobs, "{} at once", s.most);
    assert_eq!(
        *met.lock().unwrap(),
        [true, true],
        "the shared areas ran one at a time"
    );
    // one result per area, in the plan's order
    let want: Vec<String> = areas.iter().map(|a| format!("ran {a}")).collect();
    assert_eq!(results, want);
}

#[test]
fn direct_launch_starts_with_the_pilot() {
    let files: Vec<String> = ["daemon", "engine", "model", "schema", "swarm"]
        .map(String::from)
        .to_vec();
    let names = |v: Option<&str>| direct_areas(v, &files).into_iter().collect::<Vec<_>>();
    assert_eq!(names(None), ["model", "schema"]);
    assert_eq!(names(Some("all")), files);
    assert!(names(Some("none")).is_empty());
    assert!(names(Some("")).is_empty());
    assert_eq!(names(Some(" engine, nope ,swarm")), ["engine", "swarm"]);
    // a quarter of the threads, from one to four
    assert_eq!(area_jobs(1), 1);
    assert_eq!(area_jobs(8), 2);
    assert_eq!(area_jobs(24), 4);
}
