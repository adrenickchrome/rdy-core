//! relc2: the frozen Tally sample. The ordinary gate loads the checked-in dump
//! (`samples/tally/tally.rdydump`) and checks it against its freeze record; it never builds a new
//! one. Every scenario loads the frozen bytes into a new directory with an isolated home, and the
//! first session runs through the real stdio MCP path of the `rdy` under test.
use rdy_qa::fixtures::{output_within, rdy_bin};
use rdy_qa::release_sample::{
    self, AGENT_ACTOR, BUILD_LIST_MAX, DEPTH_MAX, Frozen, HUMAN_ACTOR, NODES_MAX, STRING_MAX,
    Sample, SampleFailure,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

/// The hang guard of this file's own helper runs.
const GUARD: Duration = Duration::from_secs(600);

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/tally")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sha256(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The checked-in sample, with `work` as the check's new directory.
fn sample(work: &Path) -> Sample {
    Sample {
        rdy: rdy_bin(),
        dump: samples().join("tally.rdydump"),
        freeze: samples().join("freeze.json"),
        build_list: samples().join("build.json"),
        work: work.to_path_buf(),
    }
}

/// The checked-in build list as JSON.
fn build_list() -> Value {
    serde_json::from_slice(&std::fs::read(samples().join("build.json")).unwrap()).unwrap()
}

fn parsed(v: &Value) -> Result<release_sample::BuildList, SampleFailure> {
    release_sample::parse_build_list(&serde_json::to_vec(v).unwrap())
}

fn message(r: Result<release_sample::BuildList, SampleFailure>) -> String {
    match r {
        Ok(_) => panic!("the build list was accepted"),
        Err(f) => f.message(),
    }
}

/// Runs `qa-release-sample` with `args`.
fn helper(args: &[&std::ffi::OsStr]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-release-sample"));
    cmd.args(args);
    output_within(&mut cmd, GUARD).expect("run qa-release-sample")
}

/// `qa-release-sample check` over `dump` and `freeze`, with the checked-in build list.
fn check_cli(dump: &Path, freeze: &Path, work: &Path) -> Output {
    let rdy = rdy_bin();
    let list = samples().join("build.json");
    helper(&[
        "check".as_ref(),
        "--rdy".as_ref(),
        rdy.as_os_str(),
        "--dump".as_ref(),
        dump.as_os_str(),
        "--freeze".as_ref(),
        freeze.as_os_str(),
        "--build-list".as_ref(),
        list.as_os_str(),
        "--work".as_ref(),
        work.as_os_str(),
    ])
}

/// The refusal `text` and LF on stderr, exit 1 and no receipt.
fn assert_refused(out: &Output, text: &str) {
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), format!("{text}\n"));
    assert!(out.stdout.is_empty(), "no receipt: {out:?}");
}

/// The frozen sample, loaded into `work`.
fn loaded(work: &Path) -> Frozen {
    release_sample::load_frozen(&sample(work)).unwrap_or_else(|f| panic!("{}", f.message()))
}

fn ids(v: &Value) -> Vec<&str> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["block"].as_str().unwrap())
        .collect()
}

