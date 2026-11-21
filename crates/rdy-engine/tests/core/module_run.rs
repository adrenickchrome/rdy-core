//! wck5: a module rule's fuel. Each block may use the smaller of 10000000 fuel and what the
//! rule has left of its 1000000000 for the scan; both exported calls count against both. A run
//! out of fuel is charged its whole allowance, and its cause depends on which budget set that
//! allowance (D-WV7ZS). A cached result replays the fuel it recorded, so a scan with a populated
//! verdict cache gives the same answers and charges as one with an empty cache.
use rdy_check_wasm::config::{BLOCK_FUEL, RULE_FUEL};
use rdy_check_wasm::{CheckResult, RunOutcome, TestPanic, panic_in_next_run_for_tests, run_block};
use rdy_engine::check::module_run::{
    Cause, Judgement, RuleBudget, judge_module, module_runs_on_this_thread_for_tests,
};
use rdy_engine::check::rules::{CompiledRuleTest, CompiledRules, ModuleRule, compile_rule_specs};
use rdy_schema::{ModuleTest, PackSet, RuleInput, RuleSeverity, RuleTestSpec};

/// The output the burner gives: `{"pass":true}`, at offset 0.
const PASS: &[u8] = b"{\"pass\":true}";

/// The bytes one loop step of the burner copies: 8 MiB, which costs 131072 fuel.
const STEP_BYTES: i32 = 8 * 1024 * 1024;

