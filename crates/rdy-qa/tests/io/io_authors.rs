//! mfb3: per-block authorship through export and import (intake 2026-10-09). Export writes
//! authors.yaml; import writes one change per author with the author's time, names the importer
//! in each message, and keeps the import view the importer's.
use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::path::Path;

fn sess(actor: &str, run: Option<&str>, role: Role) -> Session {
    Session {
        actor: actor.into(),
        run: run.map(str::to_string),
        role,
        kind: SessionKind::Human,
    }
}

fn founder() -> Session {
    sess("human:founder", None, Role::Approver)
}

fn ops(v: serde_json::Value) -> Vec<OpInput> {
    serde_json::from_value(v).unwrap()
}

fn put(r: &mut Rdy, s: &Session, view: &str, v: serde_json::Value) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: ops(v),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

/// Canon: doc `d` by agent:a (run r1), then prose `p` under it by agent:b (run r2).
fn two_authors(dir: &Path) -> (Rdy, String, String) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let a = sess("agent:a", Some("r1"), Role::Writer);
    let w = put(
        &mut r,
        &a,
        "va",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}}
        ]),
    );
    let d = w.created["$d"].clone();
    let p = r.propose(&a, "va", "a").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let b = sess("agent:b", Some("r2"), Role::Writer);
    let w = put(
        &mut r,
        &b,
        "vb",
        json!([
            {"op": "create", "type": "prose", "id": "$p", "parent": d, "body": "Hello."}
        ]),
    );
    let prose = w.created["$p"].clone();
    let p = r.propose(&b, "vb", "b").unwrap();
    r.land(&founder(), &p.id).unwrap();
    (r, d, prose)
}

fn authors(out: &Path) -> Vec<serde_yaml_ng::Value> {
    serde_yaml_ng::from_str(&std::fs::read_to_string(out.join("authors.yaml")).unwrap()).unwrap()
}

fn row<'a>(rows: &'a [serde_yaml_ng::Value], id: &str) -> &'a serde_yaml_ng::Value {
    rows.iter()
        .find(|r| r["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("{id} not in authors.yaml"))
}

#[test]
fn mfb3_export_writes_each_blocks_author_and_times() {
    let a_dir = tempfile::tempdir().unwrap();
    let (r, d, p) = two_authors(a_dir.path());
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("x");
    r.read().export(&Scope::Canon, &out).unwrap();
    let rows = authors(&out);
    assert_eq!(row(&rows, &d)["actor"].as_str(), Some("agent:a"));
    assert_eq!(row(&rows, &d)["run"].as_str(), Some("r1"));
    assert_eq!(row(&rows, &d)["role"].as_str(), Some("writer"));
    assert_eq!(row(&rows, &p)["actor"].as_str(), Some("agent:b"));
    assert!(row(&rows, &p)["at_ms"].as_i64().is_some());
    assert!(row(&rows, &p)["landed_ms"].as_i64().is_some());
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "sorted by id");
}

#[test]
fn mfb3_import_writes_one_change_per_author_with_their_times() {
    let a_dir = tempfile::tempdir().unwrap();
    let (r, d, p) = two_authors(a_dir.path());
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("x");
    r.read().export(&Scope::Canon, &out).unwrap();
    let rows = authors(&out);
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let imp = sess("human:imp", None, Role::Approver);
    let res = b.import(&imp, "import", &out).unwrap();
    assert_eq!(res.changes.len(), 2, "{res:?}");
    assert_eq!(res.change, res.changes[1]);
    let st = b.read();
    let ch = |h: &str| st.reader().change(&h.parse::<Hash>().unwrap()).unwrap();
    let first = ch(&res.changes[0]);
    let second = ch(&res.changes[1]);
    assert_eq!(first.provenance.actor, "agent:a");
    assert_eq!(first.provenance.run.as_deref(), Some("r1"));
    assert_eq!(
        first.provenance.at_ms,
        row(&rows, &d)["at_ms"].as_i64().unwrap()
    );
    assert_eq!(second.provenance.actor, "agent:b");
    assert_eq!(
        second.provenance.at_ms,
        row(&rows, &p)["at_ms"].as_i64().unwrap()
    );
    assert_eq!(
        first.message,
        format!(
            "import from {}: 1 block(s) by agent:a, imported by human:imp",
            out.display()
        )
    );
    let json = serde_json::to_value(&res).unwrap();
    assert_eq!(json["changes"].as_array().map(Vec::len), Some(2));
}

