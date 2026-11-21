//! wck5: one block runs in a fresh store and instance, with fixed caps on memory, tables, call
//! depth, value stack, fuel and output, and a closed result shape. Every range the guest gives is
//! checked as unsigned 32-bit values before memory is read or written. A trap in either call
//! takes the same path, and a panic of the host is a host fault, never a result.
use guest::*;
use rdy_check_wasm::config::{BLOCK_FUEL, INPUT_BYTES_MAX, OUTPUT_BYTES_MAX};
use rdy_check_wasm::{
    CheckResult, CompiledModule, RunFailure, RunOutcome, TestPanic, compile_module, decode_result,
    panic_in_next_run_for_tests, run_block,
};

/// A small encoder of check modules and of the instructions their two functions run.
mod guest {
    pub const I32: u8 = 0x7f;
    pub const I64: u8 = 0x7e;
    pub const FUNCREF: u8 = 0x70;
    pub const EMPTY: u8 = 0x40;

    pub const UNREACHABLE: u8 = 0x00;
    pub const LOOP: u8 = 0x03;
    pub const IF: u8 = 0x04;
    pub const END: u8 = 0x0b;
    pub const BR: u8 = 0x0c;
    pub const BR_IF: u8 = 0x0d;
    pub const CALL: u8 = 0x10;
    pub const DROP: u8 = 0x1a;
    pub const LOCAL_GET: u8 = 0x20;
    pub const LOCAL_SET: u8 = 0x21;
    pub const GLOBAL_GET: u8 = 0x23;
    pub const GLOBAL_SET: u8 = 0x24;
    pub const I32_NE: u8 = 0x47;
    pub const I32_SUB: u8 = 0x6b;
    pub const I32_DIV_U: u8 = 0x6e;
    pub const I64_OR: u8 = 0x84;
    pub const I64_SHL: u8 = 0x86;
    pub const I64_EXTEND_I32_U: u8 = 0xad;

