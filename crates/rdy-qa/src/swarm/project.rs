//! The swarm's project: a served `rdy-qa` project with one stable doc per area, seeded in-process
//! before any daemon runs, landed and closed.
use super::script::{AREAS, Config};
use crate::daemon::Served;
use crate::edit_loop::{brief_of, write_cited};
use crate::fixtures::{land, write, writer};
use crate::packs::{QA_PACK_YAML, use_pack};
use crate::platform;
use rdy_engine::{DecisionInput, Rdy, Scope};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The rule of the swarm's authority policy (D-ZWP7Q): an agent `agent:a<i>` lands a version of
/// draft content whose every change implements a decision or is editorial, that records no
/// decision, with at most 20 blocks and 20 plan items.
pub const SWARM_RULE: &str = "swarm-drafts";

/// The name of a swarm process's parent directory under `/tmp`, before its pid.
const PARENT_PREFIX: &str = "rdysw-";

/// This process's directory for every swarm project and frozen copy: `/tmp/rdysw-<pid>`. The
/// sg5 checks share one run per seed in a static, which is never dropped, so those directories
/// outlive the test binary. The first call removes the directories of swarm processes that have
/// exited (`sweep`), so `/tmp` holds at most the last run of each process (final review I1).
pub fn run_parent() -> &'static Path {
    static PARENT: OnceLock<PathBuf> = OnceLock::new();
    PARENT.get_or_init(|| {
        let root = platform::short_tmp_root();
        let me = std::process::id();
        sweep(root, me);
        let p = root.join(format!("{PARENT_PREFIX}{me}"));
        std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("cannot make {}: {e}", p.display()));
        p
    })
}

/// Removes each `<root>/rdysw-<pid>` whose process has exited (`daemon::exited`, which also
/// sees another user's process), except `me`'s. Returns the paths it removed. A directory it
/// cannot remove is left.
pub fn sweep(root: &Path, me: u32) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return vec![];
    };
    let mut removed = vec![];
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(PARENT_PREFIX))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me || !crate::daemon::exited(pid) {
            continue;
        }
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    removed
}

/// How many claims each seed doc holds.
pub const CLAIMS_PER_AREA: usize = 4;

/// The project a swarm run acts on. Dropping it stops the daemon.
pub struct SwarmProject {
    pub served: Served,
    pub seed: SeedBlocks,
    /// The run's seed, for the text of a failure.
    pub run_seed: u64,
    /// The landings that seeded the project, oldest first: canon's chain before any actor runs.
    pub seed_landings: Vec<String>,
}

/// The ids of the seeded blocks. `docs`, `sections` and `decisions` hold one id per area, in the
/// order of `AREAS` (0 `payments`, 1 `identity`, 2 `glossary`). `claims` holds
/// `CLAIMS_PER_AREA` ids per area, area first: claim `j` of area `k` is `claims[k * 4 + j]`.
#[derive(Debug, Clone)]
pub struct SeedBlocks {
    pub docs: Vec<String>,
    pub sections: Vec<String>,
    pub claims: Vec<String>,
    pub decisions: Vec<String>,
}

impl SeedBlocks {
    /// The seed block a `Pick::Seed(n)` names: the first two claims of each area, area by area
    /// (`n` is taken modulo `SEED_BLOCKS`). Returns its id and its area index.
    pub fn pick(&self, n: usize) -> (&str, usize) {
        let n = n % super::script::SEED_BLOCKS;
        let area = n % AREAS.len();
        (&self.claims[area * CLAIMS_PER_AREA + n / AREAS.len()], area)
    }
}

/// Each area's doc: folder, doc name and the distinct term it defines.
const DOCS: [(&str, &str, &str); 3] = [
    ("wallets", "ledger", "ledger entry"),
    ("identity", "accounts", "account holder"),
    ("terms", "glossary", "glossary term"),
];

/// The path of each area's doc, in the order of `AREAS`.
pub const DOC_PATHS: [&str; 3] = ["wallets/ledger", "identity/accounts", "terms/glossary"];

/// The words of a seed claim, by index.
const CLAIM_WORDS: [&str; CLAIMS_PER_AREA] = ["first", "second", "third", "fourth"];