#[test]
fn mfb3_the_import_view_stays_the_importers() {
    let a_dir = tempfile::tempdir().unwrap();
    let (r, _d, _p) = two_authors(a_dir.path());
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("x");
    r.read().export(&Scope::Canon, &out).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let imp = sess("human:imp", None, Role::Approver);
    b.import(&imp, "import", &out).unwrap();
    // `read()` returns an Arc: hold it while the view head is borrowed
    let st = b.read();
    let owner = st
        .view_head("import")
        .expect("the view exists")
        .owner
        .clone();
    drop(st);
    assert_eq!(owner.as_ref().map(|o| o.actor.as_str()), Some("human:imp"));
    // the importer proposes and lands it as before
    let p = b.propose(&imp, "import", "migrate").unwrap();
    b.land(&imp, &p.id).unwrap();
}

#[test]
fn mfb3_without_authors_yaml_the_importer_writes_one_change() {
    let a_dir = tempfile::tempdir().unwrap();
    let (r, _d, _p) = two_authors(a_dir.path());
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("x");
    r.read().export(&Scope::Canon, &out).unwrap();
    std::fs::remove_file(out.join("authors.yaml")).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let imp = sess("human:imp", None, Role::Approver);
    let res = b.import(&imp, "import", &out).unwrap();
    assert_eq!(res.changes.len(), 1);
    let c = b
        .read()
        .reader()
        .change(&res.change.parse::<Hash>().unwrap())
        .unwrap();
    assert_eq!(c.provenance.actor, "human:imp");
    assert_eq!(c.message, format!("import from {}", out.display()));
}

#[test]
fn mfb3_a_bad_authors_file_is_refused_and_writes_nothing() {
    let a_dir = tempfile::tempdir().unwrap();
    let (r, d, _p) = two_authors(a_dir.path());
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("x");
    r.read().export(&Scope::Canon, &out).unwrap();
    let good = std::fs::read_to_string(out.join("authors.yaml")).unwrap();
    let cases = [
        (good.replace("actor: agent:a", "actor: ' '"), "blank actor"),
        (good.replace("run: r1", "run: ' '"), "blank run"),
        (good.replace("role: writer", "role: boss"), "unknown role"),
        (
            format!("{good}- id: c-ZZZZZ\n  actor: agent:x\n  role: writer\n  at_ms: 1\n"),
            "unknown id",
        ),
        (good.replacen("actor:", "actr:", 1), "unknown key"),
        (
            format!("{good}- id: {d}\n  actor: agent:x\n  role: writer\n  at_ms: 1\n"),
            "duplicate id",
        ),
    ];
    for (text, why) in cases {
        std::fs::write(out.join("authors.yaml"), text).unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
        let e = b
            .import(&founder(), "import", &out)
            .err()
            .unwrap_or_else(|| panic!("{why}: accepted"));
        assert_eq!(e.code, E_BAD_INPUT, "{why}: {e:?}");
        assert!(
            e.message.starts_with("authors.yaml"),
            "{why}: {}",
            e.message
        );
        assert!(
            b.read().view_head("import").is_none(),
            "{why}: a view was created"
        );
    }
}