    pub fn leb(mut v: u64) -> Vec<u8> {
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

    pub fn sleb(mut v: i64) -> Vec<u8> {
        let mut out = vec![];
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
            if done {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }

    pub fn i32c(v: i32) -> Vec<u8> {
        [vec![0x41], sleb(i64::from(v))].concat()
    }

    pub fn i64c(v: i64) -> Vec<u8> {
        [vec![0x42], sleb(v)].concat()
    }

    /// The `i64` result of `rdy_check_v1` for the output at `at`, `len` bytes long.
    pub fn out(at: u32, len: u32) -> Vec<u8> {
        i64c(((u64::from(at) << 32) | u64::from(len)) as i64)
    }

    pub fn memory_grow() -> Vec<u8> {
        vec![0x40, 0x00]
    }

    pub fn memory_size() -> Vec<u8> {
        vec![0x3f, 0x00]
    }

    pub fn i32_load(offset: u32) -> Vec<u8> {
        [vec![0x28, 0x02], leb(u64::from(offset))].concat()
    }

    pub fn i32_load8_u(offset: u32) -> Vec<u8> {
        [vec![0x2d, 0x00], leb(u64::from(offset))].concat()
    }

    pub fn i32_store8(offset: u32) -> Vec<u8> {
        [vec![0x3a, 0x00], leb(u64::from(offset))].concat()
    }

    pub fn table_grow() -> Vec<u8> {
        vec![0xfc, 15, 0x00]
    }

    pub fn ref_null_func() -> Vec<u8> {
        vec![0xd0, FUNCREF]
    }

    fn name(s: &str) -> Vec<u8> {
        [leb(s.len() as u64), s.as_bytes().to_vec()].concat()
    }

    fn vec_of(items: &[Vec<u8>]) -> Vec<u8> {
        [leb(items.len() as u64), items.concat()].concat()
    }

    fn section(id: u8, body: &[u8]) -> Vec<u8> {
        [vec![id], leb(body.len() as u64), body.to_vec()].concat()
    }

    pub fn func_type(params: &[u8], results: &[u8]) -> Vec<u8> {
        [
            vec![0x60],
            leb(params.len() as u64),
            params.to_vec(),
            leb(results.len() as u64),
            results.to_vec(),
        ]
        .concat()
    }

    pub fn limits(min: u64, max: Option<u64>) -> Vec<u8> {
        match max {
            None => [vec![0x00], leb(min)].concat(),
            Some(max) => [vec![0x01], leb(min), leb(max)].concat(),
        }
    }

    /// A function body: its locals as (count, type) runs, then `code`, then `end`.
    pub fn body(locals: &[(u64, u8)], code: &[u8]) -> Vec<u8> {
        let decls: Vec<Vec<u8>> = locals
            .iter()
            .map(|(n, t)| [leb(*n), vec![*t]].concat())
            .collect();
        let inner = [vec_of(&decls), code.to_vec(), vec![END]].concat();
        [leb(inner.len() as u64), inner].concat()
    }

    /// The output that every module's data segment holds at offset 0 unless a test replaces it.
    pub const PASS: &[u8] = b"{\"pass\":true}";

    /// A check module. Type 0 is `(i32) -> i32`, type 1 `(i32, i32) -> i64`, type 2 `() -> ()`
    /// and type 3 `(i32) -> ()`. Function 0 is `rdy_alloc`, function 1 `rdy_check_v1`. The
    /// defaults give a module whose `rdy_alloc` returns 1024 and whose `rdy_check_v1` returns
    /// [`PASS`], held at offset 0, in a one-page memory.
    #[derive(Clone)]
    pub struct Guest {
        pub types: Vec<Vec<u8>>,
        /// Each function's type index and encoded body.
        pub funcs: Vec<(u32, Vec<u8>)>,
        pub tables: Vec<Vec<u8>>,
        pub memory: Vec<u8>,
        pub globals: Vec<Vec<u8>>,
        /// Active data segments of memory 0: offset and bytes.
        pub data: Vec<(u32, Vec<u8>)>,
    }

    impl Guest {
        pub fn new() -> Guest {
            Guest {
                types: vec![
                    func_type(&[I32], &[I32]),
                    func_type(&[I32, I32], &[I64]),
                    func_type(&[], &[]),
                    func_type(&[I32], &[]),
                ],
                funcs: vec![
                    (0, body(&[], &i32c(1024))),
                    (1, body(&[], &out(0, PASS.len() as u32))),
                ],
                tables: vec![],
                memory: limits(1, None),
                globals: vec![],
                data: vec![(0, PASS.to_vec())],
            }
        }

        /// `rdy_alloc` runs `code`, with `locals`.
        pub fn alloc(mut self, locals: &[(u64, u8)], code: &[u8]) -> Guest {
            self.funcs[0].1 = body(locals, code);
            self
        }

        /// `rdy_check_v1` runs `code`, with `locals`.
        pub fn check(mut self, locals: &[(u64, u8)], code: &[u8]) -> Guest {
            self.funcs[1].1 = body(locals, code);
            self
        }

        /// The module with one more function of type `ty`; its index.
        pub fn func(&mut self, ty: u32, locals: &[(u64, u8)], code: &[u8]) -> u32 {
            self.funcs.push((ty, body(locals, code)));
            (self.funcs.len() - 1) as u32
        }

        pub fn pages(mut self, min: u64, max: Option<u64>) -> Guest {
            self.memory = limits(min, max);
            self
        }

        pub fn bytes(&self) -> Vec<u8> {
            let mut out = b"\0asm\x01\0\0\0".to_vec();
            out.extend(section(1, &vec_of(&self.types)));
            let funcs: Vec<Vec<u8>> = self.funcs.iter().map(|f| leb(u64::from(f.0))).collect();
            out.extend(section(3, &vec_of(&funcs)));
            if !self.tables.is_empty() {
                out.extend(section(4, &vec_of(&self.tables)));
            }
            out.extend(section(5, &vec_of(std::slice::from_ref(&self.memory))));
            if !self.globals.is_empty() {
                out.extend(section(6, &vec_of(&self.globals)));
            }
            let exports = vec![
                [name("memory"), vec![2], leb(0)].concat(),
                [name("rdy_alloc"), vec![0], leb(0)].concat(),
                [name("rdy_check_v1"), vec![0], leb(1)].concat(),
            ];
            out.extend(section(7, &vec_of(&exports)));
            let code: Vec<Vec<u8>> = self.funcs.iter().map(|f| f.1.clone()).collect();
            out.extend(section(10, &vec_of(&code)));
            if !self.data.is_empty() {
                let segs: Vec<Vec<u8>> = self
                    .data
                    .iter()
                    .map(|(at, bytes)| {
                        [
                            vec![0x00],
                            i32c(*at as i32),
                            vec![END],
                            leb(bytes.len() as u64),
                            bytes.clone(),
                        ]
                        .concat()
                    })
                    .collect();
                out.extend(section(11, &vec_of(&segs)));
            }
            out
        }

        pub fn compile(&self) -> super::CompiledModule {
            match super::compile_module(&self.bytes()) {
                Ok(m) => m,
                Err(e) => panic!("refused: {e:?}"),
            }
        }
    }

    /// The code of a `rdy_check_v1` that returns its own input as the output: the input range,
    /// as the result's two halves.
    pub fn echo_code() -> Vec<u8> {
        [
            vec![LOCAL_GET, 0, I64_EXTEND_I32_U],
            i64c(32),
            vec![I64_SHL, LOCAL_GET, 1, I64_EXTEND_I32_U, I64_OR],
        ]
        .concat()
    }

    /// A module whose output is its input, with `pages` of memory. Its input goes at 1024.
    pub fn echo(pages: u64) -> Guest {
        Guest::new().pages(pages, None).check(&[], &echo_code())
    }
}

/// The fuel every test run gets, unless it measures fuel.
const FUEL: u64 = BLOCK_FUEL;

fn run(m: &CompiledModule, input: &[u8]) -> RunOutcome {
    run_block(m, input, FUEL)
}

fn result(m: &CompiledModule, input: &[u8]) -> Result<CheckResult, RunFailure> {
    run(m, input).result
}

fn bad(detail: &str) -> Result<CheckResult, RunFailure> {
    Err(RunFailure::BadOutput(detail.to_string()))
}

fn trap(words: &str) -> Result<CheckResult, RunFailure> {
    Err(RunFailure::Trap(words.to_string()))
}

fn fail(message: Option<&str>, fix: Option<&str>) -> Result<CheckResult, RunFailure> {
    Ok(CheckResult::Fail {
        message: message.map(str::to_string),
        fix: fix.map(str::to_string),
    })
}

#[test]
fn wck5_one_instance_and_fresh_state() {
    // `rdy_alloc` marks global 1, and `rdy_check_v1` passes only when that mark is there (the
    // two calls share one instance) and when global 0 and the byte at 2048 are still zero (no
    // earlier block's instance left them set). Then it sets both.
    let mut g = Guest::new();
    g.globals.push(vec![I32, 0x01, 0x41, 0x00, END]);
    g.globals.push(vec![I32, 0x01, 0x41, 0x00, END]);
    let g = g
        .alloc(&[], &[i32c(7), vec![GLOBAL_SET, 1], i32c(1024)].concat())
        .check(
            &[],
            &[
                // global 1 is 7
                vec![GLOBAL_GET, 1],
                i32c(7),
                vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
                // global 0 and the byte at 2048 are 0
                vec![GLOBAL_GET, 0, IF, EMPTY, UNREACHABLE, END],
                i32c(0),
                i32_load8_u(2048),
                vec![IF, EMPTY, UNREACHABLE, END],
                // set both
                i32c(1),
                vec![GLOBAL_SET, 0],
                i32c(0),
                i32c(1),
                i32_store8(2048),
                out(0, PASS.len() as u32),
            ]
            .concat(),
        );
    let m = g.compile();
    let first = run(&m, b"{}");
    assert_eq!(first.result, Ok(CheckResult::Pass));
    // each run starts from the module's own state, and costs the same
    for _ in 0..3 {
        assert_eq!(run(&m, b"{}"), first);
    }
    // a second compiled copy of the same bytes gives the same outcome
    assert_eq!(run(&g.compile(), b"{}"), first);
}

#[test]
fn wck5_memory_grow_at_and_past_256_pages() {
    // from one page, 255 more pages grow (the old size, 1, comes back), then one more page gives
    // -1 and no trap, and the memory holds 256 pages
    let code = [
        i32c(255),
        memory_grow(),
        i32c(1),
        vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
        i32c(1),
        memory_grow(),
        i32c(-1),
        vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
        memory_size(),
        i32c(256),
        vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
        out(0, PASS.len() as u32),
    ]
    .concat();
    for max in [None, Some(256), Some(65_536)] {
        let m = Guest::new().pages(1, max).check(&[], &code).compile();
        assert_eq!(result(&m, b"{}"), Ok(CheckResult::Pass), "maximum {max:?}");
    }
    // a declared maximum below the cap still stops growth at that maximum
    let m = Guest::new().pages(1, Some(2)).check(&[], &code).compile();
    assert_eq!(
        result(&m, b"{}"),
        trap("wasm `unreachable` instruction executed")
    );

    // a memory that starts at 256 pages grows by none and by no page more
    let code = [
        i32c(0),
        memory_grow(),
        i32c(256),
        vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
        i32c(1),
        memory_grow(),
        i32c(-1),
        vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
        out(0, PASS.len() as u32),
    ]
    .concat();
    let m = Guest::new().pages(256, None).check(&[], &code).compile();
    assert_eq!(result(&m, b"{}"), Ok(CheckResult::Pass));

    // a growth far past the cap, and one that would pass 4 GiB, give -1 too
    for pages in [65_536, i32::MAX] {
        let code = [
            i32c(pages),
            memory_grow(),
            i32c(-1),
            vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
            out(0, PASS.len() as u32),
        ]
        .concat();
        let m = Guest::new().check(&[], &code).compile();
        assert_eq!(result(&m, b"{}"), Ok(CheckResult::Pass), "{pages} pages");
    }

    // A table grows the same way: to 10000 elements, then -1 for one element more.
    let code = [
        ref_null_func(),
        i32c(10_000),
        table_grow(),
        i32c(0),
        vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
        ref_null_func(),
        i32c(1),
        table_grow(),
        i32c(-1),
        vec![I32_NE, IF, EMPTY, UNREACHABLE, END],
        out(0, PASS.len() as u32),
    ]
    .concat();
    let mut g = Guest::new().check(&[], &code);
    g.tables.push([vec![FUNCREF], limits(0, None)].concat());
    assert_eq!(result(&g.compile(), b"{}"), Ok(CheckResult::Pass));
}

/// A module whose `rdy_check_v1` calls function 2, `f(n)`, with the `i32` at the start of its
/// input; `f(n)` calls `f(n - 1)` until `n` is 0. A run with input `n` nests `n + 2` frames:
/// `rdy_check_v1` and `f(n)` to `f(0)`.
fn recursion() -> CompiledModule {
    let mut g = Guest::new();
    let f = g.func(
        3,
        &[],
        &[
            vec![LOCAL_GET, 0, IF, EMPTY, LOCAL_GET, 0],
            i32c(1),
            vec![I32_SUB, CALL, 2, END],
        ]
        .concat(),
    );
    assert_eq!(f, 2);
    g.check(
        &[],
        &[
            vec![LOCAL_GET, 0],
            i32_load(0),
            vec![CALL, 2],
            out(0, PASS.len() as u32),
        ]
        .concat(),
    )
    .compile()
}

fn frames(m: &CompiledModule, frames: u32) -> Result<CheckResult, RunFailure> {
    result(m, &(frames - 2).to_le_bytes())
}

#[test]
fn wck5_call_depth_at_and_past_1024() {
    let m = recursion();
    // 1024 nested frames run; the 1025th is refused, as a trap
    assert_eq!(frames(&m, 1024), Ok(CheckResult::Pass));
    assert_eq!(frames(&m, 1025), trap("call stack exhausted"));
    assert_eq!(frames(&m, 2), Ok(CheckResult::Pass));
    assert_eq!(frames(&m, 100_000), trap("call stack exhausted"));
    // the depth is the same on every run: no stack is kept from the last one
    assert_eq!(frames(&m, 1024), Ok(CheckResult::Pass));

    // The interpreter's portable dispatch loop keeps the host's stack flat in every build
    // profile, this test's unoptimised one too: the deepest nesting and a long loop run on a
    // thread with a quarter of the usual 2 MiB stack.
    let (deep, long) = (recursion(), looper());
    let host = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let deep = frames(&deep, 1024);
            let long = run(&long, &1_000_000u32.to_le_bytes());
            (deep, long)
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(host.0, Ok(CheckResult::Pass));
    assert_eq!(
        host.1,
        RunOutcome {
            result: Ok(CheckResult::Pass),
            fuel: 7_000_008
        }
    );
}

/// A module whose `rdy_check_v1` loops `n` times, `n` the `i32` at the start of its input.
fn looper() -> CompiledModule {
    Guest::new()
        .check(
            &[(1, I32)],
            &[
                vec![LOCAL_GET, 0],
                i32_load(0),
                vec![LOCAL_SET, 2, LOOP, EMPTY, LOCAL_GET, 2],
                i32c(1),
                vec![I32_SUB, LOCAL_SET, 2, LOCAL_GET, 2, BR_IF, 0, END],
                out(0, PASS.len() as u32),
            ]
            .concat(),
        )
        .compile()
}

/// A module whose `rdy_check_v1` has `a` `i64` locals besides its two parameters and calls
/// function 2, `() -> ()` with `b` `i64` locals and an empty body.
///
/// The pinned interpreter counts the value stack in 8-byte cells, so 524288 bytes are 65536
/// cells. A frame takes twice its locals, parameters included, plus its operand slots. A callee's
/// frame starts at its caller's first argument slot, here the first slot past the caller's
/// locals. So this run reaches cell `2 + a + 2 * b`. A function holds at most 30000 locals,
/// parameters included, so no one frame reaches the cap.
fn stack(a: u64, b: u64) -> CompiledModule {
    let mut g = Guest::new();
    let inner = g.func(2, &[(b, I64)], &[]);
    g.check(
        &[(a, I64)],
        &[vec![CALL, inner as u8], out(0, PASS.len() as u32)].concat(),
    )
    .compile()
}

#[test]
fn wck5_value_stack_at_and_past_524288_bytes() {
    const CELLS: u64 = 524_288 / 8;
    // the deepest cell is exactly the last of the 65536
    for (a, b) in [(5_534, 30_000), (29_998, 17_768)] {
        assert_eq!(2 + a + 2 * b, CELLS);
        assert_eq!(
            result(&stack(a, b), b"{}"),
            Ok(CheckResult::Pass),
            "{a} {b}"
        );
    }
    // one cell more traps, and so do two
    assert_eq!(
        result(&stack(5_535, 30_000), b"{}"),
        trap("call stack exhausted")
    );
    assert_eq!(
        result(&stack(29_998, 17_769), b"{}"),
        trap("call stack exhausted")
    );
    // one cell fewer runs, and a callee local costs two cells
    assert_eq!(result(&stack(5_533, 30_000), b"{}"), Ok(CheckResult::Pass));
    assert_eq!(result(&stack(5_536, 29_999), b"{}"), Ok(CheckResult::Pass));
    assert_eq!(
        result(&stack(5_537, 29_999), b"{}"),
        trap("call stack exhausted")
    );
    // 30001 locals in one function, parameters included, do not load
    let mut g = Guest::new();
    g.func(2, &[(30_001, I64)], &[]);
    assert_eq!(
        compile_module(&g.bytes()).err(),
        Some(rdy_check_wasm::CompileFailure::Invalid(
            "encountered function with too many function parameters".into()
        ))
    );
    let g = Guest::new().check(&[(29_999, I64)], &out(0, PASS.len() as u32));
    assert!(compile_module(&g.bytes()).is_err());
    // the stack is the same on every run: no stack is kept from the last one
    assert_eq!(result(&stack(5_534, 30_000), b"{}"), Ok(CheckResult::Pass));
}

/// The output `{"pass":true}` and then spaces, `len` bytes in all.
fn padded_pass(len: usize) -> Vec<u8> {
    let mut v = PASS.to_vec();
    v.resize(len, b' ');
    v
}

#[test]
fn wck5_output_at_and_past_64_kib() {
    let m = echo(3).compile();
    assert_eq!(OUTPUT_BYTES_MAX, 65_536);
    assert_eq!(result(&m, &padded_pass(65_536)), Ok(CheckResult::Pass));
    assert_eq!(
        result(&m, &padded_pass(65_537)),
        Err(RunFailure::OutputTooLarge)
    );
    // The length is read before the range: a length past the cap is too large wherever it
    // points, even outside memory, and the largest u32 does not wrap.
    for len in [65_537u32, 0x8000_0000, u32::MAX] {
        for at in [0u32, u32::MAX] {
            let m = Guest::new().check(&[], &out(at, len)).compile();
            assert_eq!(
                result(&m, b"{}"),
                Err(RunFailure::OutputTooLarge),
                "{at} {len}"
            );
        }
    }
}

/// A failing result with `message` and `fix`, as output bytes.
fn failing(message: &str, fix: &str) -> Vec<u8> {
    format!(r#"{{"pass":false,"message":"{message}","fix":"{fix}"}}"#).into_bytes()
}

#[test]
fn wck5_result_text_at_and_past_2000_characters() {
    let m = echo(2).compile();
    // 2000 characters of one, two, three and four UTF-8 bytes each, and of escapes
    for c in ["a", "é", "€", "😀", "\\u00e9", "\\ud83d\\ude00"] {
        let at = c.repeat(2000);
        let past = c.repeat(2001);
        let words = |s: &str| s.replace("\\u00e9", "é").replace("\\ud83d\\ude00", "😀");
        assert_eq!(
            result(&m, &failing(&at, "f")),
            fail(Some(&words(&at)), Some("f")),
            "{c}"
        );
        assert_eq!(
            result(&m, &failing("m", &at)),
            fail(Some("m"), Some(&words(&at))),
            "{c}"
        );
        assert_eq!(
            result(&m, &failing(&past, "f")),
            bad("message is not 1 to 2000 characters"),
            "{c}"
        );
        assert_eq!(
            result(&m, &failing("m", &past)),
            bad("fix is not 1 to 2000 characters"),
            "{c}"
        );
    }
    // one character is the least; an empty text is refused
    assert_eq!(result(&m, &failing("m", "f")), fail(Some("m"), Some("f")));
    assert_eq!(
        result(&m, &failing("", "f")),
        bad("message is not 1 to 2000 characters")
    );
    assert_eq!(
        result(&m, &failing("m", "")),
        bad("fix is not 1 to 2000 characters")
    );
}

#[test]
fn wck5_alloc_and_output_ranges_use_unsigned_checked_bounds() {
    const PAGE: u32 = 65_536;
    let input = b"{\"pass\":true}";
    let len = input.len() as u32;
    // `rdy_alloc` returns `at`, and `rdy_check_v1` echoes its input
    let alloc_at = |at: u32| {
        Guest::new()
            .alloc(&[], &i32c(at as i32))
            .check(&[], &echo_code())
            .compile()
    };
    // the range ends at the end of memory: in
    assert_eq!(result(&alloc_at(PAGE - len), input), Ok(CheckResult::Pass));
    // one byte past it, at the very top of 32 bits, and where a 32-bit sum would wrap: out
    for at in [
        PAGE - len + 1,
        PAGE,
        0x8000_0000,
        u32::MAX - len + 1,
        u32::MAX,
    ] {
        assert_eq!(
            result(&alloc_at(at), input),
            bad("rdy_alloc gave a range outside memory"),
            "{at}"
        );
    }
    // an empty input fits at the very end of memory, not past it
    let m = alloc_at(PAGE);
    assert_eq!(result(&m, b""), bad("it is not a JSON object"));
    let m = alloc_at(PAGE + 1);
    assert_eq!(
        result(&m, b""),
        bad("rdy_alloc gave a range outside memory")
    );

    // the output range, read as two unsigned halves, against the memory after the call
    let output_at = |at: u32, n: u32| Guest::new().check(&[], &out(at, n)).compile();
    let m = Guest::new().check(&[], &out(PAGE - len, len)).compile();
    // the PASS bytes are at 0 only, so the top of memory holds zeros: not JSON, but in range
    assert_eq!(result(&m, b"{}"), bad("it is not a JSON object"));
    for (at, n) in [
        (PAGE - len + 1, len),
        (PAGE, 1),
        (0x8000_0000, len),
        (u32::MAX, len),
        (u32::MAX - len + 1, len),
        (u32::MAX, OUTPUT_BYTES_MAX as u32),
    ] {
        assert_eq!(
            result(&output_at(at, n), b"{}"),
            bad("the output range is outside memory"),
            "{at} {n}"
        );
    }
    // the output range is checked against the memory after the call, which may have grown
    let m = Guest::new()
        .check(
            &[],
            &[i32c(1), memory_grow(), vec![DROP], out(PAGE, len)].concat(),
        )
        .compile();
    assert_eq!(result(&m, b"{}"), bad("it is not a JSON object"));
    assert_eq!(
        result(&output_at(PAGE, len), b"{}"),
        bad("the output range is outside memory")
    );

    // the input: 1 MiB is copied; one byte more runs nothing and uses no fuel
    let m = echo(17).compile();
    let mut big = padded_pass(INPUT_BYTES_MAX);
    assert_eq!(result(&m, &big), Err(RunFailure::OutputTooLarge));
    big.push(b' ');
    assert_eq!(
        run(&m, &big),
        RunOutcome {
            result: Err(RunFailure::InputTooLarge),
            fuel: 0
        }
    );
}

#[test]
fn wck5_result_shapes_and_utf8_are_closed() {
    let m = echo(2).compile();
    let cases: Vec<(&[u8], Result<CheckResult, RunFailure>)> = vec![
        // the shapes a result takes
        (br#"{"pass":true}"#, Ok(CheckResult::Pass)),
        (br#"{"pass":false}"#, fail(None, None)),
        (br#"{"pass":false,"message":"m"}"#, fail(Some("m"), None)),
        (br#"{"pass":false,"fix":"f"}"#, fail(None, Some("f"))),
        (
            br#"{"fix":"f","message":"m","pass":false}"#,
            fail(Some("m"), Some("f")),
        ),
        (
            b" \t\r\n{ \"pass\" : false , \"message\" : \"m\" } \n",
            fail(Some("m"), None),
        ),
        (
            br#"{"pass":false,"message":"a\"b\\c\/d\b\f\n\r\tA"}"#,
            fail(Some("a\"b\\c/d\u{8}\u{c}\n\r\tA"), None),
        ),
        // not UTF-8
        (
            b"{\"pass\":false,\"message\":\"\xff\"}",
            bad("it is not UTF-8"),
        ),
        (b"\xc3", bad("it is not UTF-8")),
        // not an object
        (b"", bad("it is not a JSON object")),
        (b"   ", bad("it is not a JSON object")),
        (b"true", bad("it is not a JSON object")),
        (br#"[{"pass":true}]"#, bad("it is not a JSON object")),
        (br#""pass""#, bad("it is not a JSON object")),
        // not JSON
        (b"{", bad("it is not JSON")),
        (br#"{"pass":true"#, bad("it is not JSON")),
        (br#"{"pass":true,}"#, bad("it is not JSON")),
        (br#"{"pass" true}"#, bad("it is not JSON")),
        (br#"{pass:true}"#, bad("it is not JSON")),
        (br#"{'pass':true}"#, bad("it is not JSON")),
        (br#"{"pass":true;}"#, bad("it is not JSON")),
        (
            b"{\"pass\":false,\"message\":\"a\nb\"}",
            bad("it is not JSON"),
        ),
        (br#"{"pass":false,"message":"\x"}"#, bad("it is not JSON")),
        (
            br#"{"pass":false,"message":"\u00g0"}"#,
            bad("it is not JSON"),
        ),
        (
            br#"{"pass":false,"message":"\ud800"}"#,
            bad("it is not JSON"),
        ),
        (
            br#"{"pass":false,"message":"\udc00"}"#,
            bad("it is not JSON"),
        ),
        (
            br#"{"pass":false,"message":"\ud800A"}"#,
            bad("it is not JSON"),
        ),
        (br#"{"pass":false,"message":"abc"#, bad("it is not JSON")),
        // keys: only pass, message and fix, each once
        (br#"{}"#, bad("it lacks pass")),
        (br#"{"message":"m"}"#, bad("it lacks pass")),
        (br#"{"pass":true,"pass":true}"#, bad("it gives a key twice")),
        (
            br#"{"pass":false,"message":"m","message":"m"}"#,
            bad("it gives a key twice"),
        ),
        (
            br#"{"pass":true,"extra":1}"#,
            bad("it has a key other than pass, message and fix"),
        ),
        (
            br#"{"Pass":true}"#,
            bad("it has a key other than pass, message and fix"),
        ),
        (
            br#"{"pass\u0000":true}"#,
            bad("it has a key other than pass, message and fix"),
        ),
        // value types
        (br#"{"pass":1}"#, bad("pass is not true or false")),
        (br#"{"pass":"true"}"#, bad("pass is not true or false")),
        (br#"{"pass":null}"#, bad("pass is not true or false")),
        (br#"{"pass":True}"#, bad("pass is not true or false")),
        (
            br#"{"pass":false,"message":1}"#,
            bad("message is not a string"),
        ),
        (
            br#"{"pass":false,"message":null}"#,
            bad("message is not a string"),
        ),
        (br#"{"pass":false,"fix":["f"]}"#, bad("fix is not a string")),
        // a pass carries no text
        (
            br#"{"pass":true,"message":"m"}"#,
            bad("a passing result has a message or fix"),
        ),
        (
            br#"{"fix":"f","pass":true}"#,
            bad("a passing result has a message or fix"),
        ),
        // nothing after the object
        (
            br#"{"pass":true}{"pass":true}"#,
            bad("it has more after the JSON object"),
        ),
        (
            br#"{"pass":true} x"#,
            bad("it has more after the JSON object"),
        ),
        (
            b"{\"pass\":true}\0",
            bad("it has more after the JSON object"),
        ),
    ];
    for (bytes, want) in cases {
        let got = result(&m, bytes);
        assert_eq!(got, want, "{}", String::from_utf8_lossy(bytes));
        // the decoder alone gives the same
        let direct = decode_result(bytes).map_err(RunFailure::BadOutput);
        assert_eq!(direct, want, "{}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn wck5_alloc_and_check_traps_have_the_same_failure_path() {
    // the same trap in either call gives the same failure; a trap in `rdy_alloc` copies no input
    let traps: [(&str, Vec<u8>); 3] = [
        ("wasm `unreachable` instruction executed", vec![UNREACHABLE]),
        (
            "integer divide by zero",
            [i32c(1), i32c(0), vec![I32_DIV_U]].concat(),
        ),
        (
            "out of bounds memory access",
            [i32c(-1), i32_load(0)].concat(),
        ),
    ];
    for (words, code) in traps {
        let in_alloc = Guest::new().alloc(&[], &code).compile();
        let in_check = Guest::new()
            .check(&[], &[code.clone(), vec![DROP], out(0, 13)].concat())
            .compile();
        assert_eq!(result(&in_alloc, b"{}"), trap(words));
        assert_eq!(result(&in_check, b"{}"), trap(words));
    }
    // a stack overflow in either call
    let mut g = Guest::new();
    let f = g.func(2, &[], &[CALL, 2]);
    let in_alloc = g
        .clone()
        .alloc(&[], &[vec![CALL, f as u8], i32c(0)].concat());
    let in_check = g.check(&[], &[vec![CALL, f as u8], out(0, 13)].concat());
    assert_eq!(
        result(&in_alloc.compile(), b"{}"),
        trap("call stack exhausted")
    );
    assert_eq!(
        result(&in_check.compile(), b"{}"),
        trap("call stack exhausted")
    );

    // an endless loop in either call uses all of the fuel it was given, and no more
    let spin = [vec![LOOP, EMPTY, BR, 0, END]].concat();
    let in_alloc = Guest::new()
        .alloc(&[], &[spin.clone(), i32c(0)].concat())
        .compile();
    let in_check = Guest::new()
        .check(&[], &[spin, out(0, 13)].concat())
        .compile();
    for fuel in [0, 1, 1000, BLOCK_FUEL] {
        let want = RunOutcome {
            result: Err(RunFailure::OutOfFuel),
            fuel,
        };
        assert_eq!(run_block(&in_alloc, b"{}", fuel), want);
        assert_eq!(run_block(&in_check, b"{}", fuel), want);
    }
    // fuel past the block's cap is the cap
    let want = RunOutcome {
        result: Err(RunFailure::OutOfFuel),
        fuel: BLOCK_FUEL,
    };
    assert_eq!(run_block(&in_check, b"{}", u64::MAX), want);

    // a data segment past the memory's end traps while the instance is made
    let mut g = Guest::new();
    g.data.push((65_530, vec![1; 10]));
    assert_eq!(
        result(&g.compile(), b"{}"),
        trap("out of bounds memory access")
    );
}

#[test]
fn wck5_host_panics_do_not_become_rows() {
    let m = Guest::new().compile();
    let pass = run(&m, b"{}");
    assert_eq!(pass.result, Ok(CheckResult::Pass));
    for at in [TestPanic::Instantiation, TestPanic::Alloc, TestPanic::Check] {
        panic_in_next_run_for_tests(at);
        // a host fault: no result and no trap, so no row
        assert_eq!(
            run(&m, b"{}"),
            RunOutcome {
                result: Err(RunFailure::HostFault),
                fuel: 0
            },
            "{at:?}"
        );
        // the failed store is gone; the next run of the same module starts afresh
        assert_eq!(run(&m, b"{}"), pass, "{at:?}");
    }
}
