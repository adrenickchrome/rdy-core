//! A fresh store and instance for one judged block. The store's limits are set before the module
//! is instantiated, so its one memory and its one table never grow past the sandbox's caps. A
//! failed growth returns -1 to the guest; it never traps.
use crate::RunFailure;
use crate::config::{MEMORY_BYTES_MAX, TABLE_ELEMENTS_MAX, TABLES_MAX};
use wasmi::{Instance, Module, Store, StoreLimits, StoreLimitsBuilder};

/// A store that holds one instance of a check module, and that instance.
pub(crate) struct Fresh {
    #[expect(dead_code, reason = "a run calls the instance's exports in this store")]
    pub(crate) store: Store<StoreLimits>,
    #[expect(dead_code, reason = "a run calls the instance's exports")]
    pub(crate) instance: Instance,
}

/// A new store with the sandbox's limits and `fuel`, and one instance of `module` in it.
/// Instantiation runs no guest code: a check module has no start function and no import. Err:
/// instantiation trapped, or ran out of fuel.
pub(crate) fn fresh_instance(module: &Module, fuel: u64) -> Result<Fresh, RunFailure> {
    let limits = StoreLimitsBuilder::new()
        .memory_size(MEMORY_BYTES_MAX)
        .table_elements(TABLE_ELEMENTS_MAX as usize)
        .instances(1)
        .tables(TABLES_MAX as usize)
        .memories(1)
        .trap_on_grow_failure(false)
        .build();
    let mut store = Store::new(module.engine(), limits);
    store.limiter(|limits| limits);
    store.set_fuel(fuel).map_err(|_| RunFailure::HostFault)?;
    let instance = Instance::new(&mut store, module, &[]).map_err(|e| match e.as_trap_code() {
        Some(wasmi::TrapCode::OutOfFuel) => RunFailure::OutOfFuel,
        Some(code) => RunFailure::Trap(code.to_string()),
        None => RunFailure::Trap(e.to_string()),
    })?;
    Ok(Fresh { store, instance })
}