/// Orchestrator pre-flight: author times interleave. agent:a wrote doc `d` (100) and prose `q`
/// (300); agent:b wrote prose `p` under `d` (200). agent:b's change comes first and holds `p`
/// before its parent `d` arrives in agent:a's later change. The final tree equals the export,
/// and every block keeps its author; a block's time is its author's newest time (one change per
/// author).
#[test]
fn mfb3_interleaved_authors_import_to_the_exported_tree() {
    let a_dir = tempfile::tempdir().unwrap();
    let (mut r, d, p) = two_authors(a_dir.path());
    let a = sess("agent:a", Some("r1"), Role::Writer);
    let w = put(
        &mut r,
        &a,
        "vc",
        json!([
            {"op": "create", "type": "prose", "id": "$q", "parent": d, "body": "Again."}
        ]),
    );
    let q = w.created["$q"].clone();
    let pr = r.propose(&a, "vc", "c").unwrap();
    r.land(&founder(), &pr.id).unwrap();
    let out = tempfile::tempdir().unwrap();
    let first = out.path().join("one");
    r.read().export(&Scope::Canon, &first).unwrap();
    // pin the times so the order does not hang on the clock
    let text = format!(
        "- id: {d}\n  actor: agent:a\n  run: r1\n  role: writer\n  at_ms: 100\n\
         - id: {p}\n  actor: agent:b\n  run: r2\n  role: writer\n  at_ms: 200\n\
         - id: {q}\n  actor: agent:a\n  run: r1\n  role: writer\n  at_ms: 300\n"
    );
    std::fs::write(first.join("authors.yaml"), text).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let imp = sess("human:imp", None, Role::Approver);
    let res = b.import(&imp, "import", &first).unwrap();
    assert_eq!(res.changes.len(), 2, "{res:?}");
    {
        let st = b.read();
        let ch = |h: &str| st.reader().change(&h.parse::<Hash>().unwrap()).unwrap();
        let one = ch(&res.changes[0]);
        let two = ch(&res.changes[1]);
        assert_eq!(one.provenance.actor, "agent:b");
        assert_eq!(one.provenance.at_ms, 200);
        let one_ids: Vec<String> = one.ops.iter().map(|o| o.block_id().to_string()).collect();
        assert_eq!(
            one_ids,
            vec![p.clone()],
            "the child comes before its parent"
        );
        assert_eq!(two.provenance.actor, "agent:a");
        assert_eq!(two.provenance.at_ms, 300);
    }
    let pr = b.propose(&imp, "import", "migrate").unwrap();
    b.land(&imp, &pr.id).unwrap();
    assert_eq!(
        b.read().materialized(&Scope::Canon).unwrap().snapshot(),
        r.read().materialized(&Scope::Canon).unwrap().snapshot(),
        "the tree equals the export"
    );
    let second = out.path().join("two");
    b.read().export(&Scope::Canon, &second).unwrap();
    for name in ["docs", "rdy-export.yaml"] {
        let (x, y) = (first.join(name), second.join(name));
        if x.is_dir() {
            assert!(!dir_diff(&x, &y), "{name} differs");
        } else {
            assert_eq!(
                std::fs::read(&x).unwrap(),
                std::fs::read(&y).unwrap(),
                "{name}"
            );
        }
    }
    let rows = authors(&second);
    for (id, actor, at) in [
        (&d, "agent:a", 300),
        (&p, "agent:b", 200),
        (&q, "agent:a", 300),
    ] {
        assert_eq!(row(&rows, id)["actor"].as_str(), Some(actor), "{id}");
        assert_eq!(row(&rows, id)["at_ms"].as_i64(), Some(at), "{id}");
    }
}

/// True when the two trees differ in any file name or content.
fn dir_diff(x: &Path, y: &Path) -> bool {
    fn walk(root: &Path, d: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out)
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_string_lossy().into(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let (mut a, mut b) = (Default::default(), Default::default());
    walk(x, x, &mut a);
    walk(y, y, &mut b);
    a != b
}

/// Guard: an export with no blocks still imports as the importer's one change, as before mfb3.
#[test]
fn mfb3_an_empty_export_imports_as_the_importers_one_change() {
    let a_dir = tempfile::tempdir().unwrap();
    let a = Rdy::init(a_dir.path(), "rdy-standard").unwrap();
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("x");
    a.read().export(&Scope::Canon, &out).unwrap();
    assert!(authors(&out).is_empty());
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let imp = sess("human:imp", None, Role::Approver);
    let res = b.import(&imp, "import", &out).unwrap();
    assert_eq!(res.changes.len(), 1, "{res:?}");
    let c = b
        .read()
        .reader()
        .change(&res.change.parse::<Hash>().unwrap())
        .unwrap();
    assert_eq!(c.provenance.actor, "human:imp");
}
