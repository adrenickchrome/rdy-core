//! The QA test pack: three areas and one tension override, extending `rdy-standard`.
use rdy_engine::Rdy;

pub const QA_PACK_YAML: &str = "\
name: rdy-qa
version: \"1\"
extends: rdy-standard
areas:
  payments:
    description: money in and out
    paths: [wallets, payments/**]
  identity:
    paths: [identity/*]
  glossary:
    paths: ['**/glossary']
tensions:
  unjustified_change: blocking
";

/// A project whose pack is `rdy-qa`: initialised with `rdy-standard`, then switched.
pub fn qa_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().expect("tempdir");
    let r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    drop(r);
    std::fs::write(dir.path().join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).expect("pack");
    std::fs::write(dir.path().join(".rdy/config.yaml"), "pack: rdy-qa\n").expect("config");
    let r = Rdy::open(dir.path()).expect("open");
    (dir, r)
}

/// `figure` without `asset` (traits replace on override; fields merge), and an asset type `shot`
/// under `section` (children replace, so section's list is restated).
pub const BLOB_PACK: &str = "\
name: blobs
version: \"1\"
extends: rdy-standard
block_types:
  figure:
    prefix: fg
    traits: [informative]
    body: forbidden
  shot:
    prefix: sh
    traits: [asset, informative]
    body: forbidden
    fields:
      blob: { kind: str, required: true }
      media_type: { kind: str, required: true }
  section:
    prefix: s
    traits: [structural]
    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, value, entity, flow, example, note, rationale, table, diagram, figure, code, decision, question, shot]
";

/// Makes the project at `dir` use pack `name` (written from `yaml`); `extra_config` follows the
/// `pack:` line.
pub fn use_pack(dir: &std::path::Path, name: &str, yaml: &str, extra_config: &str) {
    std::fs::write(dir.join(format!(".rdy/packs/{name}.yaml")), yaml).expect("pack");
    std::fs::write(
        dir.join(".rdy/config.yaml"),
        format!("pack: {name}\n{extra_config}"),
    )
    .expect("config");
}
