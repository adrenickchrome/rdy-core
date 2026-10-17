//! ec12: `rdy_clone {dir, file?, fork?}` gives an agent `rdy clone` (D-9QV85, D-D2NPN,
//! D-KQ4CA): this project, or a dump file, into a directory with no store. Any session may call
//! it (O6). Relative paths resolve against the project directory.
use rdy_engine::Rdy;
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::CloneParams;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The `next` of every clone (O9).
const NEXT: &str = "run rdy checkout to bind a working copy; a clone carries none";

/// A served project holding the golden corpus, landed and tagged `v1.0.0`, and its repo id.
fn golden_served() -> (Served, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let id = r.read().repo_id().unwrap().to_string();
    r.close().unwrap();
    (s, id)
}

fn agent(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    c
}

/// The code, message and fix of a tool error.
fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// Every path under `dir` with its bytes (a directory maps to none).
fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    tree_but(dir, &[])
}

/// `tree`, without the paths under each of `skip`.
fn tree_but(dir: &Path, skip: &[&str]) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if skip
                .iter()
                .any(|k| rel == *k || rel.starts_with(&format!("{k}/")))
            {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                stack.push(p.clone());
                out.insert(rel, None);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    out
}

/// The checks every clone reply passes: the summary, the repo id and the origin of `fork`.
fn check_reply(v: &Value, source_id: &str, fork: bool) {
    assert_eq!(v["fork"], fork, "{v}");
    assert_eq!(v["next"], NEXT, "{v}");
    assert_eq!(v["fsck"]["unlogged_moves"], 0, "{v}");
    assert!(v["batches"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(v["tags"], 1, "{v}");
    let (key, other) = if fork {
        ("forked_from", "origin")
    } else {
        ("origin", "forked_from")
    };
    assert_eq!(v[key]["repo_id"], source_id, "{v}");
    assert_eq!(v[key]["tag"], "v1.0.0", "{v}");
    assert!(v[key]["landing"].is_string(), "{v}");
    assert!(v.get(other).is_none(), "{v}");
    if fork {
        assert_ne!(v["repo_id"], source_id, "a fork takes a new id: {v}");
    } else {
        assert_eq!(v["repo_id"], source_id, "a clone keeps the id: {v}");
    }
}

#[test]
fn ec12_rdy_clone_clones_this_project() {
    let (s, id) = golden_served();
    let mut c = agent(&s);
    let o = c.call("rdy_overview", json!({})).ok();
    assert_eq!(o["project"]["repo_id"], id.as_str(), "{o}");

    // a relative dir resolves against the project directory
    let v = c.call("rdy_clone", json!({"dir": "copy"})).ok();
    check_reply(&v, &id, false);
    assert!(s.dir.path().join("copy/.rdy/store").is_dir(), "{v}");
    assert!(
        !s.dir.path().join("copy/.rdy/local.yaml").exists(),
        "a clone never carries the local file"
    );

    // an absolute dir is used as given; a fork takes a new id
    let scratch = short_tempdir();
    let fork = scratch.path().join("fork");
    let v = c
        .call(
            "rdy_clone",
            json!({"dir": fork.to_str().unwrap(), "fork": true}),
        )
        .ok();
    check_reply(&v, &id, true);
    let explicit = scratch.path().join("explicit");
    let v = c
        .call(
            "rdy_clone",
            json!({"dir": explicit.to_str().unwrap(), "fork": false}),
        )
        .ok();
    check_reply(&v, &id, false);

    // the clones open, keep the version, and name their origin
    drop(c);
    s.stop();
    let copy = Rdy::open(&s.dir.path().join("copy")).unwrap();
    assert_eq!(copy.read().repo_id().unwrap().to_string(), id);
    let origin = copy.read().origin().unwrap().expect("an origin");
    assert!(!origin.fork);
    assert_eq!(origin.tag.as_deref(), Some("v1.0.0"));
    copy.close().unwrap();
    let f = Rdy::open(&fork).unwrap();
    assert_ne!(f.read().repo_id().unwrap().to_string(), id);
    assert!(f.read().origin().unwrap().expect("an origin").fork);
    f.close().unwrap();
}

#[test]
fn ec12_rdy_clone_with_file_clones_a_dump() {
    let (s, id) = golden_served();
    let mut c = agent(&s);
    let d = c.call("rdy_dump", json!({"out": "p.rdydump"})).ok();
    let scratch = short_tempdir();

    // a relative file resolves against the project directory
    let to = scratch.path().join("from-file");
    let v = c
        .call(
            "rdy_clone",
            json!({"dir": to.to_str().unwrap(), "file": "p.rdydump"}),
        )
        .ok();
    check_reply(&v, &id, false);
    assert_eq!(
        v["batches"], d["batches"],
        "the source's log, then the origin batch: {v}"
    );
    assert!(to.join(".rdy/store").is_dir());

    // an absolute file, forked
    let file = s.dir.path().join("p.rdydump");
    let fork = scratch.path().join("fork");
    let v = c
        .call(
            "rdy_clone",
            json!({"dir": fork.to_str().unwrap(), "file": file.to_str().unwrap(), "fork": true}),
        )
        .ok();
    check_reply(&v, &id, true);

    // a clone of this project and a clone of its dump record the same origin
    let direct = c
        .call(
            "rdy_clone",
            json!({"dir": scratch.path().join("direct").to_str().unwrap()}),
        )
        .ok();
    let from_file = c
        .call(
            "rdy_clone",
            json!({"dir": scratch.path().join("again").to_str().unwrap(), "file": "p.rdydump"}),
        )
        .ok();
    assert_eq!(
        direct["origin"], from_file["origin"],
        "{direct} {from_file}"
    );
}

#[test]
fn ec12_rdy_clone_with_file_refuses_a_directory() {
    let (s, _) = golden_served();
    let mut c = agent(&s);
    let scratch = short_tempdir();
    let to = scratch.path().join("to");
    let to_arg = to.to_str().unwrap();

    // a directory, absolute or relative, even a project: never a dump (O39)
    let plain = scratch.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    for (file, shown) in [
        (plain.to_str().unwrap().to_string(), plain.clone()),
        (
            s.dir.path().to_str().unwrap().to_string(),
            s.dir.path().to_path_buf(),
        ),
        (".".to_string(), s.dir.path().join(".")),
    ] {
        let (code, message, fix) =
            refused(c.call("rdy_clone", json!({"dir": to_arg, "file": file})));
        assert_eq!(code, "E_BAD_INPUT", "{file}");
        assert_eq!(
            message,
            format!("{} is not a dump file", shown.display()),
            "{file}"
        );
        assert_eq!(
            fix,
            "to clone another project, call rdy_dump on its own server, then pass the file here"
        );
        assert!(!to.exists(), "{file}: the target is untouched");
    }

    // any other path follows O7: a URL, and a path that is nothing
    let (code, message, fix) = refused(c.call(
        "rdy_clone",
        json!({"dir": to_arg, "file": "rdy://hub/team/project"}),
    ));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(
        message,
        "rdy://hub/team/project is a URL: cloning from a hub comes with the sync client"
    );
    assert_eq!(
        fix,
        "run rdy dump --out <file> in that project, then clone the file"
    );
    let missing = scratch.path().join("missing.rdydump");
    let (code, message, fix) = refused(c.call(
        "rdy_clone",
        json!({"dir": to_arg, "file": missing.to_str().unwrap()}),
    ));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(
        message,
        format!(
            "{} is neither a dump file nor an rdy project",
            missing.display()
        )
    );
    assert_eq!(
        fix,
        "pass a dump file, or a directory with .rdy.yaml and .rdy/store"
    );
    assert!(!to.exists(), "the target is untouched");
}

#[test]
fn ec12_rdy_clone_params_are_strict() {
    let bad = |args: Value| {
        parse_params::<CloneParams>("rdy_clone", args.as_object().unwrap().clone())
            .expect_err("a refusal")
    };
    let e = bad(json!({"dirs": "x"}));
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("dir"), "{}", e.message);
    for args in [
        json!({}),
        json!({"file": "p.rdydump"}),
        json!({"dir": "x", "fork": "yes"}),
        json!({"dir": "x", "source": "p.rdydump"}),
        json!({"dir": "x", "url": "rdy://hub/p"}),
    ] {
        assert_eq!(bad(args.clone()).code, "E_BAD_INPUT", "{args}");
    }
    let ok = parse_params::<CloneParams>(
        "rdy_clone",
        json!({"dir": "x", "file": "p.rdydump", "fork": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(ok.is_ok(), "{ok:?}");

    // over the wire: a session is required, and an unknown key writes nothing
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    let (code, ..) = refused(c.call("rdy_clone", json!({"dir": "copy"})));
    assert_eq!(code, "E_NO_SESSION");
    assert!(!s.dir.path().join("copy").exists());
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let (code, ..) = refused(c.call("rdy_clone", json!({"dir": "copy", "forks": true})));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(!s.dir.path().join("copy").exists());

    // the tool says what it does, exactly
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_clone")
        .expect("rdy_clone is listed");
    assert_eq!(
        t["description"],
        "Clone this project, or the dump file named by file, into dir, a directory with no rdy store. The clone keeps every hash, landing, tag and view, keeps the repo id and records its origin; fork gives it a new repo id. To clone another project, call rdy_dump on its own server, then pass the file here."
    );
}

#[test]
fn ec12_rdy_clone_into_a_store_is_refused() {
    let (s, _) = golden_served();
    let other = Served::new();
    let mut c = agent(&s);
    c.call("rdy_dump", json!({"out": "p.rdydump"})).ok();

    // this project, and another project: each keeps every byte. This project's .rdy is its
    // daemon's to write; a clone would write .rdy.yaml first, and the other project has no
    // daemon, so its whole tree is compared.
    let mine = |s: &Served| tree_but(s.dir.path(), &[".rdy"]);
    let before_other = tree(other.dir.path());
    let other_dir = other.dir.path().to_str().unwrap();
    for args in [
        json!({"dir": "."}),
        json!({"dir": ".", "fork": true}),
        json!({"dir": other_dir}),
        json!({"dir": other_dir, "file": "p.rdydump"}),
        json!({"dir": other_dir, "fork": true}),
    ] {
        let before = mine(&s);
        let (code, message, _) = refused(c.call("rdy_clone", args.clone()));
        assert_eq!(code, "E_ALREADY_EXISTS", "{args}: {message}");
        assert!(
            message.ends_with(" already has a store"),
            "{args}: {message}"
        );
        assert_eq!(mine(&s), before, "{args}: this project is untouched");
        assert_eq!(
            tree(other.dir.path()),
            before_other,
            "{args}: the other project is untouched"
        );
    }
    let o = c.call("rdy_overview", json!({})).ok();
    assert!(o["project"].is_object(), "the project still serves: {o}");
}