/// Builds the project: pack `rdy-qa`, then for each area a folder and a stable doc holding one
/// section with four claims, one accepted decision and one term, landed in one landing.
pub fn build(cfg: &Config) -> SwarmProject {
    let served = Served::new_in(run_parent());
    use_pack(
        served.dir.path(),
        "rdy-qa",
        QA_PACK_YAML,
        "serve:\n  idle_timeout: 60\n",
    );
    let mut r = Rdy::open(served.dir.path()).expect("open the swarm project");
    let mut ops: Vec<Value> = Vec::new();
    for (k, (folder, doc, term)) in DOCS.iter().enumerate() {
        ops.push(
            json!({"op": "create", "type": "folder", "id": format!("$f{k}"),
            "fields": {"name": folder, "title": folder}}),
        );
        ops.push(
            json!({"op": "create", "type": "doc", "id": format!("$d{k}"),
            "parent": format!("$f{k}"),
            "fields": {"name": doc, "title": doc, "doc_type": "overview"}}),
        );
        ops.push(
            json!({"op": "create", "type": "section", "id": format!("$s{k}"),
            "parent": format!("$d{k}"), "fields": {"title": "Rules"}}),
        );
        for (j, word) in CLAIM_WORDS.iter().enumerate() {
            ops.push(
                json!({"op": "create", "type": "claim", "id": format!("$c{k}{j}"),
                "parent": format!("$s{k}"), "body": format!("The {doc} rule {word} holds.")}),
            );
        }
        ops.push(
            json!({"op": "create", "type": "term", "id": format!("$t{k}"),
            "parent": format!("$s{k}"), "fields": {"name": term},
            "body": format!("A {term} is a record the {doc} keeps.")}),
        );
    }
    let w = write(&mut r, "work", Value::Array(ops)).expect("seed the swarm project");
    let id = |name: String| w.created[&name].clone();
    let mut seed = SeedBlocks {
        docs: Vec::new(),
        sections: Vec::new(),
        claims: Vec::new(),
        decisions: Vec::new(),
    };
    for (k, (_, doc, _)) in DOCS.iter().enumerate() {
        seed.docs.push(id(format!("$d{k}")));
        seed.sections.push(id(format!("$s{k}")));
        for j in 0..CLAIMS_PER_AREA {
            seed.claims.push(id(format!("$c{k}{j}")));
        }
        let d = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: id(format!("$s{k}")),
                    statement: format!("Keep the {doc} rules"),
                    ..Default::default()
                },
            )
            .expect("decide");
        seed.decisions.push(d.created["$decision"].clone());
    }
    land(&mut r, "work").expect("land the swarm seed");
    // then each doc becomes stable, citing a briefing of the three docs
    let docs: Vec<rdy_model::BlockId> = seed
        .docs
        .iter()
        .map(|d| rdy_model::BlockId::parse(d).expect("a doc id"))
        .collect();
    let brief = brief_of(
        &r,
        &Scope::View("work".into()),
        &docs.iter().collect::<Vec<_>>(),
    )
    .base
    .token;
    let ops: Vec<Value> = seed
        .docs
        .iter()
        .map(|d| json!({"op": "update", "id": d, "fields": {"lifecycle": "stable"}}))
        .collect();
    write_cited(&mut r, "work", Value::Array(ops), Some(&brief)).expect("make the docs stable");
    land(&mut r, "work").expect("land the stable docs");
    let mut seed_landings: Vec<String> = r
        .read()
        .reader()
        .landings()
        .expect("read the seed landings")
        .into_iter()
        .map(|(h, _)| h.to_string())
        .collect();
    seed_landings.reverse();
    r.close().expect("close the swarm project");
    SwarmProject {
        served,
        seed,
        run_seed: cfg.seed,
        seed_landings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final review I1: the sweep removes the directory of a swarm process that has exited, and
    /// keeps this process's, a live process's and any other name.
    #[test]
    fn sg5_the_sweep_removes_the_parents_of_exited_processes() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut child = platform::no_op_command().spawn().expect("spawn true");
        let dead = child.id();
        child.wait().expect("wait for true");
        let live = std::process::id();
        let names = [
            format!("{PARENT_PREFIX}{dead}"),
            format!("{PARENT_PREFIX}{live}"),
            format!("{PARENT_PREFIX}1"),
            format!("{PARENT_PREFIX}x"),
            "rdyd-other".to_string(),
        ];
        for n in &names {
            std::fs::create_dir_all(root.path().join(n).join("p")).expect("mkdir");
        }
        let removed = sweep(root.path(), live);
        assert_eq!(removed, vec![root.path().join(&names[0])]);
        for n in &names[1..] {
            assert!(root.path().join(n).exists(), "{n} was kept");
        }
        assert!(run_parent().ends_with(format!("{PARENT_PREFIX}{live}")));
        assert!(run_parent().is_dir());
    }
}