fn leb(mut v: u64) -> Vec<u8> {
    let mut out = vec![];
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn sleb(mut v: i64) -> Vec<u8> {
    let mut out = vec![];
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn vec_of(items: &[Vec<u8>]) -> Vec<u8> {
    [leb(items.len() as u64), items.concat()].concat()
}

fn section(id: u8, body: &[u8]) -> Vec<u8> {
    [vec![id], leb(body.len() as u64), body.to_vec()].concat()
}

fn name(s: &str) -> Vec<u8> {
    [leb(s.len() as u64), s.as_bytes().to_vec()].concat()
}

fn i32c(v: i32) -> Vec<u8> {
    [vec![0x41], sleb(i64::from(v))].concat()
}

/// `memory.copy` within memory 0.
const COPY: [u8; 4] = [0xfc, 10, 0, 0];

/// The burner: a check module whose fuel its input sets, in a 256-page memory.
///
/// - `rdy_alloc(len)` copies `len * 64` bytes within memory, which costs `len` fuel, and
///   returns 1024.
/// - `rdy_check_v1(ptr, len)` reads two `i32` values at `ptr`, `n` and `k`. It copies 8 MiB `n`
///   times, then `k * 64` bytes, and returns `{"pass":true}`.
///
/// So a run's fuel is a fixed amount, plus the input's length, plus `n` loop steps, plus `k`.
fn burner() -> Vec<u8> {
    let types = vec![
        vec![0x60, 1, 0x7f, 1, 0x7f],
        vec![0x60, 2, 0x7f, 0x7f, 1, 0x7e],
    ];
    let alloc = [
        i32c(0),
        i32c(0),
        vec![0x20, 0],
        i32c(64),
        vec![0x6c],
        COPY.to_vec(),
        i32c(1024),
    ]
    .concat();
    let check = [
        // n in local 2, k * 64 in local 3
        vec![0x20, 0, 0x28, 2, 0, 0x21, 2],
        vec![0x20, 0, 0x28, 2, 4],
        i32c(64),
        vec![0x6c, 0x21, 3],
        // while n > 0: copy 8 MiB, n -= 1
        vec![0x02, 0x40, 0x03, 0x40, 0x20, 2, 0x45, 0x0d, 1],
        i32c(0),
        i32c(0),
        i32c(STEP_BYTES),
        COPY.to_vec(),
        vec![0x20, 2],
        i32c(1),
        vec![0x6b, 0x21, 2, 0x0c, 0, 0x0b, 0x0b],
        // copy k * 64 bytes
        i32c(0),
        i32c(0),
        vec![0x20, 3],
        COPY.to_vec(),
        // the output range: offset 0 in the high half, its length in the low half
        vec![0x42],
        sleb(PASS.len() as i64),
    ]
    .concat();
    let body = |locals: Vec<u8>, code: Vec<u8>| {
        let inner = [locals, code, vec![0x0b]].concat();
        [leb(inner.len() as u64), inner].concat()
    };
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    out.extend(section(1, &vec_of(&types)));
    out.extend(section(3, &vec_of(&[leb(0), leb(1)])));
    out.extend(section(5, &vec_of(&[vec![0x00, 0x80, 0x02]])));
    let exports = vec![
        [name("memory"), vec![2, 0]].concat(),
        [name("rdy_alloc"), vec![0, 0]].concat(),
        [name("rdy_check_v1"), vec![0, 1]].concat(),
    ];
    out.extend(section(7, &vec_of(&exports)));
    let code = vec![body(vec![0], alloc), body(vec![1, 2, 0x7f], check)];
    out.extend(section(10, &vec_of(&code)));
    let seg = [
        vec![0x00],
        i32c(0),
        vec![0x0b],
        leb(PASS.len() as u64),
        PASS.to_vec(),
    ]
    .concat();
    out.extend(section(11, &vec_of(&[seg])));
    out
}

/// The burner as the one rule `burn` of a prepared pack, with an empty verdict cache.
fn prepared() -> CompiledRules {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let spec = RuleInput {
        name: "burn".into(),
        select: "type = section".into(),
        test: RuleTestSpec::Module(ModuleTest {
            module: burner(),
            args: None,
            depth: None,
        }),
        severity: RuleSeverity::Blocking,
        message: "m".into(),
        fix: "f".into(),
    };
    compile_rule_specs(&pack, &[spec]).unwrap()
}

fn module_of(rules: &CompiledRules) -> &ModuleRule {
    match &rules.rules()[0].test {
        CompiledRuleTest::Module(m) => m,
        CompiledRuleTest::Filter(_) => panic!("burn is a module rule"),
    }
}

/// The input that makes the burner copy 8 MiB `n` times and then `k * 64` bytes, `len` bytes
/// long (at least 12), its last four bytes `tag` so that inputs of equal fuel stay distinct.
fn input(n: u32, k: u32, len: usize, tag: u32) -> Vec<u8> {
    let mut v = [n.to_le_bytes(), k.to_le_bytes()].concat();
    v.resize(len.max(12), 0);
    let at = v.len() - 4;
    v[at..].copy_from_slice(&tag.to_le_bytes());
    v
}

/// The burner's fixed fuel, for a 12-byte input with `n` and `k` both 0: both calls, and the
/// one fuel for each input byte that `rdy_alloc` copies 64 times, included.
const BASE: u64 = 48;

/// The fuel of one loop step: the 8 MiB copy's 131072 and the step's 13 operators.
const STEP: u64 = 131_085;

/// The 12-byte input whose run uses exactly `fuel`, at least [`BASE`]: whole steps, then `k`.
fn input_for(fuel: u64, tag: u32) -> Vec<u8> {
    let rest = fuel - BASE;
    input((rest / STEP) as u32, (rest % STEP) as u32, 12, tag)
}

/// The burner's run on `bytes` with the block's whole fuel.
fn measured(rules: &CompiledRules, bytes: &[u8]) -> RunOutcome {
    run_block(module_of(rules).module(), bytes, BLOCK_FUEL)
}

fn judge(rules: &CompiledRules, budget: &mut RuleBudget, bytes: Vec<u8>) -> Judgement {
    judge_module("burn", module_of(rules), rules.cache(), budget, || {
        Some(bytes)
    })
    .unwrap()
}

#[test]
fn wck5_block_fuel_at_and_past_limit() {
    let rules = prepared();
    // the burner's costs, pinned: a fixed part, one fuel per input byte in rdy_alloc, a loop
    // step, and one fuel per 64 bytes of the last copy
    let fuel = |bytes: &[u8]| measured(&rules, bytes).fuel;
    assert_eq!(fuel(&input(0, 0, 12, 0)), BASE);
    assert_eq!(fuel(&input(0, 0, 13, 0)), BASE + 1);
    assert_eq!(fuel(&input(0, 1, 12, 0)), BASE + 1);
    assert_eq!(fuel(&input(1, 0, 12, 0)), BASE + STEP);
    assert_eq!(fuel(&input(2, 5, 12, 0)), BASE + 2 * STEP + 5);

    // a run on its last permitted unit keeps its result
    let at = input_for(BLOCK_FUEL, 0);
    assert_eq!(
        measured(&rules, &at),
        RunOutcome {
            result: Ok(CheckResult::Pass),
            fuel: BLOCK_FUEL
        }
    );
    let mut budget = RuleBudget::new();
    assert_eq!(judge(&rules, &mut budget, at.clone()), Judgement::Pass);
    assert_eq!(budget.remaining(), RULE_FUEL - BLOCK_FUEL);

    // one unit more fails with the block's cause and is charged the whole block
    let past = input_for(BLOCK_FUEL + 1, 0);
    let mut budget = RuleBudget::new();
    assert_eq!(
        judge(&rules, &mut budget, past.clone()),
        Judgement::CannotJudge(Cause::BlockFuel)
    );
    assert_eq!(budget.remaining(), RULE_FUEL - BLOCK_FUEL);
    assert_eq!(
        Cause::BlockFuel.words(),
        "the module used all of its 10000000 fuel"
    );

    // the same from the populated cache, with no run
    let runs = module_runs_on_this_thread_for_tests();
    let mut budget = RuleBudget::new();
    assert_eq!(judge(&rules, &mut budget, at), Judgement::Pass);
    assert_eq!(
        judge(&rules, &mut budget, past),
        Judgement::CannotJudge(Cause::BlockFuel)
    );
    assert_eq!(budget.remaining(), RULE_FUEL - 2 * BLOCK_FUEL);
    assert_eq!(module_runs_on_this_thread_for_tests(), runs);
}

/// One scan of `inputs` in order on `rules`, with a new budget: each answer and the fuel left.
fn scan(rules: &CompiledRules, inputs: &[Vec<u8>]) -> (Vec<Judgement>, u64) {
    let mut budget = RuleBudget::new();
    let answers = inputs
        .iter()
        .map(|i| judge(rules, &mut budget, i.clone()))
        .collect();
    (answers, budget.remaining())
}

/// 99 blocks that each use the block's whole fuel, which leaves the rule 10000000, then `tail`.
fn drained_then(tail: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    (0..99u32)
        .map(|i| input_for(BLOCK_FUEL, i))
        .chain(tail)
        .collect()
}

/// An input that runs past any allowance.
fn endless(tag: u32) -> Vec<u8> {
    input(u32::MAX >> 1, 0, 12, tag)
}

/// A scan case: its name, its blocks' inputs, their answers, and the runs an empty cache makes.
type Case = (&'static str, Vec<Vec<u8>>, Vec<Judgement>, u64);

#[test]
fn wck5_rule_fuel_at_and_past_limit() {
    let pass = Judgement::Pass;
    let rule = Judgement::CannotJudge(Cause::RuleFuel);
    let block = Judgement::CannotJudge(Cause::BlockFuel);
    assert_eq!(
        Cause::RuleFuel.words(),
        "the rule used all of its 1000000000 fuel in this run"
    );
    let cases: Vec<Case> = vec![
        // smaller: a block of 5000000 leaves 5000000, below the block's cap; the next block runs
        // out with the rule's cause, charged in full, and every later block has that cause too
        // without a run
        (
            "smaller",
            drained_then(vec![
                input_for(5_000_000, 200),
                endless(201),
                input_for(BASE, 202),
            ]),
            [vec![pass.clone(); 100], vec![rule.clone(), rule.clone()]].concat(),
            101,
        ),
        // equal: the allowance equals the block's cap; the block runs out with the block's cause,
        // and every later block has the rule's
        (
            "equal",
            drained_then(vec![endless(300), input_for(BASE, 301)]),
            [vec![pass.clone(); 99], vec![block, rule.clone()]].concat(),
            100,
        ),
        // last unit: the 100th block finishes on the rule's last unit and keeps its result
        (
            "last unit",
            drained_then(vec![input_for(BLOCK_FUEL, 400), input_for(BASE, 401)]),
            [vec![pass.clone(); 100], vec![rule.clone()]].concat(),
            100,
        ),
        // one unit left: the next block may use one unit, runs out, and has the rule's cause
        (
            "one unit left",
            drained_then(vec![input_for(BLOCK_FUEL - 1, 500), input_for(BASE, 501)]),
            [vec![pass.clone(); 100], vec![rule]].concat(),
            101,
        ),
    ];
    for (what, inputs, want, ran) in cases {
        let rules = prepared();
        // an empty cache
        let runs = module_runs_on_this_thread_for_tests();
        assert_eq!(
            scan(&rules, &inputs),
            (want.clone(), 0),
            "{what}, empty cache"
        );
        assert_eq!(
            module_runs_on_this_thread_for_tests() - runs,
            ran,
            "{what}: runs"
        );
        // the populated cache: the same answers and charges, with no run
        let runs = module_runs_on_this_thread_for_tests();
        assert_eq!(scan(&rules, &inputs), (want, 0), "{what}, populated cache");
        assert_eq!(
            module_runs_on_this_thread_for_tests(),
            runs,
            "{what}: replayed"
        );
    }
}

#[test]
fn wck5_both_calls_charge_both_budgets() {
    let rules = prepared();
    // rdy_alloc's share grows with the input's length, rdy_check_v1's with n and k
    let alloc_share = 200_000u64;
    let len = 12 + alloc_share as usize;
    let split = |check_share: u64, tag: u32| {
        input(
            (check_share / STEP) as u32,
            (check_share % STEP) as u32,
            len,
            tag,
        )
    };
    let full = BLOCK_FUEL - BASE - alloc_share;
    // the two calls together use exactly the block's fuel: a pass, charged in full
    let at = split(full, 0);
    assert_eq!(measured(&rules, &at).fuel, BLOCK_FUEL);
    let mut budget = RuleBudget::new();
    assert_eq!(judge(&rules, &mut budget, at), Judgement::Pass);
    assert_eq!(budget.remaining(), RULE_FUEL - BLOCK_FUEL);
    // one unit more fails with the block's cause, though neither call alone reaches the cap
    let past = split(full + 1, 0);
    let mut budget = RuleBudget::new();
    assert_eq!(
        judge(&rules, &mut budget, past),
        Judgement::CannotJudge(Cause::BlockFuel)
    );
    assert_eq!(budget.remaining(), RULE_FUEL - BLOCK_FUEL);

    // rdy_alloc's fuel counts against the rule too: with 300000 left, a block whose rdy_alloc
    // uses 200000 and whose run needs 300001 in all has the rule's cause
    let mut inputs: Vec<Vec<u8>> = (0..99u32).map(|i| input_for(BLOCK_FUEL, i)).collect();
    inputs.push(input_for(BLOCK_FUEL - 300_000, 99));
    let mut budget = RuleBudget::new();
    for i in &inputs {
        assert_eq!(judge(&rules, &mut budget, i.clone()), Judgement::Pass);
    }
    assert_eq!(budget.remaining(), 300_000);
    let over = split(300_001 - BASE - alloc_share, 100);
    assert_eq!(measured(&rules, &over).fuel, 300_001);
    assert_eq!(
        judge(&rules, &mut budget, over),
        Judgement::CannotJudge(Cause::RuleFuel)
    );
    assert_eq!(budget.remaining(), 0);
}

#[test]
fn wck5_rule_budgets_reset_between_scans() {
    let rules = prepared();
    let inputs: Vec<Vec<u8>> = (0..101u32).map(|i| input_for(BLOCK_FUEL, i)).collect();
    let mut want = vec![Judgement::Pass; 100];
    want.push(Judgement::CannotJudge(Cause::RuleFuel));
    // a first scan uses all of the rule's fuel
    let runs = module_runs_on_this_thread_for_tests();
    assert_eq!(scan(&rules, &inputs), (want.clone(), 0));
    assert_eq!(module_runs_on_this_thread_for_tests() - runs, 100);
    // the next scan has a whole new budget, and its cache hits use it up the same way
    let runs = module_runs_on_this_thread_for_tests();
    assert_eq!(scan(&rules, &inputs), (want, 0));
    assert_eq!(module_runs_on_this_thread_for_tests(), runs);
    let mut budget = RuleBudget::new();
    assert_eq!(budget.remaining(), RULE_FUEL);
    assert_eq!(budget.allowance(), BLOCK_FUEL);
    assert_eq!(
        judge(&rules, &mut budget, inputs[0].clone()),
        Judgement::Pass
    );
    assert_eq!(budget.remaining(), RULE_FUEL - BLOCK_FUEL);
    // a hit charges a new scan's budget the fuel it recorded
    let small = input_for(1_000, 7_000);
    assert_eq!(judge(&rules, &mut budget, small.clone()), Judgement::Pass);
    let runs = module_runs_on_this_thread_for_tests();
    let mut fresh = RuleBudget::new();
    assert_eq!(judge(&rules, &mut fresh, small), Judgement::Pass);
    assert_eq!(fresh.remaining(), RULE_FUEL - 1_000);
    assert_eq!(module_runs_on_this_thread_for_tests(), runs);
}

#[test]
fn wck5_host_panics_do_not_become_rows() {
    let rules = prepared();
    let bytes = input(0, 0, 12, 1);
    for at in [TestPanic::Instantiation, TestPanic::Alloc, TestPanic::Check] {
        panic_in_next_run_for_tests(at);
        let mut budget = RuleBudget::new();
        let e = judge_module(
            "burn",
            module_of(&rules),
            rules.cache(),
            &mut budget,
            || Some(bytes.clone()),
        )
        .unwrap_err();
        // the rule read error, never a row, with no panic payload
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_INTERNAL",
                "the rule checks could not be read: check module host fault",
                "report this as a bug in rdy"
            ),
            "{at:?}"
        );
        // nothing is charged or cached
        assert_eq!(budget.remaining(), RULE_FUEL, "{at:?}");
        assert!(rules.cache().is_empty(), "{at:?}");
    }
    // the next run judges the block
    let mut budget = RuleBudget::new();
    assert_eq!(judge(&rules, &mut budget, bytes), Judgement::Pass);
    assert_eq!(budget.remaining(), RULE_FUEL - BASE);

    // an input over the cap fails the block with its cause and uses no fuel
    let mut budget = RuleBudget::new();
    let j = judge_module(
        "burn",
        module_of(&rules),
        rules.cache(),
        &mut budget,
        || None,
    );
    assert_eq!(j.unwrap(), Judgement::CannotJudge(Cause::InputTooLarge));
    assert_eq!(budget.remaining(), RULE_FUEL);
    assert_eq!(Cause::InputTooLarge.words(), "the input is over 1 MiB");
}
