//! wck3: a pack's rules compile once per load into one handle. The handle keeps each module's
//! metadata and owns that pack's verdict cache, which replays a recorded result and its fuel only
//! where an uncached run would give the same, holds at most 64 MiB of entries, and never outlives
//! its pack.
use rdy_check_wasm::{CheckResult, RunFailure, RunOutcome};
use rdy_engine::check::rules::{
    CompiledRuleTest, CompiledRules, VERDICT_CACHE_BYTES, VerdictCache, VerdictKey,
    compile_rule_specs, module_compilations_on_this_thread_for_tests,
    rule_compilations_on_this_thread_for_tests,
};
use rdy_engine::{E_CONFIG, Rdy, ReloadOutcome, Session, WriteRequest};
use rdy_schema::{ArgValue, ModuleTest, Pack, PackSet, RuleInput, RuleSeverity, RuleTestSpec};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A small encoder of check modules. The default sections give a valid one: `rdy_alloc` returns
/// 0 and `rdy_check_v1` returns `answer`. Each of `sections` adds a section, or replaces the
/// default of its id; sections go out in the binary's order.
fn module_with(answer: u8, sections: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut exports = vec![3];
    for (name, kind, index) in [
        ("memory", 2u8, 0u8),
        ("rdy_alloc", 0, 0),
        ("rdy_check_v1", 0, 1),
    ] {
        exports.push(name.len() as u8);
        exports.extend(name.as_bytes());
        exports.extend([kind, index]);
    }
    let mut by_id: BTreeMap<u8, Vec<u8>> = BTreeMap::from([
        // (i32) -> i32 and (i32, i32) -> i64
        (
            1,
            vec![2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e],
        ),
        (3, vec![2, 0, 1]),
        (5, vec![1, 0x00, 1]),
        (7, exports),
        (
            10,
            vec![2, 4, 0, 0x41, 0, 0x0b, 4, 0, 0x42, answer & 0x3f, 0x0b],
        ),
    ]);
    for (id, body) in sections {
        by_id.insert(*id, body.clone());
    }
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    for id in [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 12, 10, 11] {
        if let Some(body) = by_id.get(&id) {
            assert!(body.len() < 128);
            out.extend([id, body.len() as u8]);
            out.extend(body);
        }
    }
    out
}

fn module(answer: u8) -> Vec<u8> {
    module_with(answer, &[])
}

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn module_rule(
    name: &str,
    bytes: Vec<u8>,
    args: Option<BTreeMap<String, ArgValue>>,
    depth: Option<u8>,
) -> RuleInput {
    RuleInput {
        name: name.into(),
        select: "type = section".into(),
        test: RuleTestSpec::Module(ModuleTest {
            module: bytes,
            args,
            depth,
        }),
        severity: RuleSeverity::Advisory,
        message: format!("{name} message"),
        fix: format!("{name} fix"),
    }
}

