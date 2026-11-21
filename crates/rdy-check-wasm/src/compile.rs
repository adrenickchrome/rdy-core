//! Compiling a check module: the interpreter validates and translates every function body at
//! once, then this file inspects the module's imports, exports, memory and tables, all without
//! instantiating it, so no guest code runs at load.
//!
//! The checks run in this order, and the first that fails is the refusal: the interpreter's
//! validation (a start function included), then imports, then the three exports in the order
//! `memory`, `rdy_alloc`, `rdy_check_v1`, then the memory's minimum, then the table count and the
//! tables' declared sizes.
use crate::config::{self, MEMORY_PAGES_MAX, TABLE_ELEMENTS_MAX, TABLES_MAX};
use crate::{CompileFailure, CompiledModule};
use std::panic::{AssertUnwindSafe, catch_unwind};
use wasmi::{ExternType, ValType};

/// The words the pinned interpreter gives a start function that the configuration refuses.
const START_REFUSED: &str = "configuration disallows start functions but found one";

/// The export a check module needs: its name, its type as a refusal names it, and the kind and
/// signature that match it.
struct Needed {
    name: &'static str,
    ty: &'static str,
    matches: fn(&ExternType) -> bool,
}

/// The three exports of interface 1, in the order they are checked.
const NEEDED: [Needed; 3] = [
    Needed {
        name: "memory",
        ty: "memory",
        matches: |t| matches!(t, ExternType::Memory(_)),
    },
    Needed {
        name: "rdy_alloc",
        ty: "(i32) -> i32",
        matches: |t| {
            matches!(t, ExternType::Func(f)
                if f.params() == [ValType::I32] && f.results() == [ValType::I32])
        },
    },
    Needed {
        name: "rdy_check_v1",
        ty: "(i32, i32) -> i64",
        matches: |t| {
            matches!(t, ExternType::Func(f)
                if f.params() == [ValType::I32, ValType::I32] && f.results() == [ValType::I64])
        },
    },
];

/// Compiles `bytes` into a check module. Every function body is validated and translated now,
/// a used one or not. Err: the first refusal, in this file's order. A panic inside the
/// interpreter is [`CompileFailure::HostFault`]; catching it does not catch an allocator abort or
/// the end of the process.
pub fn compile_module(bytes: &[u8]) -> Result<CompiledModule, CompileFailure> {
    catch_unwind(AssertUnwindSafe(|| compile_caught(bytes)))
        .unwrap_or(Err(CompileFailure::HostFault))
}

fn compile_caught(bytes: &[u8]) -> Result<CompiledModule, CompileFailure> {
    let engine = wasmi::Engine::new(&config::engine_config());
    let module = wasmi::Module::new(&engine, bytes).map_err(|e| {
        let text = e.to_string();
        if matches!(e.kind(), wasmi::errors::ErrorKind::Translation(_)) && text == START_REFUSED {
            CompileFailure::StartFunction
        } else {
            CompileFailure::Invalid(text)
        }
    })?;
    if let Some(import) = module.imports().next() {
        return Err(CompileFailure::Import {
            module: import.module().to_string(),
            name: import.name().to_string(),
        });
    }
    for needed in &NEEDED {
        if !module
            .get_export(needed.name)
            .is_some_and(|t| (needed.matches)(&t))
        {
            return Err(CompileFailure::MissingExport {
                name: needed.name,
                ty: needed.ty,
            });
        }
    }
    // multi-memory is off, so the exported memory is the module's one memory
    if let Some(ExternType::Memory(memory)) = module.get_export("memory")
        && memory.minimum() > u64::from(MEMORY_PAGES_MAX)
    {
        return Err(CompileFailure::MemoryMinimum);
    }
    check_tables(bytes)?;
    Ok(CompiledModule {
        module,
        bytes: bytes.len(),
    })
}

/// The table count and each table's declared minimum and maximum, read from the table section of
/// `bytes`, which the interpreter has validated. An import is refused before this, so the section
/// holds every table. A section this reader cannot follow is a fault of rdy, never the module's.
fn check_tables(bytes: &[u8]) -> Result<(), CompileFailure> {
    let Some(tables) = table_section(bytes).ok_or(CompileFailure::HostFault)? else {
        return Ok(());
    };
    let mut r = Reader {
        bytes: tables,
        at: 0,
    };
    let count = r.u32().ok_or(CompileFailure::HostFault)?;
    if count > TABLES_MAX {
        return Err(CompileFailure::Tables);
    }
    for _ in 0..count {
        // the element type: one byte with this configuration (funcref or externref)
        r.byte().ok_or(CompileFailure::HostFault)?;
        let flags = r.byte().ok_or(CompileFailure::HostFault)?;
        let minimum = r.u32().ok_or(CompileFailure::HostFault)?;
        let maximum = if flags & 1 == 1 {
            Some(r.u32().ok_or(CompileFailure::HostFault)?)
        } else {
            None
        };
        if minimum > TABLE_ELEMENTS_MAX || maximum.is_some_and(|m| m > TABLE_ELEMENTS_MAX) {
            return Err(CompileFailure::TableElements);
        }
    }
    Ok(())
}

/// The body of the table section (id 4) of the module `bytes`: `Some(None)` when it has none,
/// `None` when the sections cannot be followed. Reads section headers only, and allocates
/// nothing.
fn table_section(bytes: &[u8]) -> Option<Option<&[u8]>> {
    let mut r = Reader { bytes, at: 8 };
    while r.at < bytes.len() {
        let id = r.byte()?;
        let len = usize::try_from(r.u32()?).ok()?;
        let end = r.at.checked_add(len).filter(|e| *e <= bytes.len())?;
        if id == 4 {
            return Some(Some(&bytes[r.at..end]));
        }
        r.at = end;
    }
    Some(None)
}

/// A cursor over validated module bytes; every read is bounds-checked.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    /// An unsigned LEB128 value of at most five bytes.
    fn u32(&mut self) -> Option<u32> {
        let mut value: u32 = 0;
        for shift in [0u32, 7, 14, 21, 28] {
            let b = self.byte()?;
            value |= u32::from(b & 0x7f).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}
