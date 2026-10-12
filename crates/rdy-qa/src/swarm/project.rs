//! The swarm's project: a served `rdy-qa` project with one stable doc per area, seeded in-process
//! before any daemon runs, landed and closed.
use super::script::{AREAS, Config};
use crate::daemon::Served;
use crate::edit_loop::{brief_of, write_cited};
use crate::fixtures::{land, write, writer};
use crate::packs::{QA_PACK_YAML, use_pack};
use rdy_engine::{DecisionInput, Rdy, Scope};
use serde_json::{Value, json};

/// How many claims each seed doc holds.
pub const CLAIMS_PER_AREA: usize = 4;

/// The project a swarm run acts on. Dropping it stops the daemon.
pub struct SwarmProject {
    pub served: Served,
    pub seed: SeedBlocks,
    /// The run's seed, for the text of a failure.
    pub run_seed: u64,
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
    let served = Served::new();
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
    r.close().expect("close the swarm project");
    SwarmProject {
        served,
        seed,
        run_seed: cfg.seed,
    }
}
