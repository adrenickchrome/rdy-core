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