#[test]
fn relc2_build_list_is_closed_and_resolves_every_local_name() {
    let list = build_list();
    let parsed_list = parsed(&list).unwrap_or_else(|f| panic!("{}", f.message()));
    assert_eq!(
        parsed_list.counts(),
        (3, 5, 25),
        "folders, docs, listed blocks"
    );
    assert_eq!(parsed_list.total_blocks(), 33);
    for name in [
        "$D30", "$s_dec", "$r_size", "$f_purge", "$t_event", "$m_purge",
    ] {
        assert!(parsed_list.names().contains(&name.to_string()), "{name}");
    }
    // an unknown key, at the top and nested, and a close one
    let mut v = list.clone();
    v["extra"] = json!(1);
    assert_eq!(message(parsed(&v)), "release sample: unknown key extra");
    let mut v = list.clone();
    v["foldrs"] = v["folders"].take();
    v.as_object_mut().unwrap().remove("folders");
    assert_eq!(
        message(parsed(&v)),
        "release sample: unknown key foldrs; did you mean folders?"
    );
    let mut v = list.clone();
    let req = &mut v["folders"][1]["docs"][1]["blocks"][1];
    assert_eq!(req["name"], "$r_size");
    req["fields"]["prority"] = req["fields"]["priority"].take();
    req["fields"].as_object_mut().unwrap().remove("priority");
    assert_eq!(
        message(parsed(&v)),
        "release sample: unknown key prority; did you mean priority?"
    );
    let mut v = list.clone();
    v["first_session"]["supersede"]["decision"]["statment"] = json!("x");
    assert_eq!(
        message(parsed(&v)),
        "release sample: unknown key statment; did you mean statement?"
    );
    // an unresolved local name, a forward name of the first session, a duplicate, a bad shape
    let mut v = list.clone();
    v["folders"][1]["docs"][1]["blocks"][1]["links"]["describes"][2] = json!("$t_missing");
    assert_eq!(message(parsed(&v)), "release sample: invalid build list");
    let mut v = list.clone();
    v["folders"][1]["docs"][1]["blocks"][1]["body"] =
        json!("The event store follows {{$D7|the new decision}}.");
    assert_eq!(message(parsed(&v)), "release sample: invalid build list");
    let mut v = list.clone();
    v["folders"][0]["docs"][0]["blocks"][2]["name"] = json!("$t_event");
    assert_eq!(message(parsed(&v)), "release sample: invalid build list");
    let mut v = list.clone();
    v["folders"][0]["docs"][0]["blocks"][1]["type"] = json!("table");
    assert_eq!(message(parsed(&v)), "release sample: invalid build list");
    let mut v = list.clone();
    v["first_session"]["fix"]["implements"] = json!("$D30");
    assert_eq!(message(parsed(&v)), "release sample: invalid build list");
    assert_eq!(
        release_sample::parse_build_list(b"{not json")
            .err()
            .unwrap()
            .message(),
        "release sample: invalid build list"
    );
    // each bound, checked before the keys
    let mut big = serde_json::to_vec(&list).unwrap();
    big.resize(BUILD_LIST_MAX + 1, b' ');
    assert_eq!(
        message(release_sample::parse_build_list(&big)),
        "release sample: size limit exceeded"
    );
    let mut v = list.clone();
    let mut deep = json!("x");
    for _ in 0..DEPTH_MAX {
        deep = json!([deep]);
    }
    v["extra"] = deep;
    assert_eq!(message(parsed(&v)), "release sample: size limit exceeded");
    let mut v = list.clone();
    v["folders"][0]["docs"][0]["blocks"][1]["body"] = json!("x".repeat(STRING_MAX + 1));
    assert_eq!(message(parsed(&v)), "release sample: size limit exceeded");
    let mut v = list.clone();
    let blocks = v["folders"][0]["docs"][0]["blocks"].as_array_mut().unwrap();
    for _ in 0..(NODES_MAX - 33 + 1) {
        blocks.push(json!({"type": "prose", "body": "More words."}));
    }
    assert_eq!(message(parsed(&v)), "release sample: size limit exceeded");
    // the helper's own argument refusal, and a build-list refusal through it
    let out = helper(&["check".as_ref(), "--rdy".as_ref()]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "release sample: invalid arguments\n"
    );
    assert!(out.stdout.is_empty());
    let scratch = tempfile::tempdir().unwrap();
    let bad = scratch.path().join("build.json");
    let mut v = list.clone();
    v["extra"] = json!(1);
    std::fs::write(&bad, serde_json::to_vec(&v).unwrap()).unwrap();
    let rdy = rdy_bin();
    let dump = samples().join("tally.rdydump");
    let freeze = samples().join("freeze.json");
    let work = scratch.path().join("work");
    let out = helper(&[
        "check".as_ref(),
        "--rdy".as_ref(),
        rdy.as_os_str(),
        "--dump".as_ref(),
        dump.as_os_str(),
        "--freeze".as_ref(),
        freeze.as_os_str(),
        "--build-list".as_ref(),
        bad.as_os_str(),
        "--work".as_ref(),
        work.as_os_str(),
    ]);
    assert_refused(&out, "release sample: unknown key extra");
}

