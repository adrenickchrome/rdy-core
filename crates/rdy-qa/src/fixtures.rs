use rdy_engine::{Rdy, RdyError, Session, WriteRequest, WriteResult};
use rdy_model::{Block, BlockId, Change, ChangeLinks, FieldValue, Op, Role};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer)
}
pub fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver)
}
pub fn reader() -> Session {
    Session::human("human:qa".into(), Role::Reader)
}
/// An agent session (writer) for ownership tests.
pub fn agent(actor: &str, run: Option<&str>) -> Session {
    Session::agent(actor.into(), run.map(str::to_string), Role::Writer).unwrap()
}

/// A user config naming `actor` as the human: `<tmp>/rdy/config.yaml`, and the environment
/// (`XDG_CONFIG_HOME`) that points a client at it. Keep the `TempDir` alive.
pub fn human_env(actor: &str) -> (tempfile::TempDir, Vec<(String, String)>) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("rdy")).expect("config dir");
    std::fs::write(
        dir.path().join("rdy/config.yaml"),
        format!("actor: {actor}\n"),
    )
    .expect("config");
    let env = vec![(
        "XDG_CONFIG_HOME".to_string(),
        dir.path().to_string_lossy().into_owned(),
    )];
    (dir, env)
}

/// An agent session (writer) with run `run`.
pub fn as_agent(actor: &str, run: &str) -> Session {
    Session::agent(actor.into(), Some(run.into()), Role::Writer).unwrap()
}

/// The human, as a writer (`writer()`).
pub fn human_writer() -> Session {
    writer()
}

pub fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().expect("tempdir");
    let r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    (dir, r)
}

pub fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> Result<WriteResult, RdyError> {
    let ops = serde_json::from_value(ops)
        .map_err(|e| RdyError::new(rdy_engine::E_BAD_INPUT, e.to_string(), "fix the test's ops"))?;
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

pub fn write_as(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    ops: serde_json::Value,
) -> Result<WriteResult, RdyError> {
    let ops = serde_json::from_value(ops)
        .map_err(|e| RdyError::new(rdy_engine::E_BAD_INPUT, e.to_string(), "fix the test's ops"))?;
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

pub fn version_of(r: &Rdy, scope: &rdy_engine::Scope, id: &str) -> String {
    let id = BlockId::parse(id).expect("block id");
    r.read()
        .materialized(scope)
        .expect("scope")
        .version_of(&id)
        .expect("block in scope")
        .to_string()
}

pub fn body_of(r: &Rdy, scope: &rdy_engine::Scope, id: &str) -> String {
    let id = BlockId::parse(id).expect("block id");
    r.read()
        .materialized(scope)
        .expect("scope")
        .get(&id)
        .expect("block in scope")
        .body
        .clone()
}

/// One glossary doc "Seed" with one section and one claim, written into `view` by `writer()`.
pub fn seed_doc(r: &mut Rdy, view: &str) -> (String, String, String) {
    let res = write(r, view, serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "seed", "title": "Seed", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "one"}
    ])).expect("seed");
    (
        res.created["$d"].clone(),
        res.created["$s"].clone(),
        res.created["$c"].clone(),
    )
}

/// Proposes `view` as a writer and lands it as an approver; returns the landing id.
pub fn land(r: &mut Rdy, view: &str) -> Result<String, RdyError> {
    let p = r.propose(&writer(), view, view)?;
    r.land(&approver(), &p.id)
}

pub fn raw(
    id: &str,
    ty: &str,
    parent: Option<&str>,
    order: &str,
    fields: &[(&str, &str)],
    body: &str,
) -> Block {
    Block {
        id: BlockId::parse(id).expect("valid id"),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).expect("valid parent id")),
        order: order.into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
            .collect::<BTreeMap<_, _>>(),
        body: body.into(),
        links: Default::default(),
        pins: Default::default(),
    }
}

/// Writes blocks straight to the store, bypassing every engine rule, then drops caches.
pub fn raw_commit(r: &mut Rdy, view: &str, blocks: Vec<Block>) {
    let ops = blocks
        .into_iter()
        .map(|block| Op::Create { block })
        .collect();
    r.store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops,
                deps: vec![],
                provenance: writer().provenance(),
                justification: None,
                message: "raw".into(),
                links: ChangeLinks::default(),
            },
        )
        .expect("raw commit");
    r.invalidate();
}

pub fn deep() -> bool {
    std::env::var("RDY_QA_DEEP").is_ok_and(|v| v == "1")
}

/// Proptest case count: `fast`, or ten times it in deep mode.
pub fn cases(fast: u32) -> u32 {
    if deep() { fast * 10 } else { fast }
}

/// The `rdy` binary: `$RDY_BIN`, else `target/release/rdy` next to this test binary.
pub fn rdy_bin() -> PathBuf {
    if let Ok(p) = std::env::var("RDY_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("current exe");
    // target/release/deps/<test> → target/release/rdy
    let bin = exe
        .parent()
        .and_then(|d| d.parent())
        .map(|d| d.join("rdy"))
        .expect("target dir");
    assert!(
        bin.exists(),
        "{} not found: run `cargo build --release -p rdy-cli` first (cargo qa does this) or set RDY_BIN",
        bin.display()
    );
    bin
}
