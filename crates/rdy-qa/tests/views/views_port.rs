//! hpt4/hpr4/hpc4 (D-G4PZX, D-1QT23, D-08M5W): a replay with a source ports landed changes of
//! another project, or of a dump of one, into an open view. `rdy view replay <view> --from
//! <project-or-dump> --brief <token> <change-or-landing>...` and `rdy_view {op: "replay",
//! from | from_file, brief, changes}` go through the shared dispatcher and one port: the replies
//! match, the source never changes, and a refusal writes nothing. A direct process (`--no-daemon`)
//! holds no briefing of its own, so it refuses at the briefing, before any write.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const A: &str = "agent:a";
const RUN: &str = "r1";

const REPLAY_KEYS: [&str; 10] = [
    "view",
    "head",
    "changes",
    "replayed",
    "added",
    "added_deps",
    "removed",
    "cascaded",
    "conflicts",
    "consequences",
];

/// The seed of the source line, which the target clones: a draft doc `notes` with claim `dc`
/// and the sections `a` and `b`.
struct Seed {
    notes: String,
    dc: String,
    a: String,
    b: String,
}

/// A served target, a closed source project and its dump.
struct World {
    _tmp: tempfile::TempDir,
    s: Served,
    project: PathBuf,
    dump: PathBuf,
    seed: Seed,
}

/// The source project, seeded and landed; the served target clones it. `target` then changes
/// the target (before its daemon starts), agent a opens `views` there, and `source` writes the
/// source's later landings. The source is dumped and closed.
fn world<L>(
    views: &[&str],
    target: impl FnOnce(&mut Rdy, &Seed),
    source: impl FnOnce(&mut Rdy, &Seed) -> L,
) -> (World, L) {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("s");
    let mut src = Rdy::init(&project, "rdy-standard").unwrap();
    let w = write(
        &mut src,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "claim", "id": "$dc", "parent": "$n", "body": "A draft note."},
            {"op": "create", "type": "section", "id": "$a", "parent": "$n", "fields": {"title": "Section A"}},
            {"op": "create", "type": "section", "id": "$b", "parent": "$n", "fields": {"title": "Section B"}}
        ]),
    )
    .unwrap();
    land(&mut src, "seed").unwrap();
    let id = |k: &str| w.created[k].clone();
    let seed = Seed {
        notes: id("$n"),
        dc: id("$dc"),
        a: id("$a"),
        b: id("$b"),
    };
    let s = Served::seeded(|p| {
        clone(CloneSource::State(&src.read()), p, true).unwrap();
    });
    let mut t = Rdy::open(s.dir.path()).unwrap();
    target(&mut t, &seed);
    for v in views {
        t.view_create(&as_agent(A, RUN), v, None, false).unwrap();
    }
    t.close().unwrap();
    let l = source(&mut src, &seed);
    let dump = tmp.path().join("s.rdydump");
    src.read().dump(&dump).unwrap();
    src.close().unwrap();
    (
        World {
            _tmp: tmp,
            s,
            project,
            dump,
            seed,
        },
        l,
    )
}

fn changes_of(r: &Rdy, landing: &str) -> Vec<String> {
    r.store()
        .reader()
        .landing(&landing.parse().unwrap())
        .unwrap()
        .changes
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn cli(s: &Served, args: &[&str]) -> (bool, Value) {
    s.json_as(A, Some(RUN), args)
}

fn cli_text(s: &Served, args: &[&str]) -> (bool, String) {
    let out = s.rdy_as(A, Some(RUN), args);
    let ok = out.status.success();
    let bytes = if ok { out.stdout } else { out.stderr };
    (ok, String::from_utf8(bytes).unwrap())
}

fn mcp(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": A, "run": RUN, "role": "writer"}),
    )
    .ok();
    c
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    }
}

fn triple(e: &Value) -> (&str, &str, &str) {
    (
        e["code"].as_str().unwrap_or_default(),
        e["message"].as_str().unwrap_or_default(),
        e["fix"].as_str().unwrap_or_default(),
    )
}

/// A registered briefing of `ids` in view `view`, through the daemon: its token.
fn token(c: &mut McpClient, view: &str, ids: &[&str]) -> String {
    let b = c.call("rdy_brief", json!({"ids": ids, "scope": view})).ok();
    b["base"]["token"]
        .as_str()
        .unwrap_or_else(|| panic!("no token: {b}"))
        .to_string()
}