fn filter_rule(name: &str) -> RuleInput {
    RuleInput {
        name: name.into(),
        select: "type = section".into(),
        test: RuleTestSpec::Filter {
            require: "type = section".into(),
        },
        severity: RuleSeverity::Blocking,
        message: format!("{name} message"),
        fix: format!("{name} fix"),
    }
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The refusal of compiling `rules` under the built-in pack.
fn refusal(rules: &[RuleInput]) -> (String, String, String) {
    let e = match compile_rule_specs(&standard(), rules) {
        Ok(_) => panic!("the rules compiled"),
        Err(e) => e,
    };
    (e.code.to_string(), e.message, e.fix)
}

/// A pack of the project that extends the built-in one with `rules` (filter rules as YAML).
fn pack_yaml(rules: &str) -> String {
    format!("name: proj\nversion: '1'\nextends: rdy-standard\nrules:\n{rules}")
}

const ONE_RULE: &str = "  - name: r1\n    select: type = section\n    require: type = section\n    message: m\n    fix: f\n";
const TWO_RULES: &str = "  - name: r1\n    select: type = section\n    require: type = section\n    message: m\n    fix: f\n  - name: r2\n    select: type = section\n    require: type = section\n    message: m\n    fix: f\n";
const BAD_RULE: &str = "  - name: r1\n    select: type = = section\n    require: type = section\n    message: m\n    fix: f\n";

/// A project whose pack extends the built-in one with `rules`, opened.
fn project(rules: &str) -> (tempfile::TempDir, Rdy) {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    set_pack(d.path(), rules);
    let r = Rdy::open(d.path()).unwrap();
    (d, r)
}

fn set_pack(dir: &std::path::Path, rules: &str) {
    std::fs::write(dir.join(".rdy/packs/proj.yaml"), pack_yaml(rules)).unwrap();
    std::fs::write(dir.join(".rdy.yaml"), "pack: proj\n").unwrap();
}

fn write_doc(r: &mut Rdy, name: &str) {
    let s = Session::human("human:qa".into(), rdy_model::Role::Writer).unwrap();
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "page"}}
    ]))
    .unwrap();
    r.write(
        &s,
        WriteRequest {
            view: "work".into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
}

fn key(rule: &str, module: u8, input: u8) -> VerdictKey {
    VerdictKey {
        rule: rule.into(),
        module: [module; 32],
        input: [input; 32],
    }
}

fn outcome(result: Result<CheckResult, RunFailure>, fuel: u64) -> RunOutcome {
    RunOutcome { result, fuel }
}

fn pass(fuel: u64) -> RunOutcome {
    outcome(Ok(CheckResult::Pass), fuel)
}

fn cache() -> Arc<CompiledRules> {
    Arc::new(compile_rule_specs(&standard(), &[]).unwrap())
}

#[test]
fn wck3_prepared_rules_compile_once_and_keep_metadata() {
    let (a, c) = (module(1), module(2));
    let args = BTreeMap::from([("k".to_string(), ArgValue::Int(1))]);
    let specs = [
        module_rule("b-mod", a.clone(), Some(args.clone()), Some(2)),
        filter_rule("a-filter"),
        module_rule("c-mod", c.clone(), None, None),
    ];
    let (sets, modules) = (
        rule_compilations_on_this_thread_for_tests(),
        module_compilations_on_this_thread_for_tests(),
    );
    let rules = compile_rule_specs(&standard(), &specs).unwrap();
    // one compilation of the set, each module compiled once
    assert_eq!(rule_compilations_on_this_thread_for_tests() - sets, 1);
    assert_eq!(module_compilations_on_this_thread_for_tests() - modules, 2);
    let names: Vec<&str> = rules.rules().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["a-filter", "b-mod", "c-mod"], "name order");
    assert!(matches!(rules.rules()[0].test, CompiledRuleTest::Filter(_)));
    assert_eq!(rules.rules()[0].severity, RuleSeverity::Blocking);
    let CompiledRuleTest::Module(b) = &rules.rules()[1].test else {
        panic!("b-mod is a module rule")
    };
    assert_eq!(b.sha256(), &sha(&a));
    assert_eq!(b.bytes(), a.len());
    assert_eq!(b.interface(), 1);
    assert_eq!(b.module().module_bytes(), a.len());
    assert_eq!(b.args(), &args);
    assert_eq!(b.depth(), 2);
    let CompiledRuleTest::Module(cm) = &rules.rules()[2].test else {
        panic!("c-mod is a module rule")
    };
    assert_eq!(cm.sha256(), &sha(&c));
    assert_ne!(cm.sha256(), b.sha256());
    // absent args is the empty mapping, absent depth the default
    assert!(cm.args().is_empty());
    assert_eq!(cm.depth(), 1);
    assert_eq!(rules.rules()[2].message, "c-mod message");
    assert!(rules.cache().is_empty());

    // each compile refusal keeps the pack envelope and its exact detail
    let config = |detail: &str| {
        (
            E_CONFIG.to_string(),
            format!("invalid pack: {detail}"),
            "fix the pack file".to_string(),
        )
    };
    let import = [
        vec![1, 3],
        b"env".to_vec(),
        vec![1],
        b"f".to_vec(),
        vec![0x00, 0x00],
    ]
    .concat();
    assert_eq!(
        refusal(&[module_rule("m", module_with(1, &[(2, import)]), None, None)]),
        config("rule m module imports env.f; a check module imports nothing")
    );
    let mut no_alloc = module(1);
    // rename the export rdy_alloc to rdy_allox
    let at = no_alloc.windows(9).position(|w| w == b"rdy_alloc").unwrap();
    no_alloc[at + 8] = b'x';
    assert_eq!(
        refusal(&[module_rule("m", no_alloc, None, None)]),
        config("rule m module lacks the export rdy_alloc of type (i32) -> i32")
    );
    // a start function of a third type, () -> ()
    let started = module_with(
        1,
        &[
            (
                1,
                vec![
                    3, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e, 0x60, 0, 0,
                ],
            ),
            (3, vec![3, 0, 1, 2]),
            (8, vec![2]),
            (
                10,
                vec![3, 4, 0, 0x41, 0, 0x0b, 4, 0, 0x42, 1, 0x0b, 2, 0, 0x0b],
            ),
        ],
    );
    assert_eq!(
        refusal(&[module_rule("m", started, None, None)]),
        config("rule m module has a start function; a check module has none")
    );
    // 257 pages
    assert_eq!(
        refusal(&[module_rule(
            "m",
            module_with(1, &[(5, vec![1, 0x00, 0x81, 0x02])]),
            None,
            None
        )]),
        config("rule m module memory starts above 256 pages")
    );
    let two = vec![2, 0x70, 0x00, 0x01, 0x70, 0x00, 0x01];
    assert_eq!(
        refusal(&[module_rule("m", module_with(1, &[(4, two)]), None, None)]),
        config("rule m module has more than one table")
    );
    // 10001 elements
    let wide = vec![1, 0x70, 0x00, 0x91, 0x4e];
    assert_eq!(
        refusal(&[module_rule("m", module_with(1, &[(4, wide)]), None, None)]),
        config("rule m module table exceeds 10000 elements")
    );
    let (code, message, fix) = refusal(&[module_rule("m", b"not wasm".to_vec(), None, None)]);
    assert_eq!(
        (code.as_str(), fix.as_str()),
        (E_CONFIG, "fix the pack file")
    );
    assert!(
        message.starts_with("invalid pack: rule m module: "),
        "{message}"
    );
    // the structural checks come first, with their own words
    let mut dup = filter_rule("a");
    dup.name = "b-mod".into();
    assert_eq!(
        refusal(&[module_rule("b-mod", module(1), None, None), dup]),
        config("rule b-mod is declared more than once")
    );

    // a project compiles its pack's rules once at open; publishes share that handle, and reads
    // and writes compile nothing
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    set_pack(d.path(), ONE_RULE);
    let before = rule_compilations_on_this_thread_for_tests();
    let mut r = Rdy::open(d.path()).unwrap();
    assert_eq!(
        rule_compilations_on_this_thread_for_tests() - before,
        1,
        "the open"
    );
    let handle = r.compiled_rules();
    assert_eq!(handle.rules().len(), 1);
    assert!(Arc::ptr_eq(&r.read().compiled_rules().unwrap(), &handle));
    let warm = rule_compilations_on_this_thread_for_tests();
    write_doc(&mut r, "one");
    write_doc(&mut r, "two");
    let s = Session::human("human:qa".into(), rdy_model::Role::Writer).unwrap();
    r.propose(&s, "work", "p", Some("test summary")).unwrap();
    assert!(Arc::ptr_eq(&r.read().compiled_rules().unwrap(), &handle));
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Unchanged));
    assert_eq!(
        rule_compilations_on_this_thread_for_tests(),
        warm,
        "writes, reads and an unchanged reload compile nothing"
    );
    // a changed pack compiles once into a new handle
    set_pack(d.path(), TWO_RULES);
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    assert_eq!(rule_compilations_on_this_thread_for_tests() - warm, 1);
    let next = r.read().compiled_rules().unwrap();
    assert!(!Arc::ptr_eq(&next, &handle));
    assert_eq!(next.rules().len(), 2);
}

