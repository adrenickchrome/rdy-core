//! A fresh store and instance for one judged block. The store's limits are set before the module
//! is instantiated, so its one memory and its one table never grow past the sandbox's caps. A
//! failed growth returns -1 to the guest; it never traps.
use crate::RunFailure;
use crate::config::{MEMORY_BYTES_MAX, TABLE_ELEMENTS_MAX, TABLES_MAX};
use wasmi::{Instance, Module, Store, StoreLimits, StoreLimitsBuilder, TrapCode};

/// A store that holds one instance of a check module, and that instance.
pub(crate) struct Fresh {
    pub(crate) store: Store<StoreLimits>,
    pub(crate) instance: Instance,
}

/// Why a fresh instance was not made, and the fuel its instantiation used.
pub(crate) struct NotMade {
    pub(crate) failure: RunFailure,
    pub(crate) fuel: u64,
}

/// A new store with the sandbox's limits and `fuel`, and one instance of `module` in it.
/// Instantiation runs no guest code: a check module has no start function and no import. Err:
/// instantiation failed ([`failure`]), with the fuel it used.
pub(crate) fn fresh_instance(module: &Module, fuel: u64) -> Result<Fresh, NotMade> {
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
    store.set_fuel(fuel).map_err(|_| NotMade {
        failure: RunFailure::HostFault,
        fuel: 0,
    })?;
    match Instance::new(&mut store, module, &[]) {
        Ok(instance) => Ok(Fresh { store, instance }),
        Err(e) => {
            let failure = failure(&e);
            Err(NotMade {
                fuel: used(&store, fuel, failure == RunFailure::OutOfFuel),
                failure,
            })
        }
    }
}

/// The failure an interpreter error stands for. Trap codes come first: running out of fuel is
/// [`RunFailure::OutOfFuel`], a host that could not supply memory is a fault of rdy, and every
/// other code is a trap, in the specification's words. An error with no trap code is a fault of
/// rdy too: a module that loaded imports nothing, so no other error comes from the guest.
pub(crate) fn failure(e: &wasmi::Error) -> RunFailure {
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => RunFailure::OutOfFuel,
        Some(TrapCode::OutOfSystemMemory) | None => RunFailure::HostFault,
        Some(code) => RunFailure::Trap(code.trap_message().to_string()),
    }
}

/// The fuel that a run in `store`, started with `allowance`, used. A run `out_of_fuel` used all
/// of its allowance, whatever the store has left.
pub(crate) fn used(store: &Store<StoreLimits>, allowance: u64, out_of_fuel: bool) -> u64 {
    if out_of_fuel {
        return allowance;
    }
    store
        .get_fuel()
        .map_or(allowance, |left| allowance.saturating_sub(left))
}
