//! wck1: the interpreter's configuration is closed. Every switch of the pinned interpreter's
//! `Config` has the value this crate chooses, read back from the engine it builds.
use rdy_check_wasm::config;

#[test]
fn wck1_engine_configuration_is_closed() {
    // the fixed sandbox limits
    assert_eq!(config::MAX_RECURSION_DEPTH, 1024);
    assert_eq!(config::MAX_STACK_HEIGHT, 524_288);
    assert_eq!(config::MIN_STACK_HEIGHT, 4096);
    assert_eq!(config::MAX_CACHED_STACKS, 0);
    assert_eq!(config::PAGE_BYTES, 65_536);
    assert_eq!(config::MEMORY_PAGES_MAX, 256);
    assert_eq!(config::MEMORY_BYTES_MAX, 16_777_216);
    assert_eq!(
        config::MEMORY_BYTES_MAX,
        config::MEMORY_PAGES_MAX as usize * config::PAGE_BYTES
    );
    assert_eq!(config::TABLES_MAX, 1);
    assert_eq!(config::TABLE_ELEMENTS_MAX, 10_000);
    assert_eq!(config::BLOCK_FUEL, 10_000_000);
    assert_eq!(config::RULE_FUEL, 1_000_000_000);
    assert_eq!(config::INPUT_BYTES_MAX, 1_048_576);
    assert_eq!(config::OUTPUT_BYTES_MAX, 65_536);

    // Every switch, as the pinned interpreter prints the engine's configuration. A switch that
    // moves, or a field a new interpreter version adds, fails here.
    let want = concat!(
        "Config { ",
        // the stack: bytes for the heights, the maximum set first; no cached stacks
        "stack: StackConfig { max_recursion_depth: 1024, min_stack_height: 4096, ",
        "max_stack_height: 524288, max_cached_stacks: 0 }, ",
        // exactly the five proposals; reference types brings the GC type flag it needs. Floats,
        // multi-memory, tail calls, extended constants, custom page sizes, wide arithmetic,
        // saturating float-to-int, SIMD, relaxed SIMD, 64-bit memories and threads are absent.
        "features: WasmFeatures(MUTABLE_GLOBAL | SIGN_EXTENSION | REFERENCE_TYPES | ",
        "MULTI_VALUE | BULK_MEMORY | GC_TYPES), ",
        // fuel on, custom sections parsed, a start function refused at load
        "consume_fuel: true, ignore_custom_sections: false, allow_start_fn: false, ",
        // the pinned default fuel and operator costs
        "fuel_costs: FuelCostsProvider { bytes_copied_per_fuel: 64, ",
        "fuel_per_bytes_translated: 7, fuel_per_bytes_validated: 2 }, ",
        "operator_cost: Default, ",
        // every function body validated and translated at load
        "compilation_mode: Eager, ",
        // no enforced parse limits: this crate checks its own bounds
        "limits: EnforcedLimits { max_globals: None, max_functions: None, max_tables: None, ",
        "max_element_segments: None, max_memories: None, max_data_segments: None, ",
        "max_params: None, max_results: None, min_avg_bytes_per_function: None } }",
    );
    assert_eq!(config::describe_engine_config(), want);
}