#[test]
fn wck3_verdict_cache_replays_fuel_and_drops_on_reload() {
    let rules = cache();
    let c: &VerdictCache = rules.cache();

    // completed results that used F fuel replay at an allowance of F or more, charging F
    let fail = outcome(
        Ok(CheckResult::Fail {
            message: Some("bad".into()),
            fix: None,
        }),
        50,
    );
    let trap = outcome(Err(RunFailure::Trap("unreachable".into())), 30);
    let bad = outcome(Err(RunFailure::BadOutput("trailing JSON".into())), 20);
    let big = outcome(Err(RunFailure::OutputTooLarge), 40);
    for (k, o) in [
        (key("r", 1, 1), pass(100)),
        (key("r", 1, 2), fail.clone()),
        (key("r", 1, 3), trap.clone()),
        (key("r", 1, 4), bad.clone()),
        (key("r", 1, 5), big.clone()),
    ] {
        assert!(c.record(&k, 1_000, &o));
        let f = o.fuel;
        assert_eq!(c.replay(&k, f - 1), None, "below F");
        assert_eq!(c.replay(&k, f), Some(o.clone()), "at F");
        assert_eq!(c.replay(&k, f + 1), Some(o.clone()), "above F");
        assert_eq!(c.replay(&k, 10_000_000), Some(o.clone()));
    }

    // a run that used all of allowance A replays at an allowance of A or less, charging all of
    // the current allowance; above A it runs again
    let k = key("r", 1, 6);
    assert!(c.record(&k, 500, &outcome(Err(RunFailure::OutOfFuel), 500)));
    let out = |fuel| Some(outcome(Err(RunFailure::OutOfFuel), fuel));
    assert_eq!(c.replay(&k, 499), out(499), "below A");
    assert_eq!(c.replay(&k, 500), out(500), "at A");
    assert_eq!(c.replay(&k, 1), out(1));
    assert_eq!(c.replay(&k, 501), None, "above A");
    // the run again completes with 700: it replaces the entry, and replays from 700 up only
    assert!(c.record(&k, 10_000_000, &pass(700)));
    assert_eq!(c.replay(&k, 699), None);
    assert_eq!(c.replay(&k, 700), Some(pass(700)));

    // a host fault and an input over the cap are never cached
    let k = key("r", 1, 7);
    assert!(!c.record(&k, 1_000, &outcome(Err(RunFailure::HostFault), 10)));
    assert!(!c.record(&k, 1_000, &outcome(Err(RunFailure::InputTooLarge), 0)));
    assert_eq!(c.replay(&k, 1_000), None);
    assert_eq!(c.len(), 6);

    // the cache lives and dies with its loaded pack
    let (d, mut r) = project(ONE_RULE);
    let first = r.compiled_rules();
    assert!(first.cache().record(&key("r1", 1, 1), 100, &pass(10)));
    assert_eq!(r.read().compiled_rules().unwrap().cache().len(), 1);
    // a replacement that does not load keeps the pack, its rules and its cache together
    set_pack(d.path(), BAD_RULE);
    assert!(matches!(
        r.reload_pack().unwrap(),
        ReloadOutcome::Invalid { .. }
    ));
    let kept = r.read().compiled_rules().unwrap();
    assert!(Arc::ptr_eq(&kept, &first));
    assert!(Arc::ptr_eq(&r.compiled_rules(), &first));
    assert_eq!(r.pack().rules.len(), 1);
    assert_eq!(kept.cache().replay(&key("r1", 1, 1), 100), Some(pass(10)));
    // a pack that loads replaces the rules, and its cache starts empty
    set_pack(d.path(), TWO_RULES);
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    let next = r.read().compiled_rules().unwrap();
    assert!(!Arc::ptr_eq(&next, &first));
    assert!(next.cache().is_empty());
    assert_eq!(next.cache().replay(&key("r1", 1, 1), 100), None);
    // a write publishes under the new pack: its states share the new handle
    write_doc(&mut r, "after");
    assert!(Arc::ptr_eq(&r.read().compiled_rules().unwrap(), &next));
}

