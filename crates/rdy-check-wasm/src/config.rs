//! The interpreter's one configuration and the fixed sandbox limits. Every value is a constant:
//! there is no configuration knob. The stack values are bytes, as the pinned interpreter's API
//! takes them.

/// The deepest call nesting a module may reach.
pub const MAX_RECURSION_DEPTH: usize = 1024;

/// The largest value stack, in bytes.
pub const MAX_STACK_HEIGHT: usize = 524_288;

/// The value stack a run starts with, in bytes.
pub const MIN_STACK_HEIGHT: usize = 4096;

/// The execution stacks the engine keeps between runs: none.
pub const MAX_CACHED_STACKS: usize = 0;

/// The bytes of one linear-memory page.
pub const PAGE_BYTES: usize = 65_536;

/// The most pages a module's memory may hold, at load and at run time.
pub const MEMORY_PAGES_MAX: u32 = 256;

/// The largest linear memory, in bytes: [`MEMORY_PAGES_MAX`] pages.
pub const MEMORY_BYTES_MAX: usize = 16_777_216;

/// The most tables a module may declare.
pub const TABLES_MAX: u32 = 1;

/// The most elements a table may hold.
pub const TABLE_ELEMENTS_MAX: u32 = 10_000;

/// The fuel of one block, both exported calls included.
pub const BLOCK_FUEL: u64 = 10_000_000;

/// The fuel of one rule in one change or propose scan.
pub const RULE_FUEL: u64 = 1_000_000_000;

/// The largest input envelope, in UTF-8 bytes.
pub const INPUT_BYTES_MAX: usize = 1_048_576;

/// The largest output, in UTF-8 bytes.
pub const OUTPUT_BYTES_MAX: usize = 65_536;

/// The interpreter's configuration. Each line sets one switch of the pinned version's `Config`;
/// a switch it does not name keeps the pinned default, which `wck1_engine_configuration_is_closed`
/// reads back.
///
/// - Validation and translation are eager for every function body, so a module that loads holds
///   no unchecked code.
/// - Fuel is on, with the pinned default operator costs.
/// - Floats, multi-memory, tail calls, extended constants, custom page sizes, wide arithmetic and
///   saturating float-to-int are off. SIMD, relaxed SIMD and 64-bit memories are off by
///   construction: their crate features, and so their switches, are absent.
/// - Multi-value, reference types, bulk memory, sign extension and mutable globals are on.
/// - A start function is refused at load, before any guest code runs.
/// - The maximum stack height is set before the minimum, so the order never depends on the
///   previous maximum.
pub(crate) fn engine_config() -> wasmi::Config {
    let mut config = wasmi::Config::default();
    config
        .set_max_recursion_depth(MAX_RECURSION_DEPTH)
        .set_max_stack_height(MAX_STACK_HEIGHT)
        .set_min_stack_height(MIN_STACK_HEIGHT)
        .set_max_cached_stacks(MAX_CACHED_STACKS)
        .compilation_mode(wasmi::CompilationMode::Eager)
        .consume_fuel(true)
        .allow_start_fn(false)
        .floats(false)
        .wasm_multi_memory(false)
        .wasm_tail_call(false)
        .wasm_extended_const(false)
        .wasm_custom_page_sizes(false)
        .wasm_wide_arithmetic(false)
        .wasm_saturating_float_to_int(false)
        .wasm_multi_value(true)
        .wasm_reference_types(true)
        .wasm_bulk_memory(true)
        .wasm_sign_extension(true)
        .wasm_mutable_global(true);
    config
}

/// The engine's configuration as the pinned interpreter prints it: every switch with its value.
pub fn describe_engine_config() -> String {
    let engine = wasmi::Engine::new(&engine_config());
    format!("{:?}", engine.config())
}