fn show(s: &Served, view: &str) -> Value {
    let (ok, v) = cli(s, &["--json", "view", "show", view]);
    assert!(ok, "{v}");
    v
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

/// A port surface: the daemon's command line or MCP, with the source as a project or a dump.
#[derive(Clone, Copy, Debug)]
enum Via {
    Cli,
    Mcp,
}

/// Ports `sel` from `from` into `view` through `via`, citing `brief`: the reply, or the refusal.
/// MCP names a project with `from` and a dump with `from_file`.
fn port(
    s: &Served,
    c: &mut McpClient,
    via: Via,
    from: &Path,
    view: &str,
    brief: &str,
    sel: &[String],
) -> Result<Value, Value> {
    match via {
        Via::Cli => {
            let mut args = vec![
                "--json".to_string(),
                "view".into(),
                "replay".into(),
                view.into(),
                "--from".into(),
                path(from),
                "--brief".into(),
                brief.into(),
            ];
            args.extend(sel.iter().cloned());
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match cli(s, &args) {
                (true, v) => Ok(v),
                (false, e) => Err(e),
            }
        }
        Via::Mcp => {
            let field = if from.is_dir() { "from" } else { "from_file" };
            let mut req = json!({"op": "replay", "view": view, "changes": sel, "brief": brief});
            req[field] = json!(path(from));
            match c.call("rdy_view", req) {
                Reply::ToolError(e) => Err(e),
                r => Ok(r.ok()),
            }
        }
    }
}

/// Every file under `dir`, by relative path, with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    out
}

