//! Sets RDY_BUILD to the first 16 hex digits of the BLAKE3 hash of the workspace sources (D-CCC6E).
mod build_support;

use std::path::PathBuf;

fn main() {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let Some(root) = manifest.as_deref().and_then(|m| m.parent()?.parent()) else {
        eprintln!("cannot compute RDY_BUILD: cannot read input: crates");
        std::process::exit(1);
    };
    match build_support::source_build_inputs(root, &build_support::LIMITS, &mut |_, _| {}) {
        Ok(out) => {
            for p in &out.rerun {
                println!("cargo:rerun-if-changed={}", p.display());
            }
            println!("cargo:rustc-env=RDY_BUILD={}", out.id);
        }
        Err(e) => {
            eprintln!("cannot compute RDY_BUILD: {e}");
            std::process::exit(1);
        }
    }
}