#[test]
fn wck3_changed_module_or_input_misses_cache() {
    let rules = cache();
    let c = rules.cache();
    let k = key("r", 1, 1);
    assert!(c.record(&k, 1_000, &pass(10)));
    assert_eq!(c.replay(&k, 1_000), Some(pass(10)));
    // each part of the key on its own
    assert_eq!(c.replay(&key("s", 1, 1), 1_000), None, "another rule");
    assert_eq!(c.replay(&key("r", 2, 1), 1_000), None, "another module");
    assert_eq!(c.replay(&key("r", 1, 2), 1_000), None, "another input");
    // a recorded entry of a changed key leaves the first one alone
    assert!(c.record(
        &key("r", 2, 1),
        1_000,
        &outcome(Err(RunFailure::OutOfFuel), 1_000)
    ));
    assert_eq!(c.replay(&k, 1_000), Some(pass(10)));

    // two modules' digests differ, so their results never share an entry
    let specs = [
        module_rule("one", module(1), None, None),
        module_rule("two", module(2), None, None),
    ];
    let compiled = compile_rule_specs(&standard(), &specs).unwrap();
    let digests: Vec<[u8; 32]> = compiled
        .rules()
        .iter()
        .map(|r| match &r.test {
            CompiledRuleTest::Module(m) => *m.sha256(),
            CompiledRuleTest::Filter(_) => panic!("module rules only"),
        })
        .collect();
    assert_ne!(digests[0], digests[1]);
    let at = |rule: &str, module: [u8; 32]| VerdictKey {
        rule: rule.into(),
        module,
        input: [9; 32],
    };
    assert!(
        compiled
            .cache()
            .record(&at("one", digests[0]), 100, &pass(5))
    );
    assert_eq!(compiled.cache().replay(&at("one", digests[1]), 100), None);
    assert_eq!(compiled.cache().replay(&at("two", digests[0]), 100), None);
    assert_eq!(
        compiled.cache().replay(&at("one", digests[0]), 100),
        Some(pass(5))
    );
}