fn same_tree(before: &BTreeMap<PathBuf, Vec<u8>>, dir: &Path) {
    let after = tree(dir);
    let differ: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    assert!(differ.is_empty(), "the source changed: {differ:?}");
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `v` with what differs between two equal ports masked: `sources` get their label, every other
/// hash is `<hash>`, a consequence id `<cq>`, a tension id `<tn>` and `view` is `<view>`.
fn norm(v: &Value, view: &str, sources: &[(String, String)]) -> Value {
    match v {
        Value::String(s) => {
            if let Some((_, label)) = sources.iter().find(|(h, _)| h == s) {
                json!(label)
            } else if is_hash(s) {
                json!("<hash>")
            } else if s.starts_with("cq-") {
                json!("<cq>")
            } else if s.starts_with("tn-") {
                json!("<tn>")
            } else if s == view {
                json!("<view>")
            } else {
                v.clone()
            }
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| norm(x, view, sources)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), norm(x, view, sources)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// The direct process: the daemon stops, and `rdy --no-daemon` ports into `view`. It refuses
/// at the briefing, with no token and with the daemon's token alike, and writes nothing.
fn direct_refuses(s: &Served, from: &Path, view: &str, daemon_token: &str, sel: &[String]) {
    let before = show(s, view);
    s.stop();
    let mut args = vec![
        "--json".to_string(),
        "--no-daemon".into(),
        "view".into(),
        "replay".into(),
        view.into(),
        "--from".into(),
        path(from),
    ];
    args.extend(sel.iter().cloned());
    let run = |args: &[String]| {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (ok, e) = cli(s, &args);
        assert!(!ok, "{e}");
        e
    };
    let e = run(&args);
    assert_eq!(
        triple(&e),
        (
            "E_BRIEF_REQUIRED",
            "a port needs a target briefing",
            "brief the target blocks and retry with the token"
        ),
        "{}",
        from.display()
    );
    args.splice(7..7, ["--brief".to_string(), daemon_token.to_string()]);
    let e = run(&args);
    assert_eq!(
        triple(&e),
        (
            "E_BRIEF_STALE",
            format!(
                "the cited briefing {} is unknown: the daemon restarted, or the briefing was evicted",
                &daemon_token[..8]
            )
            .as_str(),
            "brief these blocks (rdy_brief) and retry with the token"
        ),
        "{}",
        from.display()
    );
    assert_eq!(show(s, view), before, "nothing written");
}

// ---------------------------------------------------------------------------------------------
// parents (written first)

#[test]
fn hpc4_project_and_dump_parent_checks() {
    const CASES: [&str; 4] = ["missing", "clean", "conflicted", "loop"];
    const SURFACES: [&str; 4] = ["cli-project", "cli-dump", "mcp-project", "mcp-dump"];
    let views: Vec<String> = CASES
        .iter()
        .flat_map(|c| SURFACES.iter().map(move |s| format!("{c}-{s}")))
        .chain(["direct".to_string()])
        .collect();
    let names: Vec<&str> = views.iter().map(String::as_str).collect();
    let (w, (sec, l2, l3, l4, l5)) = world(
        &names,
        |t, seed| {
            // the target retitles the notes doc its own way and puts b under a, on canon
            write(
                t,
                "title",
                json!([{"op": "update", "id": seed.notes, "fields": {"title": "Notes, in the target"}}]),
            )
            .unwrap();
            land(t, "title").unwrap();
            write(
                t,
                "mv",
                json!([{"op": "move", "id": seed.b, "parent": seed.a}]),
            )
            .unwrap();
            land(t, "mv").unwrap();
        },
        |s, seed| {
            // l2 creates a section; l3 puts a claim under it; l4 retitles the notes doc and puts
            // a claim under it; l5 moves a under b
            let w = write(
                s,
                "p",
                json!([{"op": "create", "type": "section", "id": "$p", "parent": seed.notes, "fields": {"title": "New section"}}]),
            )
            .unwrap();
            let sec = w.created["$p"].clone();
            let l2 = land(s, "p").unwrap();
            write(
                s,
                "q",
                json!([{"op": "create", "type": "claim", "parent": sec, "body": "Under it."}]),
            )
            .unwrap();
            let l3 = land(s, "q").unwrap();
            write(
                s,
                "r",
                json!([
                    {"op": "update", "id": seed.notes, "fields": {"title": "Notes, in the source"}},
                    {"op": "create", "type": "claim", "parent": seed.notes, "body": "Under notes."}
                ]),
            )
            .unwrap();
            let l4 = land(s, "r").unwrap();
            write(
                s,
                "m",
                json!([{"op": "move", "id": seed.a, "parent": seed.b}]),
            )
            .unwrap();
            let l5 = land(s, "m").unwrap();
            (sec, l2, l3, l4, l5)
        },
    );
    let s = &w.s;
    let notes = w.seed.notes.clone();
    let (a, b) = (w.seed.a.clone(), w.seed.b.clone());
    let before_source = tree(&w.project);
    let mut c = mcp(s);
    let mut last = String::new();
    for surface in SURFACES {
        let via = if surface.starts_with("cli") {
            Via::Cli
        } else {
            Via::Mcp
        };
        let from = if surface.ends_with("project") {
            &w.project
        } else {
            &w.dump
        };
        let mut run = |case: &str, sel: &[&String]| {
            let view = format!("{case}-{surface}");
            let tok = token(&mut c, &view, &[&notes]);
            last = tok.clone();
            let before = show(s, &view);
            let sel: Vec<String> = sel.iter().map(|x| x.to_string()).collect();
            let out = port(s, &mut c, via, from, &view, &tok, &sel);
            if out.is_err() {
                assert_eq!(show(s, &view), before, "{view}: a refusal writes nothing");
            }
            out
        };
        // a missing parent
        let e = run("missing", &[&l3]).unwrap_err();
        assert_eq!(
            triple(&e),
            (
                "E_DEPS_MISSING",
                format!("the port needs parent {sec} present and clean in the target").as_str(),
                "carry the change that creates the parent first, or resolve its conflict"
            ),
            "{surface}"
        );
        // the parent created cleanly in the same call
        let ok = run("clean", &[&l2, &l3]).unwrap();
        assert_eq!(
            ok["replayed"].as_array().unwrap().len(),
            2,
            "{surface}: {ok}"
        );
        assert_eq!(ok["conflicts"], json!([]), "{surface}: {ok}");
        // a conflicted parent
        let e = run("conflicted", &[&l4]).unwrap_err();
        assert_eq!(
            triple(&e),
            (
                "E_DEPS_MISSING",
                format!("the port needs parent {notes} present and clean in the target").as_str(),
                "carry the change that creates the parent first, or resolve its conflict"
            ),
            "{surface}"
        );
        // a move that makes a loop only against the target
        let e = run("loop", &[&l5]).unwrap_err();
        let one = format!("the port would make {a} and {b} each other's ancestors");
        let other = format!("the port would make {b} and {a} each other's ancestors");
        assert_eq!(e["code"], "E_BAD_INPUT", "{surface}: {e}");
        assert_eq!(
            e["fix"],
            "move one block out of the other in the target, then carry again"
        );
        assert!(
            e["message"] == one || e["message"] == other,
            "{surface}: {e}"
        );
    }
    drop(c);
    // the direct process reaches the same port and refuses before any write
    for from in [&w.project, &w.dump] {
        direct_refuses(s, from, "direct", &last, std::slice::from_ref(&l3));
    }
    same_tree(&before_source, &w.project);
}

// ---------------------------------------------------------------------------------------------
// one port on every surface

#[test]
fn hpt4_local_and_dump_ports_match() {
    const VIEWS: [&str; 5] = [
        "cli-project",
        "cli-dump",
        "mcp-project",
        "mcp-dump",
        "direct",
    ];
    let (w, (repo, l2, l3, src)) = world(
        &VIEWS,
        |_, _| {},
        |s, seed| {
            write(
                s,
                "a",
                json!([
                    {"op": "create", "type": "claim", "parent": seed.notes, "body": "New."},
                    {"op": "update", "id": seed.dc, "body": "dc two"}
                ]),
            )
            .unwrap();
            let l2 = land(s, "a").unwrap();
            write(
                s,
                "b",
                json!([{"op": "update", "id": seed.dc, "body": "dc three"}]),
            )
            .unwrap();
            let l3 = land(s, "b").unwrap();
            let mut src = changes_of(s, &l2);
            src.extend(changes_of(s, &l3));
            (s.read().repo_id().unwrap().to_string(), l2, l3, src)
        },
    );
    let s = &w.s;
    let before_source = tree(&w.project);
    let dump_bytes = std::fs::read(&w.dump).unwrap();
    let sources: Vec<(String, String)> = src
        .iter()
        .enumerate()
        .map(|(i, h)| (h.clone(), format!("<src{i}>")))
        .collect();
    let mut c = mcp(s);
    let mut replies = vec![];
    let mut tok = String::new();
    // a change of the later landing first, then the earlier landing: oldest first, once each
    let sel = vec![src[2].clone(), l2.clone(), l3.clone()];
    for (view, via, from) in [
        ("cli-project", Via::Cli, &w.project),
        ("cli-dump", Via::Cli, &w.dump),
        ("mcp-project", Via::Mcp, &w.project),
        ("mcp-dump", Via::Mcp, &w.dump),
    ] {
        tok = token(&mut c, view, &[&w.seed.notes]);
        let reply =
            port(s, &mut c, via, from, view, &tok, &sel).unwrap_or_else(|e| panic!("{view}: {e}"));
        for key in REPLAY_KEYS {
            assert!(reply.get(key).is_some(), "{view}: no {key}: {reply}");
        }
        assert_eq!(reply["view"], view);
        let replayed = reply["replayed"].as_array().unwrap();
        let from_sources: Vec<&str> = replayed
            .iter()
            .map(|r| r["source"].as_str().unwrap())
            .collect();
        assert_eq!(from_sources, src, "{view}");
        let added: Vec<&str> = replayed
            .iter()
            .map(|r| r["change"].as_str().unwrap())
            .collect();
        assert_eq!(reply["added"], json!(added), "{view}");
        // one include op, and each row names its source project and change
        let shown = show(s, view);
        assert_eq!(shown["ops"][0]["id"], reply["head"], "{view}: {shown}");
        assert_eq!(shown["ops"][0]["kind"], "include", "{view}: {shown}");
        for (row, from) in shown["changes"].as_array().unwrap().iter().zip(&src) {
            assert_eq!(row["replayed_from"], Value::Null, "{row}");
            assert_eq!(
                row["ported_from"],
                json!({"repo_id": repo, "change": from}),
                "{view}: {row}"
            );
        }
        let (ok, text) = cli_text(s, &["view", "show", view]);
        assert!(ok, "{text}");
        for (r, from) in replayed.iter().zip(&src) {
            let id = r["change"].as_str().unwrap();
            let line = text
                .lines()
                .find(|l| l.starts_with(&format!("  {} ", &id[..12])))
                .unwrap_or_else(|| panic!("no heading for {id}: {text}"));
            assert!(
                line.ends_with(&format!(" ported from {repo} {}", &from[..12])),
                "{line}"
            );
            assert!(!line.contains("[wc]"), "{line}");
        }
        replies.push(norm(&reply, view, &sources));
    }
    // one shape and one result on every surface
    for r in &replies[1..] {
        assert_eq!(r, &replies[0]);
    }
    drop(c);
    direct_refuses(s, &w.project, "direct", &tok, std::slice::from_ref(&l2));
    direct_refuses(s, &w.dump, "direct", &tok, std::slice::from_ref(&l2));
    // the source and its dump did not change
    same_tree(&before_source, &w.project);
    assert_eq!(std::fs::read(&w.dump).unwrap(), dump_bytes);
    // the text form refuses a briefing with no source, before any write
    let before = show(s, "cli-project");
    let (ok, e) = cli(
        s,
        &[
            "--json",
            "view",
            "replay",
            "cli-project",
            "--brief",
            &tok,
            &l2,
        ],
    );
    assert!(!ok, "{e}");
    assert_eq!(
        triple(&e),
        (
            "E_BAD_INPUT",
            "replay without a source does not take brief",
            "remove brief, or name a port source"
        )
    );
    assert_eq!(show(s, "cli-project"), before);
}

// ---------------------------------------------------------------------------------------------
// the input rules

#[test]
fn hpt4_unknown_fields_and_selectors_refuse_before_any_write() {
    let (w, l2) = world(
        &["mine"],
        |_, _| {},
        |s, seed| {
            write(
                s,
                "a",
                json!([{"op": "update", "id": seed.dc, "body": "dc two"}]),
            )
            .unwrap();
            land(s, "a").unwrap()
        },
    );
    let s = &w.s;
    let mut c = mcp(s);
    let before = show(s, "mine");
    let dump = path(&w.dump);
    let proj = path(&w.project);
    // misspelt fields: a did-you-mean before any rule
    for (bad, good) in [("from_flie", "from_file"), ("breif", "brief")] {
        let mut req = json!({"op": "replay", "view": "mine", "changes": [l2], "from_file": dump});
        req[bad] = json!("x");
        let e = tool_error(c.call("rdy_view", req));
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(
            e["message"],
            format!("rdy_view: unknown parameter \"{bad}\""),
            "{e}"
        );
        assert!(
            e["fix"].as_str().unwrap().starts_with(&format!(
                "did you mean \"{good}\"? the parameters here are "
            )),
            "{e}"
        );
    }
    // two selectors; a briefing with no source; a source field on another op
    for (req, want) in [
        (
            json!({"op": "replay", "view": "mine", "changes": [l2], "from": proj, "from_file": dump}),
            (
                "replay takes only one source",
                "pass from or from_file, not both",
            ),
        ),
        (
            json!({"op": "replay", "view": "mine", "changes": [l2], "brief": "ab"}),
            (
                "replay without a source does not take brief",
                "remove brief, or name a port source",
            ),
        ),
        (
            json!({"op": "include", "view": "mine", "changes": [l2], "from_file": dump}),
            (
                "include does not take from_file",
                "use the source field only with replay",
            ),
        ),
        (
            json!({"op": "show", "view": "mine", "brief": "ab"}),
            (
                "show does not take brief",
                "use the source field only with replay",
            ),
        ),
    ] {
        let e = tool_error(c.call("rdy_view", req.clone()));
        assert_eq!(triple(&e), ("E_BAD_INPUT", want.0, want.1), "{req}");
    }
    // a view of another actor
    let tok = token(&mut c, "mine", &[&w.seed.notes]);
    let (ok, e) = s.json_as(
        "agent:z",
        Some("z1"),
        &[
            "--json", "view", "replay", "mine", "--from", &proj, "--brief", &tok, &l2,
        ],
    );
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_VIEW_OWNER", "{e}");
    // an unlanded or unknown change
    let e = tool_error(c.call(
        "rdy_view",
        json!({"op": "replay", "view": "mine", "changes": ["ab".repeat(32)], "from_file": dump, "brief": tok}),
    ));
    assert_eq!(e["code"], "E_NOT_FOUND", "{e}");
    drop(c);
    assert_eq!(show(s, "mine"), before, "nothing written");

    // the daemon's op for the command line says the same
    let (dir, r) = project();
    let local = Local::new(r);
    let me = as_agent(A, RUN);
    let cx = OpContext {
        engine: &local,
        session: &me,
        human: "human:qa",
        cwd: dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    for (bad, good) in [("from_flie", "from_file"), ("breif", "brief")] {
        let mut req = json!({"op": "replay", "view": "mine", "changes": ["x"]});
        req[bad] = json!("x");
        let err = dispatch(&cx, "view", req).unwrap_err();
        assert_eq!(
            err.message,
            format!("view: unknown field \"{bad}\""),
            "{err}"
        );
        assert!(
            err.fix
                .starts_with(&format!("did you mean \"{good}\"? the fields are ")),
            "{err}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// ruled ops (D-1QT23, D-08M5W)

#[test]
fn hpr4_local_dump_follow_ruled_ops_and_pins() {
    let views = [
        "del-cli",
        "del-mcp",
        "late-cli",
        "late-mcp",
        "empty-cli",
        "empty-mcp",
        "again",
    ];
    let (w, (m, l3, l5, l6)) = world(
        &views,
        |_, _| {},
        |s, seed| {
            // l2: the note and a new claim m; l3 edits both; l4 adds k, which l5 deletes; l6
            // deletes the note
            let w = write(
                s,
                "a",
                json!([
                    {"op": "update", "id": seed.dc, "body": "one"},
                    {"op": "create", "type": "claim", "id": "$m", "parent": seed.notes, "body": "m one"}
                ]),
            )
            .unwrap();
            let m = w.created["$m"].clone();
            land(s, "a").unwrap();
            write(
                s,
                "b",
                json!([
                    {"op": "update", "id": seed.dc, "body": "two"},
                    {"op": "update", "id": m, "body": "m two"}
                ]),
            )
            .unwrap();
            let l3 = land(s, "b").unwrap();
            let k = write(
                s,
                "k",
                json!([{"op": "create", "type": "claim", "id": "$k", "parent": seed.notes, "body": "k"}]),
            )
            .unwrap()
            .created["$k"]
                .clone();
            land(s, "k").unwrap();
            write(s, "dk", json!([{"op": "delete", "id": k}])).unwrap();
            let l5 = land(s, "dk").unwrap();
            write(s, "dd", json!([{"op": "delete", "id": seed.dc}])).unwrap();
            let l6 = land(s, "dd").unwrap();
            (m, l3, l5, l6)
        },
    );
    let s = &w.s;
    let dc = w.seed.dc.clone();
    let mut c = mcp(s);
    let tok = |c: &mut McpClient, view: &str| token(c, view, &[&w.seed.notes]);
    // a delete whose base the target lacks, of a block it holds: refused, nothing written
    for (view, via, from) in [
        ("del-cli", Via::Cli, &w.project),
        ("del-mcp", Via::Mcp, &w.dump),
    ] {
        let before = show(s, view);
        let t = tok(&mut c, view);
        let e = port(s, &mut c, via, from, view, &t, std::slice::from_ref(&l6)).unwrap_err();
        assert_eq!(
            triple(&e),
            (
                "E_BAD_INPUT",
                "the target line lacks the base of a ported delete",
                "carry the landing that wrote it first"
            ),
            "{view}"
        );
        assert_eq!(show(s, view), before, "{view}");
    }
    // missing bases become creates: the held note conflicts, the never-held claim applies
    for (view, via, from) in [
        ("late-cli", Via::Cli, &w.dump),
        ("late-mcp", Via::Mcp, &w.project),
    ] {
        let t = tok(&mut c, view);
        let reply = port(s, &mut c, via, from, view, &t, std::slice::from_ref(&l3)).unwrap();
        let blocks: Vec<&str> = reply["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["block"].as_str().unwrap())
            .collect();
        assert_eq!(blocks, [dc.as_str()], "{view}: {reply}");
        let shown = show(s, view);
        let mut got: Vec<&str> = shown["changes"][0]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_str().unwrap())
            .collect();
        got.sort();
        let mut want = vec![dc.as_str(), m.as_str()];
        want.sort();
        assert_eq!(got, want, "{view}: {shown}");
    }
    // a missing-base delete of a block the target never held: one empty change keeps its place
    for (view, via, from) in [
        ("empty-cli", Via::Cli, &w.project),
        ("empty-mcp", Via::Mcp, &w.dump),
    ] {
        let t = tok(&mut c, view);
        let reply = port(s, &mut c, via, from, view, &t, std::slice::from_ref(&l5)).unwrap();
        assert_eq!(reply["replayed"].as_array().unwrap().len(), 1, "{reply}");
        let shown = show(s, view);
        assert_eq!(shown["head"]["changes"], 1, "{shown}");
        assert_eq!(shown["changes"][0]["blocks"], json!([]), "{shown}");
    }
    drop(c);
    // direct replay ancestry: a replay of a ported change names it and keeps its origin
    let ported = show(s, "late-cli")["changes"][0].clone();
    let id = ported["id"].as_str().unwrap();
    let (ok, reply) = cli(s, &["--json", "view", "replay", "again", id]);
    assert!(ok, "{reply}");
    let again = show(s, "again")["changes"][0].clone();
    assert_eq!(again["replayed_from"], json!(id), "{again}");
    assert_eq!(again["ported_from"], ported["ported_from"], "{again}");
}

// ---------------------------------------------------------------------------------------------
// limits

#[test]
fn hpr4_local_limits_stay_separate() {
    const MIB: usize = 1 << 20;
    let (w, (ops, bytes)) = world(
        &["mine"],
        |_, _| {},
        |s, seed| {
            // over 10,000 ops in one change
            let many: Vec<Value> = (0..10_001)
                .map(|i| json!({"op": "create", "type": "claim", "parent": seed.a, "body": format!("Claim {i}.")}))
                .collect();
            write(s, "ops", json!(many)).unwrap();
            let ops = land(s, "ops").unwrap();
            // over 16,777,216 stored bytes in five changes
            let body = "x".repeat(MIB);
            for _ in 0..5 {
                let four: Vec<Value> = (0..4)
                    .map(|_| json!({"op": "create", "type": "claim", "parent": seed.b, "body": body}))
                    .collect();
                write(s, "bytes", json!(four)).unwrap();
            }
            let bytes = land(s, "bytes").unwrap();
            (ops, bytes)
        },
    );
    let s = &w.s;
    let limit = (
        "E_LIMIT",
        "replay accepts at most 1000 changes, 10000 ops and 16777216 encoded bytes",
        "replay a smaller set of changes",
    );
    let mut c = mcp(s);
    let before = show(s, "mine");
    let tok = token(&mut c, "mine", &[&w.seed.notes]);
    // changes: 1001 entries, counted before the source opens
    let many = vec![ops.clone(); 1001];
    let e = port(s, &mut c, Via::Mcp, &w.dump, "mine", &tok, &many).unwrap_err();
    assert_eq!(triple(&e), limit, "changes");
    // a Windows command line holds 32 KiB at most: 1001 full hashes do not fit, their starts do
    let short: Vec<String> = many.iter().map(|h| h[..12].to_string()).collect();
    let e = port(
        s,
        &mut c,
        Via::Cli,
        &w.project,
        "mine",
        &tok,
        if cfg!(windows) { &short } else { &many },
    )
    .unwrap_err();
    assert_eq!(triple(&e), limit, "changes, command line");
    // ops and bytes, from a project and a dump: the replay limits, not the hub's
    for (what, sel) in [("ops", &ops), ("bytes", &bytes)] {
        for (via, from) in [(Via::Cli, &w.project), (Via::Mcp, &w.dump)] {
            let e = port(
                s,
                &mut c,
                via,
                from,
                "mine",
                &tok,
                std::slice::from_ref(sel),
            )
            .unwrap_err();
            assert_eq!(triple(&e), limit, "{what} {via:?}");
        }
    }
    drop(c);
    assert_eq!(show(s, "mine"), before, "nothing written");
}