#[test]
fn relc2_frozen_tally_loads_and_passes_fsck() {
    let dump = std::fs::read(samples().join("tally.rdydump")).expect("the frozen sample dump");
    let freeze_bytes = std::fs::read(samples().join("freeze.json")).expect("its freeze record");
    let freeze: Value = serde_json::from_slice(&freeze_bytes).unwrap();
    // the facts the release candidate reads (scripts/release/candidate.py, read_sample)
    assert_eq!(freeze["sha256"], sha256(&dump));
    assert_eq!(freeze["size"], dump.len());
    assert_eq!(freeze["name"], "tally.rdydump");
    assert_eq!(
        freeze["build_list_sha256"],
        sha256(&std::fs::read(samples().join("build.json")).unwrap())
    );
    assert_eq!(freeze["tag"], "v1.0.0");
    let work = tempfile::tempdir().unwrap();
    let f = loaded(&work.path().join("w"));
    assert_eq!(f.freeze.blake3, blake3_hex(&dump));
    assert_eq!(f.load["landings"], 1, "{}", f.load);
    assert_eq!(f.load["tags"], 1, "{}", f.load);
    assert_eq!(f.load["dump_format"], freeze["dump_format"], "{}", f.load);
    assert_eq!(f.load["store_format"], freeze["store_format"], "{}", f.load);
    assert_eq!(f.load["files_written"], json!([".rdy.yaml"]), "{}", f.load);
    assert_eq!(f.fsck["landings"], 1, "{}", f.fsck);
    let config = std::fs::read_to_string(f.project.join(".rdy.yaml")).unwrap();
    assert_eq!(
        config,
        "pack: rdy-standard\nname: tally\ndescription: Tally collects error reports from apps and helps a team find and fix the errors.\ndecisions:\n  home: design/decisions\n"
    );
}

fn blake3_hex(bytes: &[u8]) -> String {
    release_sample::blake3_hex(bytes)
}

#[test]
fn relc2_all_history_uses_only_two_sample_actors() {
    let dump = std::fs::read(samples().join("tally.rdydump")).expect("the frozen sample dump");
    let allowed = std::fs::read_to_string(samples().join("build.json")).unwrap();
    release_sample::audit_history(&dump, &allowed).unwrap_or_else(|f| panic!("{}", f.message()));
    // an independent count: both actors recorded, and no other actor prefix at all
    let text = String::from_utf8_lossy(&dump);
    assert!(text.contains(HUMAN_ACTOR) && text.contains(AGENT_ACTOR));
    let actors = text.matches("human:").count() + text.matches("agent:").count();
    assert_eq!(
        actors,
        text.matches(HUMAN_ACTOR).count() + text.matches(AGENT_ACTOR).count()
    );
    assert!(!text.contains("ci:") && !text.contains("inbox/"));
    // a changed actor, an address, a home path and a third actor are each refused
    let swap = |from: &str, to: &str| -> Vec<u8> {
        assert_eq!(from.len(), to.len());
        let mut b = dump.clone();
        let at = b
            .windows(from.len())
            .position(|w| w == from.as_bytes())
            .unwrap_or_else(|| panic!("{from} in the dump"));
        b[at..at + to.len()].copy_from_slice(to.as_bytes());
        b
    };
    for bad in [
        swap(HUMAN_ACTOR, "human:sample-others"),
        swap(AGENT_ACTOR, "agent:sample-agen/"),
        // built here, so this file holds no address and no home path
        swap("Purge job runbook", &["ops", "example.org a"].join("@")),
        swap(
            "Purge job runbook",
            &["", "home", "someone", "abc"].join("/"),
        ),
        swap(AGENT_ACTOR, "ci:repo/sample-age"),
    ] {
        assert_eq!(
            release_sample::audit_history(&bad, &allowed)
                .err()
                .map(|f| f.message()),
            Some("release sample: baseline audit failed".to_string())
        );
    }
    // the recorded objects say the same through the tools: proposer, approver, relay, tagger
    let work = tempfile::tempdir().unwrap();
    let f = loaded(&work.path().join("w"));
    let b = release_sample::audit_baseline(&f).unwrap_or_else(|e| panic!("{}", e.message()));
    let landing = &b.log[0];
    assert_eq!(landing["approved_by"], HUMAN_ACTOR, "{landing}");
    assert_eq!(landing["proposed_by"]["actor"], AGENT_ACTOR, "{landing}");
    assert_eq!(landing["relayed_by"]["actor"], AGENT_ACTOR, "{landing}");
    assert_eq!(b.tags["versions"][0]["tagged_by"], HUMAN_ACTOR);
    for v in b.views["views"].as_array().unwrap() {
        assert_eq!(v["owner"]["actor"], AGENT_ACTOR, "{v}");
    }
}

