//! The quarantine files an open leaves behind: torn segment tails and damaged indexes. The names
//! are listed, never opened; only the human's `rdy fsck --clean-quarantine` removes them.
use std::path::Path;

/// The quarantine files an open left behind: torn segment tails (`segments/<name>`) and
/// damaged indexes, sorted. A missing directory contributes nothing. Links and directories that
/// carry a quarantine name are listed too; only the cleanup filters them before it deletes.
pub fn names(root: &Path) -> Vec<String> {
    let store = root.join(".rdy/store");
    let names = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out: Vec<String> = names(&store.join("segments"))
        .into_iter()
        .filter(|n| n.contains(".torn-"))
        .map(|n| format!("segments/{n}"))
        .collect();
    out.extend(
        names(&store)
            .into_iter()
            .filter(|n| n.starts_with("index.redb.damaged-")),
    );
    out.sort();
    out
}
