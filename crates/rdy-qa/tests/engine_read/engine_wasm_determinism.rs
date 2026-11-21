//! wck9 (D-QVTJ9, D-WV7ZS): the same committed input, pack and module bytes give the same rows
//! on every system. One fixed synthetic project, its ids written as they are, runs ten module
//! rules: a pass, a failure with the rule's words and one with the module's, a trap, memory
//! growth that stops at 256 pages, the call depth and the value stack past their caps, a block's
//! fuel and a rule's fuel, and a tail rule whose rows the 256-row cut leaves out. The change
//! stage and the propose stage, each read through the pack's verdict cache and as an uncached
//! scan, give exactly `tests/golden/wasm/rows-v1.json`; the canonical input of one block, with
//! lower-case control escapes, and each module's digest, fuel and answer on it give exactly
//! `fixtures/wasm/determinism-v1.json`. No text, count or fuel is normalized: a difference on a
//! system is a failure. Bless both with
//! `RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test engine_read wck9_`.
use rdy_engine::check::module_input::{ModuleState, encode_input};
use rdy_engine::check::module_run::{
    Judgement, RuleBudget, judge_module, module_runs_on_this_thread_for_tests,
};
use rdy_engine::check::rules::{CompiledRuleTest, CompiledRules};
use rdy_engine::check::{RuleIssue, Stage, evaluate_rules};
use rdy_engine::consequence::write_and_report;
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_model::{Block, BlockId};
use rdy_qa::fixtures::{project, raw, raw_commit, write, writer};
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items};
use rdy_qa::packs::use_pack;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The view that holds the project's content.
const VIEW: &str = "det";

/// The block whose canonical input the determinism fixture pins.
const PINNED: &str = "c-P0000";

/// Each rule: its name, the module fixture under `fixtures/wasm` and its `select`. The names
/// sort in this order, which is the order rules run in.
const RULES: [(&str, &str, &str); 10] = [
    ("a-pass", "pass", "type = claim"),
    ("b-fail", "fail", "type = claim"),
    ("c-text", "fail-text", "id ^= \"c-F\""),
    ("d-trap", "trap", "id ^= \"c-T\""),
    ("e-memory", "memory-grow", "id ^= \"c-M\""),
    ("f-depth", "call-depth", "id ^= \"c-S\""),
    ("g-stack", "value-stack", "id ^= \"c-S\""),
    ("h-spin", "spin", "id ^= \"c-W\""),
    ("i-burn", "burn-marker", "type = claim"),
    ("j-tail", "fail", "type = claim"),
];

/// The marked claims: `i-burn` burns a whole block's fuel on each, so the hundredth that runs
/// has less than a block's fuel left and every claim after it meets an empty rule budget.
const MARKED: usize = 102;

