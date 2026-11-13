//! rfr2 (D-MKSFF): the released-format compatibility matrix after `figure_dark`.
//!
//! - Every released dump, and the frozen first-use `figure_dark` dump, loads into this rdy and
//!   passes fsck; the released fixtures keep the bytes the Release 1 contract records; store
//!   format 19 and dump format 4 do not move.
//! - An older rdy refuses a store or dump that uses `figure_dark`. The static facts come from the
//!   predecessor contract (`contracts/m4bc-v1.json`, what rdy 0.50.0 registers) and the frozen
//!   members' own bytes. The run of an actual older binary is
//!   [`rfr2_old_binary_refuses_dark_store_and_dump`]: it needs `RDY_RELEASED_RDY`.
use rdy_engine::Rdy;
use rdy_engine::load::{load, load_chain};
use rdy_model::Hash;
use rdy_qa::foreign_fixture;
use rdy_store::dump::{DUMP_FORMAT, READS, READS_FEATURES, read_envelope};
use rdy_store::format::{DumpStore, FEATURE_FIGURE_DARK, FIGURE_DARK_REQUIRED_RDY};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The rdy before `figure_dark` (tag `v0.50.0`), whose registry the predecessor contract records.
const OLDER_RDY: &str = "0.50.0";
const DUMP: &str = "v4-f1-e1-figure-dark.rdydump";
const DUMP_EXPECT: &str = "v4-f1-e1-figure-dark.expect.json";
const NATIVE: &str = "store/figure-dark-1";

/// The new members and their blake3, set when they were written. A change needs a founder
/// decision; after publication none is allowed.
const MEMBERS: [(&str, &str, &str); 3] = [
    (
        "dump",
        DUMP,
        "a64524b4ae80f73235ae9b06b773b0e15cfe4ca34be859b81bdeb33474969aa0",
    ),
    (
        "dump",
        DUMP_EXPECT,
        "b6b68175c227aae5b869d55394ae6a939975241eb005391a4a2ae4a312e4b4c6",
    ),
    (
        "store/figure-dark-1",
        "expect.json",
        "c68069fa0dcca3507b7c2b8fb776963d941d511e1d1bc4c4d784531b27335604",
    ),
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn version_key(v: &str) -> (u64, u64, u64) {
    let p: Vec<u64> = v.split('.').map(|x| x.parse().unwrap()).collect();
    assert_eq!(p.len(), 3, "{v}");
    (p[0], p[1], p[2])
}

/// Every file of `dir` the contract's `digests` row names keeps its length and blake3.
fn released_digests_hold(dir: &Path, digests: &Value) -> usize {
    let digests = digests.as_object().unwrap();
    for (name, recorded) in digests {
        let bytes = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            &format!("{} {}", bytes.len(), Hash::of(&bytes)),
            recorded.as_str().unwrap(),
            "{name}: a released fixture never changes"
        );
    }
    digests.len()
}

/// One dump fixture to load: its name, whether it is a holder's, and its files.
struct Input {
    name: String,
    holder: bool,
    files: Vec<PathBuf>,
}

fn dump_inputs() -> Vec<Input> {
    let dir = fixtures().join("dump");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".rdydump"))
        .collect();
    names.sort();
    let mut out = vec![];
    for n in &names {
        let stem = n.trim_end_matches(".rdydump");
        let holder = stem.contains("holder");
        if let Some(prefix) = stem.strip_suffix("-chain-full") {
            out.push(Input {
                name: format!("{prefix}-chain"),
                holder,
                files: ["full", "1", "2"]
                    .iter()
                    .map(|p| dir.join(format!("{prefix}-chain-{p}.rdydump")))
                    .collect(),
            });
        } else if !(stem.ends_with("-chain-1") || stem.ends_with("-chain-2")) {
            out.push(Input {
                name: stem.to_string(),
                holder,
                files: vec![dir.join(n)],
            });
        }
    }
    out
}

