//! hc8 (D-2FMRR, D-FR0C4, D-QF2HQ): `rdy dump --out <file> [--after <file>]` and `rdy load <file>
//! [<dir>] [--then <file>]... [--stop-at <selector>]`, the CLI twins of `rdy_dump` and
//! `rdy_load`. The CLI prints the exact lines of the spec, keeps the order of repeated `--then`
//! arguments, and returns the engine's facts and errors unchanged.
use rdy_engine::load::load_chain;
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, RdyError};
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use rdy_qa::every_kind::facts;
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::mcp::McpClient;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

const SECRET: &str = "The launch code is ZEBRA-7731.";

/// A served project and the dump chain of its history: the full dump `f0` after the secret
/// landed (tag `v1.0.0`), the increment `i1` after its removal (tag `v2.0.0`), and `i2` after one
/// more landing (tag `v3.0.0`). The project is closed; no daemon runs.
struct Chain {
    s: Served,
    scratch: tempfile::TempDir,
    f0: PathBuf,
    i1: PathBuf,
    i2: PathBuf,
    f0_end: Location,
    i2_end: Location,
    block: String,
    secret_landing: String,
}

fn add(r: &mut Rdy, n: usize, body: &str) -> String {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": format!("D{n}"), "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
    )
    .unwrap();
    w.created["$p"].clone()
}

fn remove(r: &mut Rdy, target: &str) {
    r.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    land(r, "redact").unwrap();
}

fn chain() -> Chain {
    let s = Served::new();
    let scratch = short_tempdir();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r, "work").unwrap();
    let block = add(&mut r, 1, SECRET);
    let secret_landing = land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let f0 = scratch.path().join("f0.rdydump");
    let a = r.read().dump(&f0).unwrap();
    remove(&mut r, &block);
    add(&mut r, 2, "a later block");
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    let i1 = scratch.path().join("i1.rdydump");
    let b = r.read().dump_after(&a.chain_end(), &i1).unwrap();
    add(&mut r, 3, "the last block");
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v3.0.0", None, None).unwrap();
    r.close().unwrap();
    // an agent's first session records its actor in the log; register it before the last
    // dump, so that no later session adds a batch the chain does not hold
    let mut m = McpClient::start_proxy(&s);
    m.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    drop(m);
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let i2 = scratch.path().join("i2.rdydump");
    let c = r.read().dump_after(&b.chain_end(), &i2).unwrap();
    r.close().unwrap();
    Chain {
        s,
        scratch,
        f0,
        i1,
        i2,
        f0_end: a.end,
        i2_end: c.end,
        block,
        secret_landing,
    }
}

impl Chain {
    fn agent(&self) -> McpClient {
        let mut c = McpClient::start_proxy(&self.s);
        c.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a", "role": "writer"}),
        )
        .ok();
        c
    }

    fn to(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    fn files(&self) -> Vec<PathBuf> {
        vec![self.f0.clone(), self.i1.clone(), self.i2.clone()]
    }

    /// The CLI's `--json` load of `f0` and each of `then` into `to`, with `stop`.
    fn cli_load(&self, to: &Path, then: &[&Path], stop: Option<&str>) -> (bool, Value) {
        let mut args = vec!["--json", "load", arg(&self.f0), arg(to)];
        for t in then {
            args.extend(["--then", arg(t)]);
        }
        if let Some(stop) = stop {
            args.extend(["--stop-at", stop]);
        }
        self.s.json(&args)
    }

    /// The same load through `rdy_load`.
    fn mcp_load(&self, m: &mut McpClient, to: &Path, then: &[&Path], stop: Option<&str>) -> Value {
        let then: Vec<&str> = then.iter().map(|t| arg(t)).collect();
        let mut args = json!({"file": arg(&self.f0), "dir": arg(to), "then": then});
        if let Some(stop) = stop {
            args["stop_at"] = json!(stop);
        }
        match m.call("rdy_load", args) {
            rdy_qa::mcp::Reply::Ok(v) => v,
            rdy_qa::mcp::Reply::ToolError(v) => v,
            other => panic!("{other:?}"),
        }
    }
}