fn qa_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn module_text(fixture: &str) -> String {
    let path = qa_root().join(format!("fixtures/wasm/{fixture}.b64"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .trim()
        .to_string()
}

/// The pack's bytes: `rdy-standard` and the ten module rules, `a-pass` with args.
fn pack() -> String {
    let mut yaml = "name: wck9\nversion: \"1\"\nextends: rdy-standard\nrules:\n".to_string();
    for (name, fixture, select) in RULES {
        yaml.push_str(&format!(
            "  - name: {name}\n    select: '{select}'\n    module: {}\n    message: {name} message\n    fix: {name} fix\n",
            module_text(fixture)
        ));
        if name == "a-pass" {
            yaml.push_str(
                "    args:\n      limit: 3\n      words: [\"tab\\tend\", \"bell\\u0007\"]\n",
            );
        }
    }
    yaml
}

/// The claims, by id, with their final bodies: two plain `F` claims, the marked `K` claims, and
/// one claim each for memory, the pinned input (with control characters), the stack, the trap
/// and the spin.
fn claims() -> Vec<(String, String)> {
    let mut out = vec![];
    for k in 0..2 {
        out.push((
            format!("c-F{k:04}"),
            format!("Claim F{k:04} states one fact."),
        ));
    }
    for k in 0..MARKED {
        out.push((format!("c-K{k:04}"), format!("Marked claim K{k:04} ~!~.")));
    }
    for (id, body) in [
        ("c-M0000", "Memory grows to its cap."),
        (
            PINNED,
            "Tab\tthen escape \u{1b}[0m, unit \u{1f}, tab \u{b}, delete \u{7f},\nnew line, \"quote\" and \\ end.",
        ),
        ("c-S0000", "The stack runs out."),
        ("c-T0000", "The module traps."),
        ("c-W0000", "The module spins."),
    ] {
        out.push((id.to_string(), body.to_string()));
    }
    out
}

/// The fixed blocks, before the write that gives the claims their bodies.
fn seed_blocks() -> Vec<Block> {
    let first = rdy_model::order_run(None, None, 1).unwrap().remove(0);
    let mut blocks = vec![
        raw(
            "d-DET00",
            "doc",
            None,
            &first,
            &[
                ("name", "determinism"),
                ("title", "Determinism"),
                ("doc_type", "overview"),
            ],
            "",
        ),
        raw(
            "s-DET01",
            "section",
            Some("d-DET00"),
            &first,
            &[("title", "Cases")],
            "",
        ),
    ];
    let claims = claims();
    let orders = rdy_model::order_run(None, None, claims.len()).unwrap();
    for ((id, _), order) in claims.iter().zip(&orders) {
        blocks.push(raw(id, "claim", Some("s-DET01"), order, &[], "Draft."));
    }
    blocks
}

/// A row as the goldens hold it.
fn row(code: &str, block: &str, message: &str, fix: &str) -> Value {
    json!({"block": block, "code": code, "fix": fix, "message": message})
}

fn rows_of(rows: &[RuleIssue]) -> Vec<Value> {
    rows.iter()
        .map(|r| row(&r.code, &r.block, &r.message, &r.fix))
        .collect()
}

/// A propose stage's rows: the rule rows of a version's issues and warnings, and the counts the
/// cut left out.
fn propose_rows(p: &ProposalInfo) -> Value {
    let pick = |issues: &[rdy_schema::Issue], code: &str| -> Vec<Value> {
        issues
            .iter()
            .filter(|i| i.code == code)
            .map(|i| {
                row(
                    i.code,
                    &i.block
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    &i.message,
                    &i.fix,
                )
            })
            .collect()
    };
    let v = serde_json::to_value(p).unwrap();
    let omitted = |kind: &str| v["checks_omitted"][kind].as_u64().unwrap_or(0);
    json!({
        "blocking": pick(&p.issues, E_RULE_CHECK),
        "blocking_omitted": omitted("blocking"),
        "warnings": pick(&p.warnings, W_RULE_CHECK),
        "warnings_omitted": omitted("warnings"),
    })
}

/// The uncached scan of the view's state at `stage` over `ids`: its rules compiled for this
/// call, with an empty cache. The rules read no suspect, tension or coverage set.
fn uncached(r: &Rdy, ids: &BTreeSet<BlockId>, stage: Stage) -> rdy_engine::check::RuleScan {
    let st = r.read();
    let scope = Scope::View(VIEW.into());
    let m = st.materialized(&scope).unwrap();
    let g = st.graph(&scope).unwrap();
    let (empty, none) = (BTreeSet::new(), BTreeMap::new());
    let inputs = rdy_engine::filter::FilterInputs {
        pack: st.pack(),
        m: &m,
        graph: &g,
        areas: st.area_matcher(),
        suspects: &empty,
        tensions: &empty,
        coverage: &none,
    };
    evaluate_rules(st.pack(), inputs, ids.iter().cloned(), stage).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A judgement in words.
fn judged(j: &Judgement) -> String {
    match j {
        Judgement::Pass => "pass".into(),
        Judgement::Fail { message, fix } => format!(
            "fail: {} | {}",
            message.as_deref().unwrap_or("(rule message)"),
            fix.as_deref().unwrap_or("(rule fix)")
        ),
        Judgement::CannotJudge(cause) => format!("cannot judge: {}", cause.words()),
    }
}

/// The determinism record: the pinned block's canonical input for `a-pass`, and for each rule
/// its module's digest and length, and the fuel and answer of one fresh run on the pinned block's
/// input for that rule, with a new rule budget and an empty cache.
fn determinism(r: &Rdy, pack_bytes: &str) -> Value {
    let st = r.read();
    let scope = Scope::View(VIEW.into());
    let m = st.materialized(&scope).unwrap();
    let g = st.graph(&scope).unwrap();
    let ms = ModuleState::new(st.pack(), &m, &g, st.area_matcher());
    let compiled = st.compiled_rules().unwrap();
    let pinned = BlockId::parse(PINNED).unwrap();
    let mut modules = vec![];
    let mut input = Value::Null;
    for rule in compiled.rules() {
        let CompiledRuleTest::Module(mr) = &rule.test else {
            panic!("{} is a module rule", rule.name);
        };
        let bytes = encode_input(&ms, &rule.name, mr.args(), &pinned, mr.depth()).unwrap();
        if rule.name == "a-pass" {
            let text = String::from_utf8(bytes.clone()).unwrap();
            input = json!({
                "block": PINNED,
                "bytes": bytes.len(),
                "rule": rule.name,
                "sha256": hex(&Sha256::digest(&bytes)),
                "text": text,
            });
        }
        let fresh = CompiledRules::empty();
        let mut budget = RuleBudget::new();
        let given = bytes.clone();
        let j = judge_module(&rule.name, mr, fresh.cache(), &mut budget, move || {
            Some(given)
        })
        .unwrap();
        let fixture = RULES
            .iter()
            .find(|(n, _, _)| *n == rule.name)
            .map(|(_, f, _)| *f)
            .unwrap();
        modules.push(json!({
            "bytes": mr.bytes(),
            "fixture": format!("{fixture}.b64"),
            "fuel": budget.remaining().abs_diff(RuleBudget::new().remaining()),
            "input_sha256": hex(&Sha256::digest(&bytes)),
            "interface": mr.interface(),
            "judgement": judged(&j),
            "rule": rule.name,
            "sha256": hex(mr.sha256()),
        }));
    }
    json!({
        "input": input,
        "modules": modules,
        "pack_sha256": hex(&Sha256::digest(pack_bytes.as_bytes())),
    })
}

/// `v` as the committed file `path` (from the crate root) holds it, byte for byte; with
/// `RDY_QA_BLESS` set, `v` is written there instead.
fn exact(path: &str, v: &Value) {
    let text = format!("{}\n", serde_json::to_string_pretty(v).unwrap());
    let file = qa_root().join(path);
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &text).unwrap();
        return;
    }
    let want = std::fs::read(&file).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(
        String::from_utf8_lossy(&want),
        text,
        "{path}: this system gives other bytes"
    );
    assert_eq!(want, text.as_bytes(), "{path}");
}

#[test]
fn wck9_same_bytes_give_the_same_rows() {
    let pack_bytes = pack();
    let (d, r) = project();
    drop(r);
    use_pack(
        d.path(),
        "wck9",
        &pack_bytes,
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    let mut r = Rdy::open(d.path()).unwrap();
    raw_commit(&mut r, VIEW, seed_blocks());
    let claim_ids: BTreeSet<BlockId> = claims()
        .iter()
        .map(|(id, _)| BlockId::parse(id).unwrap())
        .collect();
    assert_eq!(claim_ids.len(), 2 + MARKED + 5);

    // the change stage: one write gives every claim its body, and judges each in the after
    // state; the pack's cache records what ran
    let e = Local::new(r);
    let ops: Vec<Value> = claims()
        .into_iter()
        .map(|(id, body)| json!({"op": "update", "id": id, "body": body}))
        .collect();
    let v = write_and_report(&e, VIEW, None, move |r| write(r, VIEW, json!(ops)))
        .unwrap_or_else(|e| panic!("the write: {e:#?}"));
    assert!(v["on_change"].get("checks_error").is_none(), "{v:#}");
    let checks: Vec<Value> = v["on_change"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("on_change.checks: {v:#}"))
        .iter()
        .map(|c| {
            let s = |k: &str| c[k].as_str().unwrap().to_string();
            row(&s("code"), &s("block"), &s("message"), &s("fix"))
        })
        .collect();
    let change = json!({
        "checks": checks,
        "checks_omitted": v["on_change"]["checks_omitted"].as_u64().unwrap_or(0),
    });

    // the propose stage, through the cache: every input the write judged replays, so no module
    // runs; the claims past the rule's fuel never ran and are judged without a run again
    let propose = rdy_qa::inbox::on(&e, |r| {
        let s = writer();
        answer_open_items(r, &s, VIEW);
        let runs = module_runs_on_this_thread_for_tests();
        let p = r
            .propose(&s, VIEW, VIEW, Some(TEST_SUMMARY))
            .unwrap_or_else(|e| panic!("propose: {e:#?}"));
        assert_eq!(
            module_runs_on_this_thread_for_tests() - runs,
            0,
            "the cache answers every input the change stage judged"
        );
        propose_rows(&p)
    });

    // the uncached scans of the same state give the same rows
    let (change_uncached, propose_uncached, ran, record) = e
        .inspect(|r| {
            let all: BTreeSet<BlockId> = r
                .read()
                .materialized(&Scope::View(VIEW.into()))
                .unwrap()
                .ids()
                .cloned()
                .collect();
            let runs = module_runs_on_this_thread_for_tests();
            let scan = uncached(r, &claim_ids, Stage::Change);
            let ran = module_runs_on_this_thread_for_tests() - runs;
            let change = json!({"checks": rows_of(&scan.rows()), "checks_omitted": scan.omitted()});
            let scan = uncached(r, &all, Stage::Propose);
            let propose = json!({
                "blocking": rows_of(&scan.blocking),
                "blocking_omitted": scan.blocking_omitted,
                "warnings": rows_of(&scan.warnings),
                "warnings_omitted": scan.warnings_omitted,
            });
            Ok((change, propose, ran, determinism(r, &pack_bytes)))
        })
        .unwrap();
    assert!(ran > 0, "the uncached scan runs its modules");
    assert_eq!(change_uncached, change, "the uncached change scan");
    assert_eq!(propose_uncached, propose, "the uncached propose scan");

    // the shape the goldens pin: every cause, the module's words and the rule's, both fuel
    // limits, the cut, and the same rows at both stages
    let words: BTreeSet<String> = change["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let m = c["message"].as_str().unwrap();
            m[m.find(": ").unwrap() + 2..].to_string()
        })
        .collect();
    for want in [
        "b-fail message",
        "the module says no",
        "the module trapped: wasm `unreachable` instruction executed",
        "the memory stopped at 256 pages",
        "the module trapped: call stack exhausted",
        "the module used all of its 10000000 fuel",
        "the rule used all of its 1000000000 fuel in this run",
    ] {
        assert!(words.contains(want), "{want}: {words:#?}");
    }
    assert!(change["checks_omitted"].as_u64().unwrap() > 0);
    assert_eq!(change["checks"], propose["warnings"]);
    assert_eq!(change["checks_omitted"], propose["warnings_omitted"]);

    exact(
        "tests/golden/wasm/rows-v1.json",
        &json!({"change": change, "propose": propose}),
    );
    let text = record["input"]["text"].as_str().unwrap();
    // lower-case six-byte escapes for every control character, the short forms never
    for escape in [
        r"\u0009", r"\u000a", r"\u000b", r"\u001b", r"\u001f", r"\u0007",
    ] {
        assert!(text.contains(escape), "{escape}: {text}");
    }
    for short in [r"\t", r"\n", r"\u001B", r"\u001F"] {
        assert!(!text.contains(short), "{short}: {text}");
    }
    exact("fixtures/wasm/determinism-v1.json", &record);
}