#[test]
fn wck3_cache_bytes_at_and_past_64_mib() {
    assert_eq!(VERDICT_CACHE_BYTES, 67_108_864);
    let rules = cache();
    let c = rules.cache();
    // an entry counts its key (the name and two 32-byte digests) and its texts
    let sized = |input: u8, total: usize| {
        let k = key("r", 1, input);
        let o = outcome(
            Ok(CheckResult::Fail {
                message: Some("m".repeat(total - 1 - 64 - 1)),
                fix: Some("f".into()),
            }),
            7,
        );
        assert_eq!(VerdictCache::entry_bytes(&k, &o), Some(total));
        (k, o)
    };
    // 64 entries of 1 MiB: exactly the cap, all kept
    let mib = 1_048_576;
    let entries: Vec<_> = (0..64u8).map(|i| sized(i, mib)).collect();
    for (k, o) in &entries {
        assert!(c.record(k, 1_000, o));
    }
    assert_eq!(c.bytes(), VERDICT_CACHE_BYTES);
    assert_eq!(c.len(), 64);
    for (k, o) in &entries {
        assert_eq!(c.replay(k, 1_000).as_ref(), Some(o));
    }
    // one byte past the cap evicts the oldest entry only
    let (k, o) = sized(64, 66);
    assert!(c.record(&k, 1_000, &o));
    assert_eq!(c.len(), 64);
    assert_eq!(c.bytes(), VERDICT_CACHE_BYTES - mib + 66);
    assert_eq!(
        c.replay(&entries[0].0, 1_000),
        None,
        "the oldest went first"
    );
    assert_eq!(c.replay(&entries[1].0, 1_000).as_ref(), Some(&entries[1].1));
    assert_eq!(c.replay(&k, 1_000), Some(o));
    // an evicted result is run again and gives the same outcome as before
    assert!(c.record(&entries[0].0, 1_000, &entries[0].1));
    assert_eq!(c.replay(&entries[0].0, 1_000).as_ref(), Some(&entries[0].1));
    assert_eq!(c.replay(&entries[1].0, 1_000), None, "then the next oldest");
    assert!(c.bytes() <= VERDICT_CACHE_BYTES);

    // one entry of exactly the cap fits alone; one byte more is never kept
    let rules = cache();
    let c = rules.cache();
    let (small, o) = sized(1, 66);
    assert!(c.record(&small, 1_000, &o));
    let (k, o) = sized(2, VERDICT_CACHE_BYTES);
    assert!(c.record(&k, 1_000, &o));
    assert_eq!((c.len(), c.bytes()), (1, VERDICT_CACHE_BYTES));
    assert_eq!(c.replay(&small, 1_000), None);
    let (past, o) = sized(3, VERDICT_CACHE_BYTES + 1);
    assert!(!c.record(&past, 1_000, &o));
    assert_eq!(c.replay(&past, 1_000), None);
    assert_eq!((c.len(), c.bytes()), (1, VERDICT_CACHE_BYTES), "unchanged");
}
