//! hc8 (D-2FMRR, D-FR0C4, D-QF2HQ): `rdy_dump {out, after?}` writes a full dump or an increment,
//! and `rdy_load {file, dir, then?, stop_at?}` loads a chain, at a stop when one is given. The
//! tools take the extra members strictly, resolve every path against the project directory,
//! and leave the semantics to the engine.
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session};
use rdy_mcp::tools::ERROR_ROWS;
use rdy_model::Role;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::mcp::McpClient;
use serde_json::json;
use std::path::{Path, PathBuf};

const SECRET: &str = "The launch code is ZEBRA-7731.";
/// The fix of a Project's stopped-load warning.
const PROJECT_FIX: &str =
    "Propose the redaction of the listed targets again, and have a person approve it.";

/// A served project and the dump chain of its history: the full dump `f0` after the secret
/// landed (tag `v1.0.0`), the increment `i1` after its removal (tag `v2.0.0`), and `i2` after one
/// more landing (tag `v3.0.0`). The project is closed; no daemon runs.
struct Chain {
    s: Served,
    scratch: tempfile::TempDir,
    f0: PathBuf,
    i1: PathBuf,
    i2: PathBuf,
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
    r.read().dump_after(&b.chain_end(), &i2).unwrap();
    r.close().unwrap();
    Chain {
        s,
        scratch,
        f0,
        i1,
        i2,
        block,
        secret_landing,
    }
}

impl Chain {
    fn agent(&self, role: &str) -> McpClient {
        let mut c = McpClient::start_proxy(&self.s);
        c.call(
            "rdy_session",
            json!({"actor": "agent:a", "run": "a", "role": role}),
        )
        .ok();
        c
    }
}