fn arg(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The store's segment files, by name.
fn segs(project: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(project.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

fn without(v: &Value, keys: &[&str]) -> Value {
    let mut v = v.clone();
    for k in keys {
        v.as_object_mut().unwrap().remove(*k);
    }
    v
}

fn failure(e: &RdyError) -> Value {
    json!({"code": e.code, "message": e.message, "fix": e.fix})
}

fn reply_failure(v: &Value) -> Value {
    json!({"code": v["code"], "message": v["message"], "fix": v["fix"]})
}

#[test]
fn hc8_mcp_and_cli_export_identical_chain_facts() {
    let c = chain();
    // the engine's answers, read before any daemon runs
    let (e_full, e_after_f0, e_after_i1) = {
        let r = Rdy::open(c.s.dir.path()).unwrap();
        let st = r.read();
        let full = st.dump(&c.to("e-full")).unwrap();
        let after_f0 = st.dump_after_file(&c.f0, &c.to("e-after-f0")).unwrap();
        let after_i1 = st.dump_after_file(&c.i1, &c.to("e-after-i1")).unwrap();
        drop(st);
        r.close().unwrap();
        (full, after_f0, after_i1)
    };
    assert_eq!(
        std::fs::read(c.to("e-after-i1")).unwrap(),
        std::fs::read(&c.i2).unwrap(),
        "the engine's increment after i1 is i2"
    );
    let mut m = c.agent();
    let outs = |name: &str| (c.to(&format!("c-{name}")), c.to(&format!("m-{name}")));
    let (c_full, m_full) = outs("full");
    let (c_f0, m_f0) = outs("after-f0");
    let (c_i1, m_i1) = outs("after-i1");
    let (ok, cli_full) = c.s.json(&["--json", "dump", "--out", arg(&c_full)]);
    assert!(ok, "{cli_full}");
    let mcp_full = m.call("rdy_dump", json!({"out": arg(&m_full)})).ok();
    for (cli_out, mcp_out, after, engine) in [
        (&c_f0, &m_f0, &c.f0, &e_after_f0),
        (&c_i1, &m_i1, &c.i1, &e_after_i1),
    ] {
        let (ok, cli) = c.s.json(&[
            "--json",
            "dump",
            "--out",
            arg(cli_out),
            "--after",
            arg(after),
        ]);
        assert!(ok, "{cli}");
        let mcp = m
            .call(
                "rdy_dump",
                json!({"out": arg(mcp_out), "after": arg(after)}),
            )
            .ok();
        let want = serde_json::to_value(engine).unwrap();
        assert_eq!(without(&cli, &["out"]), without(&want, &["out"]), "cli");
        assert_eq!(without(&mcp, &["out"]), without(&want, &["out"]), "mcp");
        assert_eq!(cli["out"], arg(cli_out));
        assert_eq!(mcp["out"], arg(mcp_out));
        assert_eq!(cli["kind"], "increment");
        assert_eq!(
            std::fs::read(cli_out).unwrap(),
            std::fs::read(mcp_out).unwrap(),
            "the same bytes"
        );
    }
    let want = serde_json::to_value(&e_full).unwrap();
    assert_eq!(without(&cli_full, &["out"]), without(&want, &["out"]));
    assert_eq!(without(&mcp_full, &["out"]), without(&want, &["out"]));
    assert_eq!(cli_full["kind"], "full");
    assert_eq!(
        std::fs::read(&c_full).unwrap(),
        std::fs::read(&m_full).unwrap()
    );
}

#[test]
fn hc8_noop_is_null_and_prints_the_exact_line() {
    let c = chain();
    let i2 = arg(&c.i2);
    let n = c.to("none.rdydump");
    let nothing = format!(
        "nothing after {i2}: the store ends at {}.{}\n",
        c.i2_end.segment, c.i2_end.offset
    );
    let out = c.s.rdy(&["dump", "--out", arg(&n), "--after", i2]);
    assert_eq!(stdout(&out), nothing);
    assert!(!n.exists(), "a no-op makes no file");
    let (ok, cli) =
        c.s.json(&["--json", "dump", "--out", arg(&n), "--after", i2]);
    assert!(ok, "{cli}");
    assert!(cli["out"].is_null(), "{cli}");
    assert_eq!(
        (cli["batches"].as_u64(), cli["frames"].as_u64()),
        (Some(0), Some(0))
    );
    assert_eq!(cli["files"], json!([]), "{cli}");
    assert_eq!(
        cli["end"],
        json!({"segment": c.i2_end.segment, "offset": c.i2_end.offset})
    );
    let mut m = c.agent();
    let mcp = m
        .call("rdy_dump", json!({"out": arg(&n), "after": i2}))
        .ok();
    assert_eq!(
        mcp, cli,
        "the same answer on both surfaces, out null on both"
    );
    assert!(!n.exists());
    // an increment that has batches prints the spec's line with the caller's after argument
    let o = c.to("inc.rdydump");
    let (ok, inc) = c.s.json(&[
        "--json",
        "dump",
        "--out",
        arg(&c.to("inc-json.rdydump")),
        "--after",
        arg(&c.f0),
    ]);
    assert!(ok, "{inc}");
    let out = c.s.rdy(&["dump", "--out", arg(&o), "--after", arg(&c.f0)]);
    assert_eq!(
        stdout(&out),
        format!(
            "dumped {} batches and {} frames after {}, with {} project files, to {} ({} bytes)\n",
            inc["batches"],
            inc["frames"],
            arg(&c.f0),
            inc["files"].as_array().unwrap().len(),
            arg(&o),
            inc["bytes"]
        )
    );
    // a full dump keeps its words
    let full = c.to("full.rdydump");
    let out = c.s.rdy(&["dump", "--out", arg(&full)]);
    let text = stdout(&out);
    assert!(
        text.starts_with("dumped ") && text.contains(" frames, with ") && !text.contains(" after "),
        "{text:?}"
    );
}

#[test]
fn hc8_then_order_and_all_three_stops_have_parity() {
    let c = chain();
    let before: Vec<Vec<u8>> = c
        .files()
        .iter()
        .map(|f| std::fs::read(f).unwrap())
        .collect();
    let source = segs(c.s.dir.path());
    let mut m = c.agent();
    let stops = [
        None,
        Some(format!("landing:{}", &c.secret_landing[..12])),
        Some("tag:v1.0.0".to_string()),
        Some(format!("pos:{}.{}", c.f0_end.segment, c.f0_end.offset)),
        Some("tag:v2.0.0".to_string()),
    ];
    for (n, stop) in stops.iter().enumerate() {
        let (a, b) = (c.to(&format!("cli{n}")), c.to(&format!("mcp{n}")));
        let then = [c.i1.as_path(), c.i2.as_path()];
        let (ok, cli) = c.cli_load(&a, &then, stop.as_deref());
        assert!(ok, "{stop:?}: {cli}");
        let mcp = c.mcp_load(&mut m, &b, &then, stop.as_deref());
        let strip = ["project", "next"];
        assert_eq!(without(&cli, &strip), without(&mcp, &strip), "{stop:?}");
        assert_eq!(cli["chain"].as_array().unwrap().len(), 3, "{cli}");
        match stop {
            None => assert!(cli["stopped_at"].is_null(), "{cli}"),
            Some(_) => assert!(!cli["stopped_at"].is_null(), "{cli}"),
        }
        let (fa, fb) = (Rdy::open(&a).unwrap(), Rdy::open(&b).unwrap());
        assert_eq!(facts(&fa), facts(&fb), "{stop:?}");
        fa.close().unwrap();
        fb.close().unwrap();
        assert_eq!(segs(&a), segs(&b), "{stop:?}: the same log bytes");
        // the text: the stop sits right before `; fsck ok`
        let t = c.to(&format!("text{n}"));
        let mut args = vec!["load", arg(&c.f0), arg(&t)];
        for f in [&c.i1, &c.i2] {
            args.extend(["--then", arg(f)]);
        }
        if let Some(stop) = stop {
            args.extend(["--stop-at", stop]);
        }
        let text = stdout(&c.s.rdy(&args));
        let at = match &cli["stopped_at"] {
            Value::Null => String::new(),
            p => format!(
                ", stopped at {}.{}",
                p["position"]["segment"], p["position"]["offset"]
            ),
        };
        let mut want = format!(
            "loaded {} batches and {} frames into {}: {} landings, {} versions, {} views{at}; fsck ok\nnext: ",
            cli["batches"],
            cli["frames"],
            arg(&t),
            cli["landings"],
            cli["tags"],
            cli["views"],
        );
        assert!(text.starts_with(&want), "{stop:?}: {text:?} vs {want:?}");
        want = text[want.len()..].to_string();
        let (next, rest) = want.split_once('\n').unwrap_or((&want, ""));
        assert!(!next.is_empty(), "{text:?}");
        // each warning: code and message, the fix, then one target line per issue
        let warnings = cli["warnings"].as_array().cloned().unwrap_or_default();
        let mut expect = String::new();
        for w in &warnings {
            expect.push_str(&format!(
                "W_STOP_BEFORE_REDACTION: {}\nfix: {}\n",
                w["message"].as_str().unwrap(),
                w["fix"].as_str().unwrap()
            ));
            for i in w["issues"].as_array().unwrap() {
                expect.push_str(&format!("  target: {}\n", i["target"].as_str().unwrap()));
            }
        }
        assert_eq!(rest, expect, "{stop:?}");
        assert_eq!(
            warnings.len(),
            usize::from(stop.as_deref().is_some_and(|s| s != "tag:v2.0.0")),
            "only a stop before the removal warns: {stop:?}"
        );
        if stop.as_deref() == Some("tag:v1.0.0") {
            assert_eq!(warnings[0]["issues"].as_array().unwrap().len(), 1);
            assert!(
                warnings[0]["issues"][0]["target"]
                    .as_str()
                    .unwrap()
                    .starts_with(&c.block)
            );
        }
    }
    // the order of the --then arguments is the order of the chain, on both surfaces
    let (a, b) = (c.to("swap-cli"), c.to("swap-mcp"));
    let swapped = [c.i2.as_path(), c.i1.as_path()];
    let (ok, cli) = c.cli_load(&a, &swapped, None);
    assert!(!ok, "{cli}");
    let mcp = c.mcp_load(&mut m, &b, &swapped, None);
    assert_eq!(cli["code"], "E_DUMP_CHAIN", "{cli}");
    assert_eq!(reply_failure(&cli), reply_failure(&mcp));
    assert!(
        !a.exists() && !b.exists(),
        "a refused load leaves no target"
    );
    // no input file and no source log changed
    let after: Vec<Vec<u8>> = c
        .files()
        .iter()
        .map(|f| std::fs::read(f).unwrap())
        .collect();
    assert_eq!(before, after);
    assert_eq!(source, segs(c.s.dir.path()));
}

#[test]
fn hc8_paths_use_project_root_for_mcp_and_session_cwd_for_cli() {
    let c = chain();
    // the CLI half: relative paths are the caller's working directory's, not the project root's
    let work = c.s.dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    for (name, from) in [("f0.rdydump", &c.f0), ("i1.rdydump", &c.i1)] {
        std::fs::copy(from, work.join(name)).unwrap();
    }
    let env = c.s.env_pairs();
    let root = c.s.dir.path();
    let run = |args: &[&str]| {
        human_command(root, &env, args)
            .current_dir(&work)
            .output()
            .unwrap()
    };
    let out = run(&["dump", "--out", "next.rdydump", "--after", "i1.rdydump"]);
    let text = stdout(&out);
    assert!(
        text.contains(" after i1.rdydump, ")
            && text.contains(&format!(
                ", to {} (",
                work.canonicalize().unwrap().join("next.rdydump").display()
            )),
        "{text:?}"
    );
    assert!(work.join("next.rdydump").is_file());
    assert!(!c.s.dir.path().join("next.rdydump").exists());
    // the daemon was asked from `work`: a missing --after is reported under that directory
    let out = run(&[
        "--json",
        "dump",
        "--out",
        "o2.rdydump",
        "--after",
        "gone.rdydump",
    ]);
    assert!(!out.status.success());
    let e: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains(arg(&work.canonicalize().unwrap().join("gone.rdydump"))),
        "{e}"
    );
    // load: the file, each --then and the dir are the caller's too
    let out = run(&[
        "load",
        "f0.rdydump",
        "loaded",
        "--then",
        "i1.rdydump",
        "--then",
        "next.rdydump",
    ]);
    let text = stdout(&out);
    assert!(text.starts_with("loaded "), "{text:?}");
    assert!(work.join("loaded/.rdy/store").exists());
    assert!(!c.s.dir.path().join("loaded").exists());
    let out = run(&[
        "--json",
        "load",
        "f0.rdydump",
        "again",
        "--then",
        "gone.rdydump",
    ]);
    assert!(!out.status.success());
    let e: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(
        e["message"].as_str().unwrap().contains("gone.rdydump"),
        "{e}"
    );
    assert!(!work.join("again").exists());
    // the target default stays: dir, else --project, else the working directory
    let proj = c.to("proj");
    std::fs::create_dir(&proj).unwrap();
    let out = c.s.rdy(&["load", arg(&c.f0), "--project", arg(&proj)]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(proj.join(".rdy/store").exists());
}

#[test]
fn hc8_unknown_fields_and_wrong_types_refuse() {
    let c = chain();
    // the CLI has no scope, --to or time flag; a flag that is not here is refused as unknown
    for args in [
        vec!["dump", "--out", "x", "--scope", "canon"],
        vec!["dump", "--out", "x", "--since", "1"],
        vec!["load", "x", "y", "--to", "z"],
        vec!["load", "x", "y", "--stop-at-time", "1"],
        vec!["load", "x", "y", "--then"],
    ] {
        let out = c.s.rdy(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--"),
            "{args:?}"
        );
    }
    // a stop that is not a selector is refused before any file is opened
    let to = c.to("never");
    let (ok, e) =
        c.s.json(&["--json", "load", "missing", arg(&to), "--stop-at", "bogus"]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(e["message"].as_str().unwrap().contains("bogus"), "{e}");
    assert!(!to.exists());
}

#[test]
fn hc8_limit_precedes_nonexistent_paths() {
    let c = chain();
    let to = c.to("never");
    let mut args: Vec<String> = ["--json", "load", "missing/full", arg(&to)]
        .into_iter()
        .map(String::from)
        .collect();
    for i in 0..4097 {
        args.push("--then".into());
        args.push(format!("missing/{i}"));
    }
    args.push("--stop-at".into());
    args.push("bogus".into());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (ok, e) = c.s.json(&refs);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_LIMIT", "{e}");
    assert!(e["message"].as_str().unwrap().contains("4096"), "{e}");
    assert!(!to.exists());
    // at the limit the first missing path is what fails
    let n = args.len() - 2 - 2;
    let refs: Vec<&str> = args[..n].iter().map(String::as_str).collect();
    let (ok, e) = c.s.json(&refs);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("missing/full"),
        "{e}"
    );
}

#[test]
fn hc8_any_session_may_dump_and_load() {
    let c = chain();
    // a person and an agent run the same commands with the same effect
    let (ok, human) = c.s.json(&[
        "--json",
        "dump",
        "--out",
        arg(&c.to("h")),
        "--after",
        arg(&c.f0),
    ]);
    assert!(ok, "{human}");
    let (ok, agent) = c.s.json_as(
        "agent:a",
        Some("a"),
        &[
            "--json",
            "dump",
            "--out",
            arg(&c.to("g")),
            "--after",
            arg(&c.f0),
        ],
    );
    assert!(ok, "{agent}");
    assert_eq!(without(&human, &["out"]), without(&agent, &["out"]));
    let (ok, loaded) = c.s.json_as(
        "agent:a",
        Some("a"),
        &[
            "--json",
            "load",
            arg(&c.f0),
            arg(&c.to("l")),
            "--then",
            arg(&c.to("g")),
        ],
    );
    assert!(ok, "{loaded}");
    assert_eq!(loaded["chain"].as_array().unwrap().len(), 2);
}

#[test]
fn hc8_late_corruption_and_link_errors_match_on_all_surfaces() {
    let c = chain();
    // a late flaw: one flipped byte before the trailer of the last file, then a cut trailer
    let bytes = std::fs::read(&c.i2).unwrap();
    let trailer = 37 + rdy_store::dump::TRAILER_LEN;
    let mut flipped = bytes.clone();
    let at = flipped.len() - trailer - 1;
    flipped[at] ^= 1;
    let bad_flip = c.to("bad-flip.rdydump");
    std::fs::write(&bad_flip, &flipped).unwrap();
    let bad_cut = c.to("bad-cut.rdydump");
    std::fs::write(&bad_cut, &bytes[..bytes.len() - 5]).unwrap();
    let bad_tail = c.to("bad-tail.rdydump");
    let mut tail = bytes.clone();
    tail.extend_from_slice(b"extra");
    std::fs::write(&bad_tail, &tail).unwrap();
    // a dump of another store, for a predecessor that does not match
    let other = short_tempdir();
    let mut o = Rdy::init(&other.path().join("o"), "rdy-standard").unwrap();
    add(&mut o, 0, "another store");
    land(&mut o, "work").unwrap();
    let other_dump = c.to("other.rdydump");
    o.read().dump(&other_dump).unwrap();
    o.close().unwrap();

    let [f0, i1, i2] = [c.f0.clone(), c.i1.clone(), c.i2.clone()];
    let load_cases: Vec<(&str, Vec<PathBuf>, &str)> = vec![
        (
            "flipped last byte",
            vec![f0.clone(), i1.clone(), bad_flip],
            "E_DUMP",
        ),
        (
            "cut trailer",
            vec![f0.clone(), i1.clone(), bad_cut],
            "E_DUMP",
        ),
        (
            "bytes after the trailer",
            vec![f0.clone(), i1.clone(), bad_tail],
            "E_DUMP",
        ),
        ("a gap", vec![f0.clone(), i2.clone()], "E_DUMP_CHAIN"),
        (
            "a repeat",
            vec![f0.clone(), i1.clone(), i1.clone()],
            "E_DUMP_CHAIN",
        ),
        ("an increment first", vec![i1.clone()], "E_DUMP_CHAIN"),
        (
            "a second full dump",
            vec![f0.clone(), f0.clone()],
            "E_DUMP_CHAIN",
        ),
        (
            "the wrong order",
            vec![f0.clone(), i2.clone(), i1.clone()],
            "E_DUMP_CHAIN",
        ),
        (
            "another store",
            vec![f0.clone(), other_dump.clone()],
            "E_DUMP_CHAIN",
        ),
    ];
    // the engine first: nothing runs in the project directory yet
    let to = c.to("target");
    let engine: Vec<Value> = load_cases
        .iter()
        .map(|(name, files, code)| {
            let e = load_chain(files, &to, None).expect_err(name);
            assert_eq!(e.code, *code, "{name}: {e:?}");
            failure(&e)
        })
        .collect();
    let export_cases: Vec<(&str, PathBuf, &str)> = vec![
        ("another store", other_dump.clone(), "E_DUMP_CHAIN"),
        ("a missing file", c.to("gone.rdydump"), "E_BAD_INPUT"),
        ("a corrupt file", c.to("bad-cut.rdydump"), "E_DUMP"),
    ];
    let engine_export: Vec<Value> = {
        let r = Rdy::open(c.s.dir.path()).unwrap();
        let st = r.read();
        let v = export_cases
            .iter()
            .map(|(name, after, code)| {
                let e = st
                    .dump_after_file(after, &c.to("x.rdydump"))
                    .expect_err(name);
                assert_eq!(e.code, *code, "{name}: {e:?}");
                failure(&e)
            })
            .collect();
        drop(st);
        r.close().unwrap();
        v
    };
    let mut m = c.agent();
    for ((name, files, _), want) in load_cases.iter().zip(&engine) {
        let then: Vec<&Path> = files[1..].iter().map(PathBuf::as_path).collect();
        let (ok, cli) = {
            let mut args = vec!["--json", "load", arg(&files[0]), arg(&to)];
            for t in &then {
                args.extend(["--then", arg(t)]);
            }
            c.s.json(&args)
        };
        assert!(!ok, "{name}: {cli}");
        assert_eq!(&reply_failure(&cli), want, "{name}: cli");
        let thens: Vec<&str> = then.iter().map(|t| arg(t)).collect();
        let r = m.call(
            "rdy_load",
            json!({"file": arg(&files[0]), "dir": arg(&to), "then": thens}),
        );
        let rdy_qa::mcp::Reply::ToolError(mcp) = r else {
            panic!("{name}: {r:?}");
        };
        assert_eq!(&reply_failure(&mcp), want, "{name}: mcp");
        assert!(
            !to.exists(),
            "{name}: a refused load leaves the target as it was"
        );
    }
    for ((name, after, _), want) in export_cases.iter().zip(&engine_export) {
        let out = c.to("y.rdydump");
        let (ok, cli) =
            c.s.json(&["--json", "dump", "--out", arg(&out), "--after", arg(after)]);
        assert!(!ok, "{name}: {cli}");
        assert_eq!(&reply_failure(&cli), want, "{name}: cli");
        let r = m.call("rdy_dump", json!({"out": arg(&out), "after": arg(after)}));
        let rdy_qa::mcp::Reply::ToolError(mcp) = r else {
            panic!("{name}: {r:?}");
        };
        assert_eq!(&reply_failure(&mcp), want, "{name}: mcp");
        assert!(!out.exists(), "{name}: a refused export places nothing");
    }
}

#[test]
fn hc8_stopped_warning_lists_reapprovable_targets() {
    let c = chain();
    let to = c.to("stopped");
    let then = [c.i1.as_path(), c.i2.as_path()];
    let (ok, v) = c.cli_load(&to, &then, Some("tag:v1.0.0"));
    assert!(ok, "{v}");
    let targets: Vec<String> = v["warnings"][0]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["target"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(targets.len(), 1, "{v}");
    assert!(targets[0].starts_with(&c.block), "{targets:?}");
    // the text lists the same target on its own line
    let text_to = c.to("stopped-text");
    let out = c.s.rdy(&[
        "load",
        arg(&c.f0),
        arg(&text_to),
        "--then",
        arg(&c.i1),
        "--then",
        arg(&c.i2),
        "--stop-at",
        "tag:v1.0.0",
    ]);
    let text = stdout(&out);
    assert!(
        text.contains("\nW_STOP_BEFORE_REDACTION: This store stops before redaction apply ")
            && text.contains("\nfix: Propose the redaction of the listed targets again")
            && text.ends_with(&format!("\n  target: {}\n", targets[0])),
        "{text:?}"
    );
    // the stopped project keeps the warning in fsck until a person approves the removal again
    let env = c.s.env_pairs();
    let fsck = human_command(&to, &env, &["--json", "fsck"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&fsck.stdout).unwrap();
    assert_eq!(
        report["warnings"][0]["issues"][0]["target"], targets[0],
        "{report}"
    );
    let stop = human_command(&to, &env, &["serve", "--stop"])
        .output()
        .unwrap();
    assert!(stop.status.success());
}
