//! Folders and docs: the engine types that form the project tree, and the rules for their names.
use crate::BlockId;
use std::collections::BTreeSet;

pub const DOC: &str = "doc";
pub const FOLDER: &str = "folder";
pub const NAME_MAX: usize = 64;
/// How deep folders may nest: the number of folders on a block's chain, a top-level folder
/// being at depth 1. Keeps tree walks shallow and exported paths short.
pub const MAX_FOLDER_DEPTH: usize = 32;

/// Docs and folders have names and paths; every other block lives inside a doc.
pub fn is_tree_type(block_type: &str) -> bool {
    block_type == DOC || block_type == FOLDER
}

/// Why `name` is not a valid folder or doc name, or None if it is.
pub fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() || name.len() > NAME_MAX {
        return Some(format!("a name has 1 to {NAME_MAX} characters"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Some("a name uses only A-Z, a-z, 0-9, '.', '_' and '-'".into());
    }
    if name.starts_with('.') || name.starts_with('_') {
        return Some("names starting with '.' or '_' are reserved for rdy's own files".into());
    }
    if name.ends_with('.') {
        return Some("a name cannot end in '.' (Windows drops it from directory names)".into());
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        return Some("a name cannot end in .yaml or .yml".into());
    }
    if lower.ends_with(".theirs") {
        return Some(
            "a name cannot end in .theirs (a working copy reads <name>.theirs.yaml as a conflict file)"
                .into(),
        );
    }
    if lower == "index" {
        return Some("index is reserved for folder pages".into());
    }
    if BlockId::parse(name).is_ok() {
        return Some("a name cannot look like a block id (it would be read as one)".into());
    }
    let stem = lower.split('.').next().unwrap_or("");
    let numbered = stem.len() == 4
        && (stem.starts_with("com") || stem.starts_with("lpt"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9');
    if matches!(stem, "con" | "prn" | "aux" | "nul") || numbered {
        return Some(format!("{stem} is a Windows device name"));
    }
    None
}

/// The name rdy assigns from a title (see the spec's §2.2a).
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let edge = |c: char| matches!(c, '-' | '.');
    let cut: String = out.trim_matches(edge).chars().take(NAME_MAX).collect();
    cut.trim_end_matches(edge).to_string()
}

/// The name a new doc or folder gets when none is given.
pub fn default_name(title: Option<&str>, id: &BlockId) -> String {
    let s = slug(title.unwrap_or(""));
    if !s.is_empty() && name_problem(&s).is_none() {
        return s;
    }
    let body = id.as_str().split_once('-').map(|(_, b)| b).unwrap_or("");
    format!("untitled-{}", body.to_ascii_lowercase())
}

/// `base`, or `base-2`, `base-3`, … — the first that is not in `taken` (lower-cased names).
pub fn dedupe(base: &str, taken: &BTreeSet<String>) -> String {
    dedupe_with(base, |lower| taken.contains(lower))
}

/// As [`dedupe`], with `taken` answering for one lower-cased name.
pub fn dedupe_with(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(&base.to_ascii_lowercase()) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let stem: String = base.chars().take(NAME_MAX - suffix.len()).collect();
            format!("{stem}{suffix}")
        })
        .find(|c| !taken(&c.to_ascii_lowercase()))
        .expect("a free suffix exists")
}
