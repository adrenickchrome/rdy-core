//! Sandboxed WebAssembly check modules. This crate is the interpreter boundary: it is the only
//! rdy crate that links the interpreter, and it depends on no rdy crate. Its API takes owned
//! bytes and gives primitive values and typed failures; no interpreter type leaves it.
//!
//! A module is compiled once per loaded pack and run in a fresh store and instance for each
//! judged block. It imports nothing, so a guest has no host function, file, clock or network.
mod compile;
pub mod config;
mod instance;
mod result;
mod run;

pub use compile::compile_module;
pub use result::decode_result;
pub use run::{TestPanic, panic_in_next_run_for_tests, run_block};

/// The interface number of the exports a check module gives.
pub const INTERFACE: u32 = 1;

/// A check module that the pinned interpreter validated and translated eagerly. It is opaque:
/// it holds the interpreter's module, which never leaves this crate.
pub struct CompiledModule {
    module: wasmi::Module,
    bytes: usize,
}

impl CompiledModule {
    /// The bytes of the module it was compiled from.
    pub fn module_bytes(&self) -> usize {
        self.bytes
    }

    /// The interface number of its exports: [`INTERFACE`].
    pub fn interface(&self) -> u32 {
        INTERFACE
    }

    /// Tests only: makes the fresh store and instance that a run of one block starts with, with
    /// a block's fuel, and drops them. Runs no export. A panic inside the interpreter is
    /// [`RunFailure::HostFault`].
    #[doc(hidden)]
    pub fn instance_setup_for_tests(&self) -> Result<(), RunFailure> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            instance::fresh_instance(&self.module, config::BLOCK_FUEL)
                .map(drop)
                .map_err(|n| n.failure)
        }))
        .unwrap_or(Err(RunFailure::HostFault))
    }
}

/// Why a module's bytes did not compile into a [`CompiledModule`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileFailure {
    /// The interpreter refused the bytes: its validation error, with no host path in it.
    Invalid(String),
    /// The module imports `module`.`name`; a check module imports nothing.
    Import { module: String, name: String },
    /// The module lacks the export `name` of type `ty`.
    MissingExport {
        name: &'static str,
        ty: &'static str,
    },
    /// The module has a start function.
    StartFunction,
    /// The module's memory starts above [`config::MEMORY_PAGES_MAX`] pages.
    MemoryMinimum,
    /// The module declares more than [`config::TABLES_MAX`] tables.
    Tables,
    /// A table may hold more than [`config::TABLE_ELEMENTS_MAX`] elements.
    TableElements,
    /// The interpreter panicked while compiling: a fault of rdy, not of the module.
    HostFault,
}

/// A check module's answer for one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckResult {
    /// The block passes; a pass carries no message or fix.
    Pass,
    /// The block fails, with the module's message and fix when it gave them.
    Fail {
        message: Option<String>,
        fix: Option<String>,
    },
}

/// Why a run gave no [`CheckResult`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunFailure {
    /// The module trapped: the trap, in deterministic words.
    Trap(String),
    /// The run used all of the fuel it was given.
    OutOfFuel,
    /// The input envelope is over [`config::INPUT_BYTES_MAX`] bytes.
    InputTooLarge,
    /// The output is over [`config::OUTPUT_BYTES_MAX`] bytes.
    OutputTooLarge,
    /// The output is not a check result: why.
    BadOutput(String),
    /// The interpreter panicked: a fault of rdy, never a row.
    HostFault,
}

/// One run's result and the fuel it consumed, both exported calls included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutcome {
    pub result: Result<CheckResult, RunFailure>,
    pub fuel: u64,
}