#[test]
fn relc2_baseline_has_one_tag_and_no_open_work() {
    let work = tempfile::tempdir().unwrap();
    let f = loaded(&work.path().join("w"));
    let b = release_sample::audit_baseline(&f).unwrap_or_else(|e| panic!("{}", e.message()));
    assert_eq!(b.log.as_array().unwrap().len(), 1, "{}", b.log);
    assert_eq!(b.log[0]["landing"], f.freeze.landing.as_str());
    assert_eq!(b.log[0]["tags"], json!(["v1.0.0"]));
    let versions = b.tags["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1, "{}", b.tags);
    assert_eq!(versions[0]["name"], "v1.0.0");
    assert_eq!(versions[0]["landing"], f.freeze.landing.as_str());
    assert_eq!(b.pending["items"], json!([]), "{}", b.pending);
    assert_eq!(b.pending["not_ready"], json!([]), "{}", b.pending);
    for v in b.views["views"].as_array().unwrap() {
        assert_eq!(v["closed"], true, "no open view: {v}");
    }
    assert_eq!(b.publication["selection"]["docs"], 5);
    assert_eq!(b.publication["selection"]["blocks"], 33);
    assert!(f.freeze.ids.len() >= 23, "{:?}", f.freeze.ids);
    assert!(f.freeze.ids["$D30"].starts_with("D-"));
}

#[test]
fn relc2_real_mcp_first_session_confirms_requirement_and_updates_flow() {
    let work = tempfile::tempdir().unwrap();
    let f = loaded(&work.path().join("w"));
    let s = release_sample::first_session(&f).unwrap_or_else(|e| panic!("{}", e.message()));
    let (d30, r_size, f_purge) = (
        f.freeze.ids["$D30"].as_str(),
        f.freeze.ids["$r_size"].as_str(),
        f.freeze.ids["$f_purge"].as_str(),
    );
    // step 4: both content blocks depend on D30 through both links
    for block in [r_size, f_purge] {
        for kind in ["decided_by", "implements"] {
            assert!(
                s.brief_before["dependents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["block"] == block && d["kind"] == kind && d["on"] == d30),
                "{block} {kind}: {:#}",
                s.brief_before
            );
        }
        assert!(
            ids(&s.brief_before["will_go_stale"]).contains(&block),
            "{:#}",
            s.brief_before
        );
    }
    // step 5: exactly the two content blocks
    let c = s.supersede["consequences"].as_array().unwrap();
    assert_eq!(c.len(), 2, "{:#}", s.supersede);
    let entry = |b: &str| c.iter().find(|e| e["block"] == b).unwrap().clone();
    for b in [r_size, f_purge] {
        let e = entry(b);
        assert_eq!(e["kind"], "dependent", "{e}");
        assert_eq!(e["because"]["changed"], d30, "{e}");
        assert_eq!(e["because"]["values"], json!(["30 days"]), "{e}");
        assert_eq!(
            e["because"]["edges"],
            json!(["decided_by", "implements"]),
            "{e}"
        );
    }
    let req = entry(r_size);
    assert_eq!(req["follow_up"], "confirm", "{req}");
    assert!(req.get("matched").is_none(), "{req}");
    let flow = entry(f_purge);
    assert_eq!(flow["follow_up"], "update", "{flow}");
    let m = flow["matched"].as_array().unwrap();
    assert_eq!(m.len(), 1, "{flow}");
    assert_eq!(m[0]["value"], "30 days");
    assert_eq!(m[0]["field"], "steps[1]");
    assert_eq!(m[0]["line"], 1);
    assert!(s.d7.starts_with("D-") && s.d7 != d30);
}

#[test]
fn relc2_fixed_content_keeps_supersedes_lineage() {
    let work = tempfile::tempdir().unwrap();
    let f = loaded(&work.path().join("w"));
    let s = release_sample::first_session(&f).unwrap_or_else(|e| panic!("{}", e.message()));
    let d30 = f.freeze.ids["$D30"].as_str();
    // the fix write leaves no consequence on D30
    for e in s.fix["consequences"].as_array().unwrap() {
        assert_ne!(e["because"]["changed"], d30, "{e}");
    }
    // nothing left to fix, and the lineage stays
    assert_eq!(
        s.brief_after["will_go_stale"],
        json!([]),
        "{:#}",
        s.brief_after
    );
    assert_eq!(s.brief_after["suspects"], json!([]), "{:#}", s.brief_after);
    for b in [&f.freeze.ids["$r_size"], &f.freeze.ids["$f_purge"]] {
        assert!(
            !ids(&s.brief_after["dependents"]).contains(&b.as_str()),
            "{:#}",
            s.brief_after
        );
    }
    assert!(
        s.brief_after["lineage"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["block"] == s.d7.as_str()
                && l["kind"] == "supersedes"
                && l["direction"] == "in"),
        "{:#}",
        s.brief_after
    );
    // the card names the superseded decision; the tag names the new landing
    let fact = |name: &str| -> Value {
        s.card["facts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == name)
            .map(|x| x["value"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(fact("Supersedes"), d30, "{:#}", s.card);
    assert_eq!(fact("Tensions"), "0 blocking, 0 advisory", "{:#}", s.card);
    assert_ne!(s.landing, f.freeze.landing);
    assert_eq!(s.tag["landing"], s.landing.as_str());
    assert_eq!(s.tag["name"], "v1.1.0");
    assert_eq!(s.tag["tagged_by"], HUMAN_ACTOR);
}

#[test]
fn relc2_reopen_preserves_the_first_session_result() {
    let work = tempfile::tempdir().unwrap();
    let f = loaded(&work.path().join("w"));
    let s = release_sample::first_session(&f).unwrap_or_else(|e| panic!("{}", e.message()));
    let r = release_sample::reopen(&f, &s).unwrap_or_else(|e| panic!("{}", e.message()));
    let names: Vec<&str> = r.tags["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["v1.0.0", "v1.1.0"], "{}", r.tags);
    assert_eq!(r.tags["versions"][1]["landing"], s.landing.as_str());
    assert_eq!(
        r.publication["profile"]["audience"], "org",
        "{}",
        r.publication
    );
    assert_eq!(r.publication["references"]["status"], "clear");
    assert_eq!(r.publication["scope"]["landing"], s.landing.as_str());
    assert!(
        r.flow.contains("7 days") && !r.flow.contains("30 days"),
        "{}",
        r.flow
    );
    assert!(r.flow.contains(&s.d7), "{}", r.flow);
    assert!(r.requirement.contains(&s.d7), "{}", r.requirement);
    assert!(
        r.requirement
            .contains("with 20% free space above that amount"),
        "{}",
        r.requirement
    );
    assert!(
        !r.requirement.contains(&f.freeze.ids["$D30"]),
        "{}",
        r.requirement
    );
}

#[test]
fn relc2_bad_or_changed_dump_never_gets_a_pass_receipt() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    let dump = std::fs::read(samples().join("tally.rdydump")).expect("the frozen sample dump");
    let freeze = samples().join("freeze.json");
    // one flipped byte, a cut file and a longer file
    let mut flipped = dump.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x01;
    let mut longer = dump.clone();
    longer.push(0);
    for (i, bytes) in [flipped, dump[..dump.len() - 1].to_vec(), longer]
        .iter()
        .enumerate()
    {
        let f = dir.join(format!("bad{i}.rdydump"));
        std::fs::write(&f, bytes).unwrap();
        let out = check_cli(&f, &freeze, &dir.join(format!("w{i}")));
        assert_refused(&out, "release sample: frozen dump does not verify");
    }
    // the right bytes under a freeze record that names other bytes
    let copy = dir.join("tally.rdydump");
    std::fs::write(&copy, &dump).unwrap();
    let mut other: Value = serde_json::from_slice(&std::fs::read(&freeze).unwrap()).unwrap();
    other["blake3"] = json!("0".repeat(64));
    let other_file = dir.join("other.json");
    std::fs::write(&other_file, serde_json::to_vec(&other).unwrap()).unwrap();
    assert_refused(
        &check_cli(&copy, &other_file, &dir.join("wf")),
        "release sample: frozen dump does not verify",
    );
    // a valid dump of another project, under a freeze record of its own bytes
    let p = dir.join("empty");
    let mut init = Command::new(rdy_bin());
    rdy_qa::fixtures::isolate_person(&mut init, &dir.join("home"), &dir.join("home/.config"));
    init.args(["init", "--no-daemon"]).arg(&p);
    assert!(output_within(&mut init, GUARD).unwrap().status.success());
    let empty = dir.join("empty.rdydump");
    let mut d = Command::new(rdy_bin());
    rdy_qa::fixtures::isolate_person(&mut d, &dir.join("home"), &dir.join("home/.config"));
    d.args(["dump", "--no-daemon", "--project"])
        .arg(&p)
        .arg("--out")
        .arg(&empty);
    assert!(output_within(&mut d, GUARD).unwrap().status.success());
    let bytes = std::fs::read(&empty).unwrap();
    let mut forged = other.clone();
    forged["blake3"] = json!(blake3_hex(&bytes));
    forged["sha256"] = json!(sha256(&bytes));
    forged["size"] = json!(bytes.len());
    let forged_file = dir.join("forged.json");
    std::fs::write(&forged_file, serde_json::to_vec(&forged).unwrap()).unwrap();
    let out = check_cli(&empty, &forged_file, &dir.join("we"));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "no receipt: {out:?}");
    // a missing dump, and a work directory that exists
    assert_refused(
        &check_cli(&dir.join("missing.rdydump"), &freeze, &dir.join("wm")),
        "release sample: file operation failed",
    );
    std::fs::create_dir(dir.join("taken")).unwrap();
    assert_refused(
        &check_cli(&copy, &freeze, &dir.join("taken")),
        "release sample: file operation failed",
    );
}

#[test]
fn relc2_release_fixture_is_loaded_without_regeneration() {
    let frozen = samples().join("tally.rdydump");
    let before = std::fs::read(&frozen).expect("the frozen sample dump");
    let freeze: Value =
        serde_json::from_slice(&std::fs::read(samples().join("freeze.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let copy = scratch.path().join("tally.rdydump");
    std::fs::write(&copy, &before).unwrap();
    let out = check_cli(
        &copy,
        &samples().join("freeze.json"),
        &scratch.path().join("w"),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.ends_with('\n') && text.lines().count() == 1, "{text}");
    let receipt: Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<&str> = receipt
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut want = release_sample::RECEIPT_KEYS.to_vec();
    want.sort_unstable();
    assert_eq!(keys, want, "{receipt:#}");
    assert_eq!(receipt["receipt"], release_sample::RECEIPT_FORMAT);
    assert_eq!(receipt["result"], "pass");
    assert_eq!(receipt["skipped"], 0);
    assert_eq!(receipt["dump_sha256"], freeze["sha256"]);
    assert_eq!(receipt["dump_blake3"], freeze["blake3"]);
    assert_eq!(receipt["dump_size"], freeze["size"]);
    assert_eq!(receipt["baseline_landing"], freeze["landing"]);
    assert_eq!(receipt["baseline_tag"], "v1.0.0");
    assert_eq!(receipt["session_tag"], "v1.1.0");
    assert_ne!(receipt["session_landing"], freeze["landing"]);
    // the checked-in bytes stay as they were: the gate never regenerates them
    assert_eq!(std::fs::read(&frozen).unwrap(), before);
    // git keeps the frozen files byte for byte
    let attributes = std::fs::read_to_string(root().join(".gitattributes")).unwrap();
    for rule in [
        "samples/tally/tally.rdydump -text",
        "samples/tally/freeze.json -text",
        "samples/tally/build.json -text",
    ] {
        assert!(attributes.lines().any(|l| l == rule), "{rule}");
    }
}
