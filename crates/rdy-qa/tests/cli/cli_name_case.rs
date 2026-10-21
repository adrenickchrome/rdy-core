//! plc4: a doc or folder name that is a `d` or `fd` block id in other case is refused the same
//! way by the CLI, `rdy_write` and a checked tree. Other prefixes keep the exact-case rule.
use rdy_engine::check_path;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;

const LOOKALIKE: &str = "a name cannot look like a block id (it would be read as one)";
const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: T\n    doc_type: page\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Terms\n";
const FOLDER: &str = "id: fd-AAAAA\ntype: folder\norder: a1\n";
const BAD: [&str; 2] = ["d-abcde", "fd-7kq2m"];

fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

/// What one refusal says: its code, its message, and the name it proposes in place.
struct Refusal {
    code: String,
    message: String,
    fixed: String,
}

fn from_cli(v: &Value) -> Refusal {
    Refusal {
        code: v["code"].as_str().unwrap_or_default().to_string(),
        message: v["message"].as_str().unwrap_or_default().to_string(),
        fixed: v["corrected"]["fields"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    }
}

/// The name in a fix of the form `use a name like "<name>"`.
fn fix_name(fix: &str) -> String {
    fix.strip_prefix("use a name like \"")
        .and_then(|f| f.strip_suffix('"'))
        .unwrap_or_else(|| panic!("unexpected fix {fix:?}"))
        .to_string()
}

fn assert_refused(r: &Refusal, bad: &str, what: &str) {
    assert_eq!(r.code, "E_BAD_NAME", "{what}: {bad}");
    assert_eq!(
        r.message,
        format!("{bad:?} is not a valid name: {LOOKALIKE}"),
        "{what}: {bad}"
    );
    assert!(r.fixed.starts_with("untitled-"), "{what}: {}", r.fixed);
    assert_eq!(
        rdy_model::tree::name_problem(&r.fixed),
        None,
        "{what}: the correction is valid"
    );
}

fn tree(doc_stem: Option<&str>, folder: Option<&str>) -> tempfile::TempDir {
    let t = short_tempdir();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    if let Some(stem) = doc_stem {
        std::fs::write(t.path().join("docs").join(format!("{stem}.yaml")), DOC).unwrap();
    }
    if let Some(f) = folder {
        let dir = t.path().join("docs").join(f);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("_folder.yaml"), FOLDER).unwrap();
    }
    t
}

/// The open views with their change counts: a refused write leaves it as it was.
fn log_of(s: &Served) -> Value {
    let (ok, v) = s.json(&["--json", "view", "ls"]);
    assert!(ok, "{v}");
    v
}

#[test]
fn plc4_names_have_cli_mcp_parity() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let before = log_of(&s);
    for bad in BAD {
        let mut seen = vec![];
        for kind in ["folder", "doc"] {
            let mut args = vec!["--json", "new", kind, bad];
            if kind == "doc" {
                args.extend(["--type", "page", "--title", "T"]);
            }
            let (ok, v) = s.json(&args);
            assert!(!ok, "cli new {kind} {bad}: {v}");
            let r = from_cli(&v);
            assert_refused(&r, bad, &format!("cli new {kind}"));
            assert_eq!(v["fix"], json!(format!("use a name like {:?}", r.fixed)));
            seen.push(r);
        }
        let ops = [
            json!([{"op": "create", "type": "folder", "fields": {"name": bad}}]),
            json!([{"op": "create", "type": "doc", "fields": {"name": bad, "title": "T", "doc_type": "page"}}]),
        ];
        for ops in ops {
            let reply = c.call("rdy_write", json!({"ops": ops}));
            assert_eq!(reply.code(), Some("E_BAD_NAME"), "{bad}: {reply:?}");
            let v = match reply {
                rdy_qa::mcp::Reply::ToolError(v) => v,
                other => panic!("{other:?}"),
            };
            let r = from_cli(&v);
            assert_refused(&r, bad, "rdy_write create");
            assert_eq!(fix_name(v["fix"].as_str().unwrap()), r.fixed);
            assert_eq!(
                v["corrected"],
                json!({"op": 0, "fields": {"name": r.fixed}})
            );
        }
    }
    // a rename onto each name, from valid docs and folders
    let seeded = c
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "seed-folder"}},
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "seed-doc", "title": "S", "doc_type": "page"}}
            ]}),
        )
        .ok();
    let after_seed = log_of(&s);
    for bad in BAD {
        for key in ["$f", "$d"] {
            let id = seeded["created"][key].as_str().unwrap();
            let reply = c.call(
                "rdy_write",
                json!({"ops": [{"op": "update", "id": id, "fields": {"name": bad}}]}),
            );
            assert_eq!(reply.code(), Some("E_BAD_NAME"), "{bad}: {reply:?}");
            let rdy_qa::mcp::Reply::ToolError(v) = reply else {
                panic!("not a tool error")
            };
            let r = from_cli(&v);
            assert_refused(&r, bad, "rdy_write rename");
            assert_eq!(fix_name(v["fix"].as_str().unwrap()), r.fixed);
        }
    }
    assert_ne!(before, after_seed, "the seed write is the only change");
    assert_eq!(log_of(&s), after_seed, "every refusal left the log alone");
}

#[test]
fn plc4_tree_check_refuses_case_folded_id_stems() {
    let s = Served::new();
    let before = log_of(&s);
    for (bad, t) in [
        ("d-abcde", tree(Some("d-abcde"), None)),
        ("fd-7kq2m", tree(None, Some("fd-7kq2m"))),
    ] {
        let diags = check_path(&pack(), None, t.path()).unwrap();
        // the file-name check at (1,1); the block check on the entry adds its own line
        let hit: Vec<_> = diags
            .iter()
            .filter(|d| d.code == "E_BAD_NAME" && (d.line, d.column) == (1, 1))
            .collect();
        assert_eq!(hit.len(), 1, "{bad}: {diags:#?}");
        let d = hit[0];
        assert_eq!((d.line, d.column), (1, 1), "{d:?}");
        assert_eq!(
            d.message,
            format!("{bad:?} is not a valid name: {LOOKALIKE}")
        );
        assert_eq!(d.fix, "rename it, e.g. to \"untitled-00000\"");
        assert!(d.file.contains(bad), "{d:?}");

        let dir: &Path = t.path();
        let out = s.rdy(&["import", dir.to_str().unwrap(), "--view", "work"]);
        assert!(!out.status.success(), "{bad}: the import is refused");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("E_BAD_NAME") && err.contains(LOOKALIKE),
            "{err}"
        );
        assert_eq!(log_of(&s), before, "{bad}: no change was imported");
    }
}
