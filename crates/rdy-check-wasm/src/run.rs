//! Running a check module on one block, interface 1: a fresh store and instance, then
//! `rdy_alloc(len)`, the input copied to the range it gives, `rdy_check_v1(ptr, len)`, and the
//! output read from the range its result gives. Both calls run in the one store, on the one fuel
//! allowance. The store and instance are dropped after the block; only the compiled module is
//! reused.
//!
//! Every range the guest gives is checked before memory is touched: the two halves of the result
//! are unsigned 32-bit values, the end of a range is a checked sum, and the output's length is
//! checked against [`OUTPUT_BYTES_MAX`] before its range is read. The output is decoded in guest
//! memory, so no buffer of a length the guest chose is made.
use crate::config::{BLOCK_FUEL, INPUT_BYTES_MAX, OUTPUT_BYTES_MAX};
use crate::instance::{self, Fresh, NotMade};
use crate::result::decode_result;
use crate::{CheckResult, CompiledModule, RunFailure, RunOutcome};
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The detail of a range from `rdy_alloc` that lies outside memory.
const ALLOC_OUTSIDE: &str = "rdy_alloc gave a range outside memory";

/// The detail of an output range that lies outside memory.
const OUTPUT_OUTSIDE: &str = "the output range is outside memory";

/// Tests only: a point of a run where [`panic_in_next_run_for_tests`] makes the host panic.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestPanic {
    /// While the store and instance are made.
    Instantiation,
    /// Inside the host's part of the `rdy_alloc` call.
    Alloc,
    /// Inside the host's part of the `rdy_check_v1` call.
    Check,
}

thread_local! {
    static PANIC_IN_NEXT_RUN: Cell<Option<TestPanic>> = const { Cell::new(None) };
}

/// Tests only: the next [`run_block`] on this thread panics at `at`, as a fault inside the
/// interpreter would.
#[doc(hidden)]
pub fn panic_in_next_run_for_tests(at: TestPanic) {
    PANIC_IN_NEXT_RUN.with(|p| p.set(Some(at)));
}

fn injected(at: TestPanic) {
    if PANIC_IN_NEXT_RUN.with(|p| p.get()) == Some(at) {
        PANIC_IN_NEXT_RUN.with(|p| p.set(None));
        panic!("injected check module host fault");
    }
}

/// Runs `module` on one block's `input` with `fuel`, at most [`BLOCK_FUEL`], in a fresh store
/// and instance. The outcome's fuel is what both calls and the instantiation used; a run out of
/// fuel used all of it. An input over [`INPUT_BYTES_MAX`] runs nothing. A panic inside the
/// interpreter is [`RunFailure::HostFault`], and the store it left is dropped; catching it does
/// not catch an allocator abort or the end of the process.
pub fn run_block(module: &CompiledModule, input: &[u8], fuel: u64) -> RunOutcome {
    if input.len() > INPUT_BYTES_MAX {
        return RunOutcome {
            result: Err(RunFailure::InputTooLarge),
            fuel: 0,
        };
    }
    let allowance = fuel.min(BLOCK_FUEL);
    catch_unwind(AssertUnwindSafe(|| {
        run_caught(&module.module, input, allowance)
    }))
    .unwrap_or(RunOutcome {
        result: Err(RunFailure::HostFault),
        fuel: 0,
    })
}

fn run_caught(module: &wasmi::Module, input: &[u8], allowance: u64) -> RunOutcome {
    injected(TestPanic::Instantiation);
    let Fresh {
        mut store,
        instance,
    } = match instance::fresh_instance(module, allowance) {
        Ok(fresh) => fresh,
        Err(NotMade { failure, fuel }) => {
            return RunOutcome {
                result: Err(failure),
                fuel,
            };
        }
    };
    let result = calls(&mut store, &instance, input);
    let fuel = instance::used(&store, allowance, result == Err(RunFailure::OutOfFuel));
    RunOutcome { result, fuel }
}

/// The two calls in `store`, and the result they give.
fn calls(
    store: &mut wasmi::Store<wasmi::StoreLimits>,
    instance: &wasmi::Instance,
    input: &[u8],
) -> Result<CheckResult, RunFailure> {
    // the module was checked for these three exports when it compiled
    let alloc = instance
        .get_typed_func::<i32, i32>(&*store, "rdy_alloc")
        .map_err(|_| RunFailure::HostFault)?;
    let check = instance
        .get_typed_func::<(i32, i32), i64>(&*store, "rdy_check_v1")
        .map_err(|_| RunFailure::HostFault)?;
    let memory = instance
        .get_memory(&*store, "memory")
        .ok_or(RunFailure::HostFault)?;
    let len = u32::try_from(input.len()).map_err(|_| RunFailure::HostFault)?;

    injected(TestPanic::Alloc);
    let at = alloc
        .call(&mut *store, len.cast_signed())
        .map_err(|e| instance::failure(&e))?
        .cast_unsigned();
    let range = within(at, len, memory.data_size(&*store))
        .ok_or_else(|| RunFailure::BadOutput(ALLOC_OUTSIDE.to_string()))?;
    memory.data_mut(&mut *store)[range].copy_from_slice(input);

    injected(TestPanic::Check);
    let packed = check
        .call(&mut *store, (at.cast_signed(), len.cast_signed()))
        .map_err(|e| instance::failure(&e))?
        .cast_unsigned();
    let (out_at, out_len) = ((packed >> 32) as u32, packed as u32);
    if usize::try_from(out_len).map_or(true, |n| n > OUTPUT_BYTES_MAX) {
        return Err(RunFailure::OutputTooLarge);
    }
    let range = within(out_at, out_len, memory.data_size(&*store))
        .ok_or_else(|| RunFailure::BadOutput(OUTPUT_OUTSIDE.to_string()))?;
    decode_result(&memory.data(&*store)[range]).map_err(RunFailure::BadOutput)
}

/// The range of `len` bytes at `at` when it lies inside a memory of `size` bytes. The end is a
/// checked sum in 64 bits, so no pair of 32-bit values wraps.
fn within(at: u32, len: u32, size: usize) -> Option<std::ops::Range<usize>> {
    let end = u64::from(at).checked_add(u64::from(len))?;
    let size = u64::try_from(size).ok()?;
    if end > size {
        return None;
    }
    Some(usize::try_from(at).ok()?..usize::try_from(end).ok()?)
}