fn arg(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn message(r: &rdy_qa::mcp::Reply) -> String {
    match r {
        rdy_qa::mcp::Reply::ToolError(v) => v["message"].as_str().unwrap_or_default().to_string(),
        other => panic!("expected a tool error, got {other:?}"),
    }
}

#[test]
fn hc8_unknown_fields_and_wrong_types_refuse() {
    let c = chain();
    let mut m = c.agent("writer");
    let dir = c.scratch.path().join("never");
    // an unknown key names the parameter it was close to, for each of the new members
    let cases = [
        ("rdy_dump", json!({"out": "x", "afters": "y"}), "after"),
        ("rdy_dump", json!({"outs": "x"}), "out"),
        (
            "rdy_load",
            json!({"file": "x", "dir": arg(&dir), "thens": []}),
            "then",
        ),
        (
            "rdy_load",
            json!({"file": "x", "dir": arg(&dir), "stop_ats": "tag:v1"}),
            "stop_at",
        ),
        ("rdy_load", json!({"file": "x", "dirs": arg(&dir)}), "dir"),
    ];
    for (tool, args, near) in cases {
        let r = m.call(tool, args.clone());
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{tool} {args}: {r:?}");
        assert!(message(&r).contains(near), "{tool} {args}: {r:?}");
    }
    // a wrong type is refused as it is; a string never becomes a one-file list
    let f0 = arg(&c.f0);
    let cases = [
        ("rdy_dump", json!({"out": "x", "after": ["a"]})),
        ("rdy_dump", json!({"out": "x", "after": 5})),
        (
            "rdy_load",
            json!({"file": f0, "dir": arg(&dir), "then": arg(&c.i1)}),
        ),
        (
            "rdy_load",
            json!({"file": f0, "dir": arg(&dir), "then": [1]}),
        ),
        (
            "rdy_load",
            json!({"file": f0, "dir": arg(&dir), "then": {"a": 1}}),
        ),
        (
            "rdy_load",
            json!({"file": f0, "dir": arg(&dir), "stop_at": 5}),
        ),
        (
            "rdy_load",
            json!({"file": f0, "dir": arg(&dir), "stop_at": ["tag:v1.0.0"]}),
        ),
        ("rdy_load", json!({"file": f0, "dir": 5})),
    ];
    for (tool, args) in cases {
        let r = m.call(tool, args.clone());
        assert_eq!(r.code(), Some("E_BAD_INPUT"), "{tool} {args}: {r:?}");
    }
    assert!(!dir.exists(), "a refused load makes no directory");
    assert!(!c.s.dir.path().join("x").exists());
    // an omitted, a null and an empty list all mean no increments; null stop_at means none
    let mut loaded = vec![];
    for (n, extra) in [
        json!({}),
        json!({"then": null, "stop_at": null}),
        json!({"then": []}),
    ]
    .into_iter()
    .enumerate()
    {
        let to = c.scratch.path().join(format!("to{n}"));
        let mut args = json!({"file": f0, "dir": arg(&to)});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let l = m.call("rdy_load", args).ok();
        assert!(l["stopped_at"].is_null(), "{l}");
        assert_eq!(l["chain"].as_array().unwrap().len(), 1, "{l}");
        loaded.push(l["batches"].clone());
    }
    assert!(loaded.windows(2).all(|w| w[0] == w[1]), "{loaded:?}");
}

#[test]
fn hc8_limit_precedes_nonexistent_paths() {
    let c = chain();
    let mut m = c.agent("writer");
    let to = c.scratch.path().join("to");
    let missing = |n: usize| -> Vec<String> { (0..n).map(|i| format!("missing/{i}")).collect() };
    // 4097 increments are over the limit; no path is opened, so none is reported missing
    let r = m.call(
        "rdy_load",
        json!({"file": "missing/full", "dir": arg(&to), "then": missing(4097)}),
    );
    assert_eq!(r.code(), Some("E_LIMIT"), "{r:?}");
    assert!(message(&r).contains("4096"), "{r:?}");
    // the count comes before the stop is parsed
    let r = m.call(
        "rdy_load",
        json!({"file": "missing/full", "dir": arg(&to), "then": missing(4097), "stop_at": "bogus"}),
    );
    assert_eq!(r.code(), Some("E_LIMIT"), "{r:?}");
    assert!(message(&r).contains("4096"), "{r:?}");
    // 4096 are within it: the first missing path is what fails
    let r = m.call(
        "rdy_load",
        json!({"file": "missing/full", "dir": arg(&to), "then": missing(4096)}),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    assert!(message(&r).contains("missing/full"), "{r:?}");
    // the stop is parsed before any path is opened
    let r = m.call(
        "rdy_load",
        json!({"file": "missing/full", "dir": arg(&to), "then": ["missing/1"], "stop_at": "bogus"}),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    assert!(message(&r).contains("bogus"), "{r:?}");
    let long = format!("tag:{}", "a".repeat(1_048_577));
    let r = m.call(
        "rdy_load",
        json!({"file": "missing/full", "dir": arg(&to), "stop_at": long}),
    );
    assert_eq!(r.code(), Some("E_LIMIT"), "{r:?}");
    assert!(!to.exists());
}

#[test]
fn hc8_any_session_may_dump_and_load() {
    let c = chain();
    for (n, role) in ["reader", "writer"].into_iter().enumerate() {
        let mut m = c.agent(role);
        let out = c.scratch.path().join(format!("all{n}.rdydump"));
        let d = m
            .call("rdy_dump", json!({"out": arg(&out), "after": arg(&c.f0)}))
            .ok();
        assert_eq!(d["kind"], "increment", "{role}: {d}");
        let to = c.scratch.path().join(format!("to{n}"));
        let l = m
            .call(
                "rdy_load",
                json!({"file": arg(&c.f0), "dir": arg(&to), "then": [arg(&out)], "stop_at": "tag:v2.0.0"}),
            )
            .ok();
        assert_eq!(l["stopped_at"]["tag"], "v2.0.0", "{role}: {l}");
    }
    // a call with no session is refused before any file is read
    let mut none = McpClient::start_proxy(&c.s);
    let r = none.call("rdy_dump", json!({"out": "n.rdydump", "after": arg(&c.f0)}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
    let r = none.call(
        "rdy_load",
        json!({"file": arg(&c.f0), "dir": "n", "then": [arg(&c.i1)], "stop_at": "tag:v1.0.0"}),
    );
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
    assert!(!c.s.dir.path().join("n").exists());
}

#[test]
fn hc8_paths_use_project_root_for_mcp_and_session_cwd_for_cli() {
    // the MCP half: a relative out, after, file, each then and dir are under the project root
    let c = chain();
    let root = c.s.dir.path();
    for (name, from) in [
        ("f0.rdydump", &c.f0),
        ("i1.rdydump", &c.i1),
        ("i2.rdydump", &c.i2),
    ] {
        std::fs::copy(from, root.join(name)).unwrap();
    }
    let mut m = c.agent("writer");
    let d = m
        .call(
            "rdy_dump",
            json!({"out": "after-f0.rdydump", "after": "f0.rdydump"}),
        )
        .ok();
    assert!(root.join("after-f0.rdydump").is_file(), "{d}");
    assert_eq!(d["out"], arg(&root.join("after-f0.rdydump")), "{d}");
    let again = m.call(
        "rdy_dump",
        json!({"out": "after-f0.rdydump", "after": "f0.rdydump"}),
    );
    assert_eq!(again.code(), Some("E_ALREADY_EXISTS"), "{again:?}");
    let l = m
        .call(
            "rdy_load",
            json!({"file": "f0.rdydump", "dir": "loaded", "then": ["i1.rdydump", arg(&c.i2)]}),
        )
        .ok();
    assert!(root.join("loaded/.rdy/store").exists(), "{l}");
    let files: Vec<&str> = l["chain"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        vec![
            arg(&root.join("f0.rdydump")),
            arg(&root.join("i1.rdydump")),
            arg(&c.i2)
        ],
        "relative paths are under the root; an absolute path stays as it is: {l}"
    );
    // a missing relative file is reported under the root, not where the daemon was started
    let r = m.call(
        "rdy_load",
        json!({"file": "f0.rdydump", "dir": "other", "then": ["gone.rdydump"]}),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
    assert!(
        message(&r).contains(arg(&root.join("gone.rdydump"))),
        "{r:?}"
    );
    assert!(!root.join("other").exists());
}

#[test]
fn hc8_stopped_warning_lists_reapprovable_targets() {
    let c = chain();
    let mut m = c.agent("writer");
    let to = c.scratch.path().join("stopped");
    let l = m
        .call(
            "rdy_load",
            json!({"file": arg(&c.f0), "dir": arg(&to), "then": [arg(&c.i1), arg(&c.i2)], "stop_at": format!("landing:{}", &c.secret_landing[..12])}),
        )
        .ok();
    assert_eq!(l["stopped_at"]["landing"], c.secret_landing, "{l}");
    let w = l["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 1, "{l}");
    assert_eq!(w[0]["code"], "W_STOP_BEFORE_REDACTION", "{l}");
    assert_eq!(w[0]["fix"], PROJECT_FIX, "{l}");
    assert!(
        w[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("This store stops before redaction apply "),
        "{l}"
    );
    let targets: Vec<&str> = w[0]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["target"].as_str().unwrap())
        .collect();
    assert_eq!(targets.len(), 1, "{l}");
    assert!(targets[0].starts_with(&c.block), "{targets:?}");
    // the listed target is one a person can approve again in the stopped project
    let mut r = Rdy::open(&to).unwrap();
    r.redact_in(
        &Session::human("human:qa".into(), Role::Writer),
        "again",
        RedactInput {
            targets: vec![targets[0].to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: true,
        },
    )
    .unwrap();
    r.close().unwrap();
    // a whole load of the same chain carries no warning
    let whole = c.scratch.path().join("whole");
    let l = m
        .call(
            "rdy_load",
            json!({"file": arg(&c.f0), "dir": arg(&whole), "then": [arg(&c.i1), arg(&c.i2)]}),
        )
        .ok();
    assert!(
        l.get("warnings").is_none() && l["stopped_at"].is_null(),
        "{l}"
    );
}

#[test]
fn hc8_error_rows_are_published_before_the_publication_rows() {
    let n = ERROR_ROWS.len();
    assert_eq!(ERROR_ROWS[n - 2].0, "E_PUBLICATION_REFERENCE");
    assert_eq!(ERROR_ROWS[n - 1].0, "W_PUBLICATION_REFERENCE");
    // Main's E_HELD_BY_READER row merged first and stays directly before the publication rows
    // (rdy-mcp tests/lock_causes.rs); these three rows sit together before both.
    let at = ERROR_ROWS
        .iter()
        .position(|(c, _, _)| *c == "E_DUMP_CHAIN")
        .expect("an E_DUMP_CHAIN row");
    assert!(at + 3 <= n - 2, "before the two publication rows");
    assert_eq!(
        ERROR_ROWS[at..at + 3],
        [
            (
                "E_DUMP_CHAIN",
                "The dump files do not form a chain, or the predecessor does not match this store.",
                "Pass a full dump and its increments in order, or start a new chain."
            ),
            (
                "E_LOAD_STOP",
                "The requested stop does not name one batch end in the dump chain.",
                "Name a landing, a tag, or a batch end that the chain holds."
            ),
            (
                "W_STOP_BEFORE_REDACTION",
                "This store stops before redaction apply {apply} of its source, approved by {actor} at landing {landing}. It holds {held} removed payloads again, and {removed} stay removed.",
                PROJECT_FIX
            ),
        ]
    );
    let codes: Vec<&str> = ERROR_ROWS.iter().map(|(c, _, _)| *c).collect();
    for code in ["E_DUMP_CHAIN", "E_LOAD_STOP", "W_STOP_BEFORE_REDACTION"] {
        assert_eq!(codes.iter().filter(|c| **c == code).count(), 1, "{code}");
    }
}
