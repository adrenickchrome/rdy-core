//! wck3: a check module compiles once, at load, with every function body validated, and is
//! refused for what it imports, exports, starts and declares, before any guest code runs. The
//! hostile probes measure what compilation and one block's instance setup allocate: fuel counts
//! neither, so each must stay within the stated source, memory and table caps.
use rdy_check_wasm::{CompileFailure, compile_module};
use wasm::*;

/// A small encoder of WebAssembly binaries: the tests build every module from these parts.
mod wasm {
    pub const I32: u8 = 0x7f;
    pub const I64: u8 = 0x7e;
    pub const F32: u8 = 0x7d;
    pub const V128: u8 = 0x7b;
    pub const FUNCREF: u8 = 0x70;
    pub const END: u8 = 0x0b;

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

    pub fn name(s: &str) -> Vec<u8> {
        [leb(s.len() as u64), s.as_bytes().to_vec()].concat()
    }

    /// A vector of `items`, each already encoded.
    pub fn vec_of(items: &[Vec<u8>]) -> Vec<u8> {
        [leb(items.len() as u64), items.concat()].concat()
    }

    pub fn section(id: u8, body: &[u8]) -> Vec<u8> {
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

    /// An export of `kind` (0 function, 1 table, 2 memory, 3 global) at `index`.
    pub fn export(field: &str, kind: u8, index: u32) -> Vec<u8> {
        [name(field), vec![kind], leb(u64::from(index))].concat()
    }

    /// A check module as sections: the defaults give a valid one, with type 0 `(i32) -> i32`,
    /// type 1 `(i32, i32) -> i64`, `rdy_alloc` (function 0), `rdy_check_v1` (function 1) and a
    /// one-page memory, all exported.
    #[derive(Clone)]
    pub struct Check {
        pub types: Vec<Vec<u8>>,
        pub imports: Vec<Vec<u8>>,
        /// Each function's type index.
        pub funcs: Vec<u32>,
        pub tables: Vec<Vec<u8>>,
        pub memories: Vec<Vec<u8>>,
        pub globals: Vec<Vec<u8>>,
        pub exports: Vec<Vec<u8>>,
        pub start: Option<u32>,
        pub elements: Vec<Vec<u8>>,
        pub data_count: Option<u32>,
        pub code: Vec<Vec<u8>>,
        pub data: Vec<Vec<u8>>,
    }

    impl Check {
        pub fn new() -> Check {
            Check {
                types: vec![func_type(&[I32], &[I32]), func_type(&[I32, I32], &[I64])],
                imports: vec![],
                funcs: vec![0, 1],
                tables: vec![],
                memories: vec![limits(1, None)],
                globals: vec![],
                exports: vec![
                    export("memory", 2, 0),
                    export("rdy_alloc", 0, 0),
                    export("rdy_check_v1", 0, 1),
                ],
                start: None,
                elements: vec![],
                data_count: None,
                code: vec![body(&[], &[0x41, 0x00]), body(&[], &[0x42, 0x00])],
                data: vec![],
            }
        }

        /// The module with one more function of type `ty` and body `code`; its index.
        pub fn func(&mut self, ty: u32, locals: &[(u64, u8)], code: &[u8]) -> u32 {
            self.funcs.push(ty);
            self.code.push(body(locals, code));
            (self.funcs.len() - 1) as u32
        }

        pub fn bytes(&self) -> Vec<u8> {
            let mut out = b"\0asm\x01\0\0\0".to_vec();
            let mut put = |id: u8, items: &[Vec<u8>]| {
                if !items.is_empty() {
                    out.extend(section(id, &vec_of(items)));
                }
            };
            put(1, &self.types);
            put(2, &self.imports);
            let funcs: Vec<Vec<u8>> = self.funcs.iter().map(|t| leb(u64::from(*t))).collect();
            put(3, &funcs);
            put(4, &self.tables);
            put(5, &self.memories);
            put(6, &self.globals);
            put(7, &self.exports);
            if let Some(f) = self.start {
                out.extend(section(8, &leb(u64::from(f))));
            }
            let mut put = |id: u8, items: &[Vec<u8>]| {
                if !items.is_empty() {
                    out.extend(section(id, &vec_of(items)));
                }
            };
            put(9, &self.elements);
            if let Some(n) = self.data_count {
                out.extend(section(12, &leb(u64::from(n))));
            }
            let mut put = |id: u8, items: &[Vec<u8>]| {
                if !items.is_empty() {
                    out.extend(section(id, &vec_of(items)));
                }
            };
            put(10, &self.code);
            put(11, &self.data);
            out
        }
    }
}

fn compiles(c: &Check) {
    if let Err(e) = compile_module(&c.bytes()) {
        panic!("refused: {e:?}");
    }
}

fn refusal(c: &Check) -> CompileFailure {
    match compile_module(&c.bytes()) {
        Ok(_) => panic!("the module compiled"),
        Err(e) => e,
    }
}

fn invalid(c: &Check) -> String {
    match refusal(c) {
        CompileFailure::Invalid(text) => text,
        other => panic!("not an engine refusal: {other:?}"),
    }
}

#[test]
fn wck3_invalid_unused_body_and_disabled_features_refuse_at_load() {
    // the valid default compiles, and its metadata is the decoded length and interface 1
    let ok = Check::new().bytes();
    let m = compile_module(&ok).unwrap();
    assert_eq!(m.module_bytes(), ok.len());
    assert_eq!(m.interface(), 1);

    // not a module at all
    let text = match compile_module(b"not wasm") {
        Err(CompileFailure::Invalid(t)) => t,
        other => panic!("{:?}", other.err()),
    };
    assert!(!text.is_empty());

    // a function nothing calls or exports, whose body adds with nothing on the stack: eager
    // compilation validates it at load
    let mut c = Check::new();
    c.func(0, &[], &[0x6a]);
    let text = invalid(&c);
    assert!(!text.contains('/') && !text.contains('\\'), "{text}");
    // the same refusal every time, word for word
    assert_eq!(invalid(&c), text);

    // each proposal switched off, one module each
    let disabled: Vec<(&str, Check)> = vec![
        ("floats: an f32 parameter", {
            let mut c = Check::new();
            c.types.push(func_type(&[F32], &[]));
            c.func(2, &[], &[]);
            c
        }),
        ("floats: f32.const", {
            let mut c = Check::new();
            c.func(0, &[], &[0x43, 0, 0, 0, 0, 0x1a, 0x41, 0]);
            c
        }),
        ("multi-memory: two memories", {
            let mut c = Check::new();
            c.memories.push(limits(1, None));
            c
        }),
        ("tail calls: return_call", {
            let mut c = Check::new();
            c.func(0, &[], &[0x20, 0x00, 0x12, 0x00]);
            c
        }),
        ("extended constants: i32.add in a global's init", {
            let mut c = Check::new();
            c.globals
                .push(vec![I32, 0x00, 0x41, 0x01, 0x41, 0x02, 0x6a, END]);
            c
        }),
        ("custom page sizes: a one-byte page", {
            let mut c = Check::new();
            c.memories = vec![vec![0x08, 0x01, 0x00]];
            c
        }),
        ("wide arithmetic: i64.add128", {
            let mut c = Check::new();
            c.types.push(func_type(&[], &[I64, I64]));
            c.func(2, &[], &[0x42, 0, 0x42, 0, 0x42, 0, 0x42, 0, 0xfc, 0x13]);
            c
        }),
        ("saturating float-to-int: i32.trunc_sat_f32_s", {
            let mut c = Check::new();
            c.func(0, &[], &[0x00, 0xfc, 0x00]);
            c
        }),
        ("SIMD: a v128 local", {
            let mut c = Check::new();
            c.func(0, &[(1, V128)], &[0x41, 0]);
            c
        }),
        ("SIMD: v128.const", {
            let mut c = Check::new();
            let mut code = vec![0xfd, 0x0c];
            code.extend([0u8; 16]);
            code.extend([0x1a, 0x41, 0]);
            c.func(0, &[], &code);
            c
        }),
        ("relaxed SIMD: i8x16.relaxed_swizzle", {
            let mut c = Check::new();
            c.func(0, &[], &[0x00, 0xfd, 0x80, 0x02, 0x1a, 0x41, 0]);
            c
        }),
        ("64-bit memories: an i64 memory", {
            let mut c = Check::new();
            c.memories = vec![vec![0x04, 0x01]];
            c
        }),
        ("threads: a shared memory", {
            let mut c = Check::new();
            c.memories = vec![vec![0x03, 0x01, 0x01]];
            c
        }),
        ("threads: an atomic fence", {
            let mut c = Check::new();
            c.func(0, &[], &[0xfe, 0x03, 0x00, 0x41, 0]);
            c
        }),
    ];
    for (what, c) in &disabled {
        let text = invalid(c);
        assert!(!text.is_empty(), "{what}");
        println!("wck3 disabled: {what}: {text}");
    }
    // a tag section after the memory section: the exceptions proposal is off
    let base = Check::new().bytes();
    let mut tagged = base[..8].to_vec();
    let mut rest = &base[8..];
    while !rest.is_empty() {
        let id = rest[0];
        let mut len = 0usize;
        let mut at = 1;
        let mut shift = 0;
        loop {
            let b = rest[at];
            len |= usize::from(b & 0x7f) << shift;
            at += 1;
            shift += 7;
            if b & 0x80 == 0 {
                break;
            }
        }
        tagged.extend(&rest[..at + len]);
        if id == 5 {
            tagged.extend(section(13, &vec_of(&[vec![0x00, 0x00]])));
        }
        rest = &rest[at + len..];
    }
    assert!(matches!(
        compile_module(&tagged),
        Err(CompileFailure::Invalid(_))
    ));

    // the five proposals that are on load
    let enabled: Vec<(&str, Check)> = vec![
        ("multi-value: two results", {
            let mut c = Check::new();
            c.types.push(func_type(&[], &[I32, I32]));
            c.func(2, &[], &[0x41, 0, 0x41, 0]);
            c
        }),
        ("reference types: ref.null and ref.is_null", {
            let mut c = Check::new();
            c.func(0, &[], &[0xd0, FUNCREF, 0xd1]);
            c
        }),
        ("bulk memory: memory.copy", {
            let mut c = Check::new();
            c.func(
                0,
                &[],
                &[0x41, 0, 0x41, 0, 0x41, 0, 0xfc, 0x0a, 0x00, 0x00, 0x41, 0],
            );
            c
        }),
        ("sign extension: i32.extend8_s", {
            let mut c = Check::new();
            c.func(0, &[], &[0x20, 0x00, 0xc0]);
            c
        }),
        ("mutable globals: an exported mutable global", {
            let mut c = Check::new();
            c.globals.push(vec![I32, 0x01, 0x41, 0x00, END]);
            c.exports.push(export("g", 3, 0));
            c
        }),
    ];
    for (what, c) in &enabled {
        assert!(compile_module(&c.bytes()).is_ok(), "{what}");
    }
}

#[test]
fn wck3_exports_imports_start_and_memory_details() {
    // each import kind is refused, with its module and name
    let imports = [
        ("func", vec![0x00, 0x00]),
        ("table", [vec![0x01, FUNCREF], limits(1, None)].concat()),
        ("memory", [vec![0x02], limits(1, None)].concat()),
        ("global", vec![0x03, I32, 0x00]),
    ];
    for (kind, desc) in imports {
        let mut c = Check::new();
        c.imports.push([name("env"), name(kind), desc].concat());
        if kind == "memory" {
            // the imported memory is the one memory
            c.memories.clear();
        }
        assert_eq!(
            refusal(&c),
            CompileFailure::Import {
                module: "env".into(),
                name: kind.into()
            },
            "{kind}"
        );
    }

    // a missing export, in the order memory, rdy_alloc, rdy_check_v1
    let lacks = |name: &'static str, ty: &'static str| CompileFailure::MissingExport { name, ty };
    let mut c = Check::new();
    c.exports.clear();
    assert_eq!(refusal(&c), lacks("memory", "memory"));
    let mut c = Check::new();
    c.memories.clear();
    c.exports.remove(0);
    assert_eq!(refusal(&c), lacks("memory", "memory"));
    let mut c = Check::new();
    c.exports.remove(1);
    assert_eq!(refusal(&c), lacks("rdy_alloc", "(i32) -> i32"));
    let mut c = Check::new();
    c.exports.remove(2);
    assert_eq!(refusal(&c), lacks("rdy_check_v1", "(i32, i32) -> i64"));

    // a wrong kind: memory as a function, rdy_alloc as a global, rdy_check_v1 as the memory
    let mut c = Check::new();
    c.exports[0] = export("memory", 0, 0);
    c.exports.push(export("mem", 2, 0));
    assert_eq!(refusal(&c), lacks("memory", "memory"));
    let mut c = Check::new();
    c.globals.push(vec![I32, 0x00, 0x41, 0x00, END]);
    c.exports[1] = export("rdy_alloc", 3, 0);
    assert_eq!(refusal(&c), lacks("rdy_alloc", "(i32) -> i32"));
    let mut c = Check::new();
    c.exports[2] = export("rdy_check_v1", 2, 0);
    assert_eq!(refusal(&c), lacks("rdy_check_v1", "(i32, i32) -> i64"));

    // a wrong signature
    let mut c = Check::new();
    c.types[0] = func_type(&[I32], &[I64]);
    c.code[0] = body(&[], &[0x42, 0x00]);
    assert_eq!(refusal(&c), lacks("rdy_alloc", "(i32) -> i32"));
    let mut c = Check::new();
    c.types[1] = func_type(&[I32], &[I64]);
    assert_eq!(refusal(&c), lacks("rdy_check_v1", "(i32, i32) -> i64"));
    let mut c = Check::new();
    c.types[1] = func_type(&[I32, I32], &[I32]);
    c.code[1] = body(&[], &[0x41, 0x00]);
    assert_eq!(refusal(&c), lacks("rdy_check_v1", "(i32, i32) -> i64"));

    // extra exports are allowed: a function, a global and a table
    let mut c = Check::new();
    let f = c.func(0, &[], &[0x20, 0x00]);
    c.exports.push(export("helper", 0, f));
    c.globals.push(vec![I32, 0x00, 0x41, 0x07, END]);
    c.exports.push(export("seven", 3, 0));
    c.tables.push([vec![FUNCREF], limits(1, None)].concat());
    c.exports.push(export("table", 1, 0));
    compiles(&c);

    // a start function, before any guest code runs: it would trap if it ran
    let mut c = Check::new();
    c.types.push(func_type(&[], &[]));
    let f = c.func(2, &[], &[0x00]);
    c.start = Some(f);
    assert_eq!(refusal(&c), CompileFailure::StartFunction);

    // the first refusal wins: an import before a missing export
    let mut c = Check::new();
    c.imports
        .push([name("env"), name("f"), vec![0x00, 0x00]].concat());
    c.exports.clear();
    assert!(matches!(refusal(&c), CompileFailure::Import { .. }));
}

#[test]
fn wck3_memory_minimum_at_and_past_limit() {
    for (min, max, ok) in [
        (0, None, true),
        (256, None, true),
        (256, Some(256), true),
        // a declared maximum above the cap is allowed: growth stops at 16 MiB at run time
        (1, Some(65_536), true),
        (256, Some(65_536), true),
        (257, None, false),
        (257, Some(300), false),
        (65_536, None, false),
    ] {
        let mut c = Check::new();
        c.memories = vec![limits(min, max)];
        match compile_module(&c.bytes()) {
            Ok(_) => assert!(ok, "{min} {max:?} compiled"),
            Err(e) => {
                assert!(!ok, "{min} {max:?}: {e:?}");
                assert_eq!(e, CompileFailure::MemoryMinimum);
            }
        }
    }
}

#[test]
fn wck3_table_count_and_elements_at_and_past_limit() {
    let table = |min, max| [vec![FUNCREF], limits(min, max)].concat();
    for (tables, want) in [
        (vec![], None),
        (vec![table(0, None)], None),
        (vec![table(10_000, None)], None),
        (vec![table(10_000, Some(10_000))], None),
        // an absent maximum is allowed: growth stops at 10000 elements
        (vec![table(1, None)], None),
        (
            vec![table(10_001, None)],
            Some(CompileFailure::TableElements),
        ),
        (
            vec![table(1, Some(10_001))],
            Some(CompileFailure::TableElements),
        ),
        (
            vec![table(1, Some(4_294_967_295))],
            Some(CompileFailure::TableElements),
        ),
        (
            vec![table(1, None), table(1, None)],
            Some(CompileFailure::Tables),
        ),
        // the count is checked before any table's size
        (
            vec![table(10_001, None), table(1, None)],
            Some(CompileFailure::Tables),
        ),
    ] {
        let mut c = Check::new();
        c.tables = tables.clone();
        assert_eq!(
            compile_module(&c.bytes()).err(),
            want,
            "{} tables",
            tables.len()
        );
    }
    // an externref table counts the same
    let mut c = Check::new();
    c.tables = vec![[vec![0x6f], limits(10_001, None)].concat()];
    assert_eq!(refusal(&c), CompileFailure::TableElements);
}

/// Every encoding of a table's element type the interpreter takes is read in full before the
/// limits: the one-byte shorthand and the two-byte nullable forms, at 10000 elements and at
/// 10001, as minimum and as maximum. The forms the configuration refuses are refused by the
/// interpreter, never misread.
#[test]
fn wck3_table_element_type_encodings_at_and_past_limit() {
    let mut wrong = vec![];
    for (what, element, taken) in [
        ("funcref", &[FUNCREF][..], true),
        ("externref", &[0x6f], true),
        ("(ref null func)", &[0x63, FUNCREF], true),
        ("(ref null extern)", &[0x63, 0x6f], true),
        // a non-null reference, and a concrete type, need function references, which are off
        ("(ref func)", &[0x64, FUNCREF], false),
        ("(ref null 0)", &[0x63, 0x00], false),
    ] {
        let table = |limits: Vec<u8>| {
            let mut c = Check::new();
            c.tables = vec![[element.to_vec(), limits].concat()];
            compile_module(&c.bytes()).err()
        };
        if !taken {
            let got = table(limits(1, None));
            if !matches!(got, Some(CompileFailure::Invalid(_))) {
                wrong.push(format!("{what}: {got:?}"));
            }
            continue;
        }
        for (case, limits, want) in [
            ("minimum 10000", limits(10_000, None), None),
            ("maximum 10000", limits(10_000, Some(10_000)), None),
            (
                "minimum 10001",
                limits(10_001, None),
                Some(CompileFailure::TableElements),
            ),
            (
                "maximum 10001",
                limits(1, Some(10_001)),
                Some(CompileFailure::TableElements),
            ),
        ] {
            let got = table(limits);
            if got != want {
                wrong.push(format!("{what} {case}: {got:?}, not {want:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    // a table with an initializer expression needs function references, which are off
    for init in [
        [
            &[0x40, 0x00, FUNCREF][..],
            &limits(1, None),
            &[0xd0, FUNCREF, 0x0b],
        ]
        .concat(),
        [
            &[0x40, 0x00, 0x64, FUNCREF][..],
            &limits(1, None),
            &[0xd2, 0x00, 0x0b],
        ]
        .concat(),
    ] {
        let mut c = Check::new();
        c.tables = vec![init];
        assert!(matches!(refusal(&c), CompileFailure::Invalid(_)));
    }
}

/// The memory a call of this thread holds at its peak, in bytes (the counting allocator).
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    thread_local! {
        static NOW: Cell<usize> = const { Cell::new(0) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged; the counters
    // are this thread's own cells. The default `realloc` and `alloc_zeroed` come back here.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let _ = NOW.try_with(|n| {
                n.set(n.get() + l.size());
                let _ = PEAK.try_with(|p| p.set(p.get().max(n.get())));
            });
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(l) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
            let _ = NOW.try_with(|n| n.set(n.get().saturating_sub(l.size())));
            // SAFETY: `ptr` came from `System` with `l` (every allocation here is System's).
            unsafe { System.dealloc(ptr, l) }
        }
    }

    /// This thread's live allocations now.
    pub fn live() -> usize {
        NOW.with(|n| n.get())
    }

    /// The peak of this thread's live allocations during `f`, above what was live before it.
    pub fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let base = NOW.with(|n| n.get());
        PEAK.with(|p| p.set(base));
        let out = f();
        (out, PEAK.with(|p| p.get()) - base)
    }
}

#[global_allocator]
static COUNTING: counting::Counting = counting::Counting;

/// The decoded module cap.
const MODULE_MAX: usize = 1_048_576;
/// The memory cap, 256 pages.
const MEMORY_MAX: usize = 16_777_216;

/// One hostile probe: its name, its bytes and whether it compiles.
struct Probe {
    what: &'static str,
    bytes: Vec<u8>,
}

/// Modules close to the 1 MiB cap that declare as much of one kind as fits, or declare counts
/// their bytes do not hold.
fn probes() -> Vec<Probe> {
    let mut out = vec![];
    let fit = |c: &Check| c.bytes().len() <= MODULE_MAX;

    // functions: as many one-instruction functions as fit
    let mut c = Check::new();
    c.types.push(func_type(&[], &[]));
    let one = body(&[], &[]);
    while c.bytes().len() + 5 * 4096 < MODULE_MAX {
        for _ in 0..4096 {
            c.funcs.push(2);
            c.code.push(one.clone());
        }
    }
    assert!(fit(&c));
    out.push(Probe {
        what: "functions",
        bytes: c.bytes(),
    });

    // locals: each function declares the most locals the interpreter takes (30000), in one run
    let mut c = Check::new();
    c.types.push(func_type(&[], &[]));
    let wide = body(&[(30_000, I64)], &[]);
    while c.bytes().len() + 2 * wide.len() * 1024 < MODULE_MAX {
        for _ in 0..1024 {
            c.funcs.push(2);
            c.code.push(wide.clone());
        }
    }
    assert!(fit(&c));
    out.push(Probe {
        what: "locals",
        bytes: c.bytes(),
    });

    // globals: as many as fit, each with a constant
    let mut c = Check::new();
    let g = vec![I64, 0x01, 0x42, 0x00, END];
    while c.bytes().len() + g.len() * 8192 < MODULE_MAX {
        for _ in 0..8192 {
            c.globals.push(g.clone());
        }
    }
    assert!(fit(&c));
    out.push(Probe {
        what: "globals",
        bytes: c.bytes(),
    });

    // element segments: one passive segment of as many function references as fit
    let mut c = Check::new();
    let n = (MODULE_MAX - 4096) / 2;
    let items: Vec<Vec<u8>> = (0..n).map(|i| leb((i % 2) as u64)).collect();
    // flags 1: passive, element kind 0 (funcref), function indices
    c.elements.push([vec![0x01, 0x00], vec_of(&items)].concat());
    assert!(fit(&c));
    out.push(Probe {
        what: "a passive element segment",
        bytes: c.bytes(),
    });

    // element segments: as many one-item passive segments as fit
    let mut c = Check::new();
    let seg = vec![0x01, 0x00, 0x01, 0x00];
    while c.bytes().len() + seg.len() * 4096 < MODULE_MAX && c.elements.len() < 90_000 {
        for _ in 0..4096 {
            c.elements.push(seg.clone());
        }
    }
    assert!(fit(&c));
    out.push(Probe {
        what: "element segments",
        bytes: c.bytes(),
    });

    // data segments: one active segment of almost 1 MiB into the 256-page memory
    let mut c = Check::new();
    c.memories = vec![limits(256, None)];
    let len = MODULE_MAX - 4096;
    c.data.push(
        [
            vec![0x00, 0x41, 0x00, END],
            leb(len as u64),
            vec![0xab; len],
        ]
        .concat(),
    );
    assert!(fit(&c));
    out.push(Probe {
        what: "an active data segment",
        bytes: c.bytes(),
    });

    // data segments: as many one-byte passive segments as the validator allows
    let mut c = Check::new();
    let seg = vec![0x01, 0x01, 0x00];
    for _ in 0..100_000 {
        c.data.push(seg.clone());
    }
    c.data_count = Some(100_000);
    assert!(fit(&c));
    out.push(Probe {
        what: "data segments",
        bytes: c.bytes(),
    });

    // nesting: one function of nested blocks, as deep as a function's 128 KiB allows
    let mut c = Check::new();
    let depth = 40_000;
    let mut code = vec![];
    for _ in 0..depth {
        code.extend([0x02, 0x40]);
    }
    code.extend(std::iter::repeat_n(END, depth));
    code.extend([0x41, 0x00]);
    c.func(0, &[], &code);
    assert!(fit(&c));
    out.push(Probe {
        what: "nested blocks",
        bytes: c.bytes(),
    });

    // nesting again: eight such functions, so the translator's buffers are reused, not summed
    let mut c = Check::new();
    for _ in 0..8 {
        c.func(0, &[], &code);
    }
    assert!(fit(&c));
    out.push(Probe {
        what: "eight functions of nested blocks",
        bytes: c.bytes(),
    });

    // locals past the interpreter's 30000 are refused
    let mut c = Check::new();
    c.types.push(func_type(&[], &[]));
    c.func(2, &[(30_001, I64)], &[]);
    out.push(Probe {
        what: "30001 locals",
        bytes: c.bytes(),
    });

    // declared counts the bytes do not hold: each section claims the validator's most
    for (what, id, count) in [
        (
            "a type section that claims 1000000 types",
            1u8,
            1_000_000u64,
        ),
        (
            "an import section that claims 1000000 imports",
            2,
            1_000_000,
        ),
        ("a table section that claims 100 tables", 4, 100),
        ("a memory section that claims 100 memories", 5, 100),
        (
            "a function section that claims 1000000 functions",
            3,
            1_000_000,
        ),
        ("a global section that claims 1000000 globals", 6, 1_000_000),
        (
            "an export section that claims 1000000 exports",
            7,
            1_000_000,
        ),
        ("an element section that claims 100000 segments", 9, 100_000),
        ("a code section that claims 1000000 bodies", 10, 1_000_000),
        ("a data section that claims 100000 segments", 11, 100_000),
    ] {
        let mut bytes = b"\0asm\x01\0\0\0".to_vec();
        bytes.extend(section(id, &leb(count)));
        out.push(Probe { what, bytes });
    }
    // the sum: real imports that fill the module, then the export section's largest claim
    let mut imports = leb(262_000);
    for _ in 0..262_000 {
        // module "", name "", a function of type 0
        imports.extend([0, 0, 0, 0]);
    }
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    bytes.extend(section(1, &vec_of(&[func_type(&[], &[])])));
    bytes.extend(section(2, &imports));
    bytes.extend(section(7, &leb(1_000_000)));
    assert!(bytes.len() <= MODULE_MAX);
    out.push(Probe {
        what: "262000 imports, then an export section that claims 1000000 exports",
        bytes,
    });
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    bytes.extend(section(12, &leb(100_000)));
    bytes.extend(section(11, &leb(100_000)));
    out.push(Probe {
        what: "a data count of 100000 and an empty data section",
        bytes,
    });
    out
}

#[test]
fn wck3_hostile_nesting_segments_and_declared_counts_are_bounded() {
    // The ceilings are this test's, not limits of rdy: each shows the allocation has a fixed
    // bound. The largest compile peaks come from a count a section declares, which the validator
    // caps (1000000 imports of 72 bytes each, 1000000 exports of 64), not from the module's size;
    // the worst is real entries that fill the module and then the last section's largest claim.
    // One block's setup holds at most the 16 MiB memory and what the module's own functions,
    // globals and segments need.
    let compile_ceiling = 128 * MODULE_MAX;
    let setup_ceiling = 80 * MODULE_MAX;
    for p in probes() {
        let live = counting::live();
        let (compiled, compile_peak) = counting::peak_during(|| compile_module(&p.bytes));
        let held = counting::live().saturating_sub(live);
        assert!(
            compile_peak <= compile_ceiling,
            "{}: compiling {} bytes held {compile_peak} bytes",
            p.what,
            p.bytes.len()
        );
        let setup_peak = match &compiled {
            Ok(m) => {
                let (setup, peak) = counting::peak_during(|| m.instance_setup_for_tests());
                assert_eq!(setup, Ok(()), "{}", p.what);
                assert!(
                    peak <= setup_ceiling,
                    "{}: one instance setup held {peak} bytes",
                    p.what
                );
                Some(peak)
            }
            Err(_) => None,
        };
        println!(
            "wck3 probe: {}: {} module bytes, {}, compile peak {compile_peak} bytes, held {held} bytes, setup peak {}",
            p.what,
            p.bytes.len(),
            match &compiled {
                Ok(_) => "compiles".to_string(),
                Err(e) => format!("refused ({e:?})").chars().take(120).collect(),
            },
            setup_peak.map_or("none".to_string(), |n| format!("{n} bytes")),
        );
    }

    // the 256-page memory minimum: one setup allocates the whole memory, and no more than the
    // cap; the next setup starts afresh
    let mut c = Check::new();
    c.memories = vec![limits(256, Some(65_536))];
    let m = compile_module(&c.bytes()).unwrap();
    let (setup, peak) = counting::peak_during(|| m.instance_setup_for_tests());
    assert_eq!(setup, Ok(()));
    assert!(peak >= MEMORY_MAX, "{peak}");
    assert!(peak <= setup_ceiling, "{peak}");
    let (_, again) = counting::peak_during(|| m.instance_setup_for_tests());
    assert!(again <= setup_ceiling, "{again}");
    println!("wck3 probe: a 256-page memory: setup peak {peak} bytes, then {again} bytes");

    // a table at the 10000-element cap
    let mut c = Check::new();
    c.tables = vec![[vec![FUNCREF], limits(10_000, None)].concat()];
    let m = compile_module(&c.bytes()).unwrap();
    let (setup, peak) = counting::peak_during(|| m.instance_setup_for_tests());
    assert_eq!(setup, Ok(()));
    assert!(peak <= setup_ceiling, "{peak}");
    println!("wck3 probe: a 10000-element table: setup peak {peak} bytes");
}
