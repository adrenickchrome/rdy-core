//! Compiles the agent guide (D-GEKKY): checks `release/agent-guide.json` against its digest, then
//! writes a static catalogue and the exact bundle bytes to `OUT_DIR`. A bad input stops the build;
//! there is no empty-guide fallback. Only after that check does it check the kungfu tool table and
//! values (D-Y18E1) and write the table's static rows.
#[allow(dead_code)]
#[path = "build/guide.rs"]
mod guide;
#[allow(dead_code)]
#[path = "build/kungfu.rs"]
mod kungfu;

use std::path::{Path, PathBuf};

fn stop(message: impl std::fmt::Display) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn main() {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let Some(root) = manifest.as_deref().and_then(|m| m.parent()?.parent()) else {
        stop("embedded guide: cannot find the workspace root");
    };
    for path in [
        root.join(guide::BUNDLE_PATH),
        root.join(guide::SIDECAR_PATH),
        root.join(kungfu::TABLE_PATH),
        Path::new("build.rs").to_path_buf(),
        Path::new("build").join("guide.rs"),
        Path::new("build").join("kungfu.rs"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let catalogue = guide::load(root).unwrap_or_else(|e| stop(e));
    let table = kungfu::load(root).unwrap_or_else(|e| stop(e));
    let sets: Vec<(&str, Vec<&str>)> = catalogue
        .rulesets
        .iter()
        .filter(|set| kungfu::OWNERS.contains(&set.name.as_str()))
        .map(|set| {
            let texts = set.rows.iter().map(|r| r.text.as_str()).collect();
            (set.name.as_str(), texts)
        })
        .collect();
    kungfu::check_values(&sets).unwrap_or_else(|e| stop(e));
    let Some(out) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        stop("embedded guide: cannot find OUT_DIR");
    };
    let write = |name: &str, bytes: &[u8]| {
        if let Err(e) = std::fs::write(out.join(name), bytes) {
            stop(format!("embedded guide: cannot write {name}: {e}"));
        }
    };
    write("agent-guide.json", &catalogue.bytes);
    write("embedded_guide.rs", guide::generate(&catalogue).as_bytes());
    write("kungfu_table.rs", kungfu::generate(&table).as_bytes());
}