/// rfr2: every released dump and the frozen `figure_dark` dump load into this rdy and pass
/// fsck; no released fixture or digest is rewritten; the dark dump's file and whole-load facts
/// are the ones its expect file records, the same store as the frozen native store.
#[test]
fn rfr2_every_released_dump_still_loads() {
    let r1 = read_json(&fixtures().join("contracts/release-1.json"));
    let dumps = released_digests_hold(
        &fixtures().join("dump"),
        &r1["dump_load"]["fixture_digests"],
    );
    assert!(dumps >= 70, "{dumps}");
    for (area, name, digest) in MEMBERS {
        let bytes = std::fs::read(fixtures().join(area).join(name))
            .unwrap_or_else(|e| panic!("{name}: the figure_dark members are frozen: {e}"));
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
    }

    let scratch = tempfile::tempdir().unwrap();
    let (mut loaded, mut dark) = (0, 0);
    for (i, input) in dump_inputs().iter().enumerate() {
        let facts = rdy_engine::formats::inspect_dump(&input.files[0])
            .unwrap_or_else(|e| panic!("{}: {e:?}", input.name));
        match &facts.store {
            DumpStore::Legacy(n) => assert!(READS.contains(&(facts.dump, *n)), "{}", input.name),
            DumpStore::Features(m) => {
                assert_eq!(facts.dump, DUMP_FORMAT, "{}", input.name);
                assert!(
                    READS_FEATURES.contains(&m.manifest_version),
                    "{}",
                    input.name
                );
                dark += usize::from(m.features.iter().any(|f| f.name == FEATURE_FIGURE_DARK));
            }
        }
        let to = scratch.path().join(format!("l{i}"));
        let done = if input.files.len() == 1 {
            load(&input.files[0], &to)
        } else {
            load_chain(&input.files, &to, None)
        };
        done.unwrap_or_else(|e| panic!("{}: {e:?}", input.name));
        let r = if input.holder {
            Rdy::open_holder(&to).unwrap()
        } else {
            Rdy::open(&to).unwrap()
        };
        r.fsck()
            .unwrap_or_else(|e| panic!("{}: fsck refused the load: {e:?}", input.name));
        r.close().unwrap();
        loaded += 1;
    }
    assert!(loaded >= 30, "{loaded}");
    assert_eq!(dark, 1, "one dump uses figure_dark");

    let file = fixtures().join("dump").join(DUMP);
    let expect = read_json(&fixtures().join("dump").join(DUMP_EXPECT));
    assert_eq!(expect["published"], json!(false));
    assert_eq!(foreign_fixture::dump_file_facts(&file), expect["file"]);
    assert_eq!(expect["file"]["dump_format"], json!(4));
    let fd = expect["file"]["store_format"]["features"]["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == FEATURE_FIGURE_DARK)
        .expect("the dump's manifest holds figure_dark")
        .clone();
    assert_eq!(fd["class"], "incompatible");
    assert_eq!(fd["required_rdy"], json!(FIGURE_DARK_REQUIRED_RDY));
    let whole = foreign_fixture::dump_load_facts(&file, &scratch.path().join("dark"));
    assert_eq!(whole, expect["whole"], "the whole load");
    let native = read_json(&fixtures().join(NATIVE).join("expect.json"));
    assert_eq!(
        whole["store"], native["store"],
        "the dump holds the native store"
    );
}

/// The manifest JSON of a `FEATURES` file's text (its second line).
fn features_of(text: &str) -> Value {
    let (first, rest) = text.split_once('\n').unwrap();
    assert_eq!(first, "rdy store features 1");
    serde_json::from_str(rest.trim_end()).unwrap()
}

/// rfr2: by the rules rdy 0.50.0 reads (the predecessor contract), the frozen native store and
/// dump use one feature it does not register, `figure_dark`, incompatible, with a minimum newer
/// than 0.50.0; sentinel 19 and dump format 4 do not move. An unknown incompatible feature
/// refuses an open and a load.
#[test]
fn rfr2_older_registry_does_not_know_figure_dark() {
    let older = read_json(&fixtures().join("contracts/m4bc-v1.json"));
    let registered: Vec<&str> = older["store"]["registered_names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(registered.len(), 8, "{registered:?}");
    assert!(!registered.contains(&FEATURE_FIGURE_DARK));
    assert!(version_key(FIGURE_DARK_REQUIRED_RDY) > version_key(OLDER_RDY));
    assert_eq!(
        std::fs::read_to_string(fixtures().join(NATIVE).join("FORMAT")).unwrap(),
        rdy_store::format::format_file_text()
    );
    let m =
        features_of(&std::fs::read_to_string(fixtures().join(NATIVE).join("FEATURES")).unwrap());
    let unknown: Vec<&Value> = m["features"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| !registered.contains(&f["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        unknown,
        [
            &json!({"name": "figure_dark", "class": "incompatible", "required_rdy": FIGURE_DARK_REQUIRED_RDY})
        ]
    );
    let mut f = std::fs::File::open(fixtures().join("dump").join(DUMP)).unwrap();
    let (format, header, _, _) = read_envelope(&mut f).unwrap();
    assert_eq!(format, 4);
    let DumpStore::Features(dm) = header.store else {
        panic!("a feature dump")
    };
    let unknown: Vec<_> = dm
        .features
        .iter()
        .filter(|f| !registered.contains(&f.name.as_str()))
        .map(|f| (f.name.as_str(), f.class.as_str(), f.required_rdy.as_str()))
        .collect();
    assert_eq!(
        unknown,
        [("figure_dark", "incompatible", FIGURE_DARK_REQUIRED_RDY)]
    );
}

fn bytes_under(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    foreign_fixture::files_under(dir)
        .into_iter()
        .filter(|f| !f.starts_with("store/index.redb"))
        .map(|f| {
            let b = std::fs::read(dir.join(&f)).unwrap();
            (f, b)
        })
        .collect()
}

/// Runs the released rdy `bin` with `args` in `dir`; returns whether it succeeded and its error
/// reply (`--json` puts it on stderr).
fn run_released(bin: &std::ffi::OsStr, dir: &Path, args: &[&str]) -> (bool, Value, String) {
    let out = std::process::Command::new(bin)
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("RDY_NO_DAEMON", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let text = format!("{}{stderr}", String::from_utf8_lossy(&out.stdout));
    let reply = stderr
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .unwrap_or(Value::Null);
    (out.status.success(), reply, text)
}

/// rfr2, end to end with an older rdy (`RDY_RELEASED_RDY`, the path of its binary: rdy 0.50.0,
/// tag `v0.50.0`, when this test was added). It refuses the frozen native store and the frozen
/// dump with `E_STORE_FORMAT`, naming `figure_dark` and its minimum rdy, changes no byte of the
/// store and writes no store for the refused load; it opens a store with no dark image. The
/// gate skips it; run it by hand: `RDY_RELEASED_RDY=<path> cargo test -p rdy-qa --release --test
/// release rfr2_old_binary -- --ignored`.
#[test]
#[ignore = "needs RDY_RELEASED_RDY, the path of an rdy binary older than figure_dark"]
fn rfr2_old_binary_refuses_dark_store_and_dump() {
    let bin = std::env::var_os("RDY_RELEASED_RDY").expect("RDY_RELEASED_RDY names a released rdy");
    let version = std::process::Command::new(&bin)
        .arg("--version")
        .output()
        .unwrap();
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    eprintln!(
        "older rdy: {version}, blake3 {}",
        Hash::of(&std::fs::read(&bin).unwrap())
    );
    let v = version.strip_prefix("rdy ").expect("rdy <version>");
    assert!(
        version_key(v) < version_key(FIGURE_DARK_REQUIRED_RDY),
        "an rdy older than {FIGURE_DARK_REQUIRED_RDY}: {version}"
    );
    let need = format!("store feature figure_dark needs rdy {FIGURE_DARK_REQUIRED_RDY}");
    let fix = format!("open the project with rdy {FIGURE_DARK_REQUIRED_RDY} or later");

    // the native store: refused at open, no byte changed
    let (_base, root) = foreign_fixture::project_copy(&fixtures().join(NATIVE));
    let before = bytes_under(&root.join(".rdy"));
    let (ok, reply, text) = run_released(&bin, &root, &["status", "--no-daemon"]);
    eprintln!("status: {text}");
    assert!(!ok, "{text}");
    assert_eq!(reply["code"], "E_STORE_FORMAT", "{text}");
    assert_eq!(reply["message"].as_str(), Some(need.as_str()), "{text}");
    assert_eq!(reply["fix"].as_str(), Some(fix.as_str()), "{text}");
    assert_eq!(bytes_under(&root.join(".rdy")), before, "no byte changed");

    // the dump: refused, and no store written
    let scratch = tempfile::tempdir().unwrap();
    let to = scratch.path().join("loaded");
    let dump = fixtures().join("dump").join(DUMP);
    let (ok, reply, text) = run_released(
        &bin,
        scratch.path(),
        &["load", dump.to_str().unwrap(), to.to_str().unwrap()],
    );
    eprintln!("load: {text}");
    assert!(!ok, "{text}");
    assert_eq!(reply["code"], "E_STORE_FORMAT", "{text}");
    assert_eq!(reply["message"].as_str(), Some(need.as_str()), "{text}");
    assert!(
        !to.join(".rdy/store").exists(),
        "the refused load wrote a store"
    );

    // the control: a store this rdy wrote with no dark image opens in the older rdy
    let (control, r) = rdy_qa::golden::tiny_with_doc();
    r.close().unwrap();
    let (ok, _, text) = run_released(&bin, control.path(), &["status", "--no-daemon"]);
    eprintln!("control status: {text}");
    assert!(ok, "{text}");
}
